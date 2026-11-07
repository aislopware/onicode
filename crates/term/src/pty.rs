//! The PTY a host terminal reads, behind the [`Spawner`] seam: the engine spawns login shells on
//! real PTYs ([`NativePty`]), tests hand it pipes, and a viewer that hosts nothing spawns nothing.
use std::io::{Read, Write};
use std::path::PathBuf;

use onicode_protocol::terminal::TerminalSize;

/// The program a new PTY runs and its environment, on top of the engine's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Spec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    /// Variables of the engine's environment the program must not inherit.
    pub unset: Vec<String>,
}

/// A program running on a PTY.
pub struct Spawned {
    pub process: Box<dyn Process>,
    /// What the program writes; ends when the PTY closes.
    pub output: Box<dyn Read + Send>,
    /// What the program reads.
    pub input: Box<dyn Write + Send>,
    /// Blocks until the program exits: its status, or `None` when a signal ended it.
    pub wait: Box<dyn FnOnce() -> Option<i32> + Send>,
}

/// The PTY side of a running program.
pub trait Process: Send {
    /// Tells the program its new size.
    fn resize(&self, size: TerminalSize);
    /// The process group holding the terminal, when it is not the shell's.
    fn foreground(&self) -> Option<i32>;
    /// The hangup a closing terminal sends the shell and its job.
    fn hangup(&self);
}

/// Starts programs on PTYs.
pub trait Spawner: Send + Sync + 'static {
    /// Starts `spec` on a PTY of `size`, or says why it could not.
    ///
    /// # Errors
    /// The PTY could not open or the program could not start.
    fn spawn(&self, spec: &Spec, size: TerminalSize) -> Result<Spawned, String>;
}

/// Hangs up a process group, leaving whatever else holds the terminal.
pub fn hang_up_group(pgrp: i32) {
    if pgrp > 1 {
        // SAFETY: `kill` takes any pid and signal number; a stale group fails harmlessly.
        unsafe {
            libc::kill(-pgrp, libc::SIGHUP);
        }
    }
}

/// Login shells on the platform's PTYs: `openpty`, then the program as a session leader whose
/// controlling terminal is the PTY.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativePty;

impl Spawner for NativePty {
    fn spawn(&self, spec: &Spec, size: TerminalSize) -> Result<Spawned, String> {
        native::spawn(spec, size)
    }
}

mod native {
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    use onicode_protocol::terminal::TerminalSize;

    use super::{Process, Spawned, Spec};

    fn winsize(size: TerminalSize) -> libc::winsize {
        libc::winsize {
            ws_row: size.rows.max(1),
            ws_col: size.cols.max(2),
            ws_xpixel: size.cell_width.saturating_mul(size.cols),
            ws_ypixel: size.cell_height.saturating_mul(size.rows),
        }
    }

    fn failed(what: &str) -> String {
        format!("{what}: {}", io::Error::last_os_error())
    }

    pub(super) fn spawn(spec: &Spec, size: TerminalSize) -> Result<Spawned, String> {
        let (mut master, mut slave) = (-1, -1);
        let mut window = winsize(size);
        // SAFETY: `openpty` writes two descriptors into the out parameters; a null name and
        // termios take the defaults.
        let opened = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut window,
            )
        };
        if opened != 0 {
            return Err(failed("Cannot open a PTY"));
        }
        // SAFETY: `openpty` succeeded, so both descriptors are open and owned by nothing else.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // SAFETY: setting close-on-exec on a descriptor we own: the shell never inherits the
        // master.
        unsafe {
            libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let side = |fd: &OwnedFd| fd.try_clone().map_err(|error| error.to_string());
        let mut command = Command::new(&spec.program);
        command.args(&spec.args).current_dir(&spec.cwd);
        for key in &spec.unset {
            command.env_remove(key);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        command
            .stdin(Stdio::from(side(&slave)?))
            .stdout(Stdio::from(side(&slave)?))
            .stderr(Stdio::from(slave));
        // SAFETY: between fork and exec only async-signal-safe calls run: signal, setsid and
        // ioctl.
        unsafe {
            command.pre_exec(|| {
                for signal in [
                    libc::SIGCHLD,
                    libc::SIGHUP,
                    libc::SIGINT,
                    libc::SIGQUIT,
                    libc::SIGTERM,
                    libc::SIGALRM,
                ] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                // stdin is the PTY's slave: it becomes this new session's controlling terminal.
                #[cfg(target_vendor = "apple")]
                let request = libc::c_ulong::from(libc::TIOCSCTTY);
                #[cfg(not(target_vendor = "apple"))]
                let request = libc::TIOCSCTTY;
                if libc::ioctl(0, request, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("Could not start {}: {error}", spec.program))?;
        // The shell holds the only slave ends now: the reader sees the end when it exits.
        drop(command);
        let output = File::from(side(&master)?);
        let input = File::from(side(&master)?);
        let pid = i32::try_from(child.id()).ok();
        Ok(Spawned {
            process: Box::new(NativeProcess { master, pid }),
            output: Box::new(output),
            input: Box::new(input),
            wait: Box::new(move || child.wait().ok().and_then(|status| status.code())),
        })
    }

    struct NativeProcess {
        master: OwnedFd,
        pid: Option<i32>,
    }

    impl Process for NativeProcess {
        fn resize(&self, size: TerminalSize) {
            let window = winsize(size);
            // SAFETY: TIOCSWINSZ reads one `winsize` from the pointer, on a descriptor we own.
            unsafe {
                libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &raw const window);
            }
        }

        fn foreground(&self) -> Option<i32> {
            // SAFETY: `tcgetpgrp` only reads the terminal's foreground group.
            let pgrp = unsafe { libc::tcgetpgrp(self.master.as_raw_fd()) };
            (pgrp > 0 && Some(pgrp) != self.pid).then_some(pgrp)
        }

        fn hangup(&self) {
            if let Some(pid) = self.pid.filter(|pid| *pid > 1) {
                // SAFETY: `kill` takes any pid and signal number; an exited shell fails harmlessly.
                unsafe {
                    libc::kill(pid, libc::SIGHUP);
                    libc::kill(-pid, libc::SIGHUP);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program on a real PTY: its output arrives, then its exit status. `sh -c` reads no rc file
    /// and prompts for nothing.
    #[test]
    fn a_program_on_a_pty_writes_and_exits() {
        let spec = Spec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'mono%scode\\n' -; exit 3".into()],
            cwd: std::env::temp_dir(),
            ..Spec::default()
        };
        let size = TerminalSize {
            cols: 60,
            rows: 10,
            cell_width: 8,
            cell_height: 16,
        };
        let Spawned {
            process,
            mut output,
            input,
            wait,
        } = NativePty.spawn(&spec, size).expect("the PTY opens");
        process.resize(TerminalSize { cols: 80, ..size });
        let reader = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let mut chunk = [0; 4096];
            // Output ends with a zero read or EIO, depending on the platform.
            while let Ok(read @ 1..) = output.read(&mut chunk) {
                seen.extend_from_slice(&chunk[..read]);
            }
            seen
        });
        assert_eq!(wait(), Some(3));
        drop(input);
        drop(process);
        let seen = String::from_utf8_lossy(&reader.join().expect("the reader ends")).into_owned();
        assert!(seen.contains("mono-code"), "{seen:?}");
    }
}
