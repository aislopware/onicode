//! The PTY a host terminal reads, behind the [`Spawner`] seam: the engine spawns login shells on
//! real PTYs ([`NativePty`]), tests hand it pipes, and a viewer that hosts nothing spawns nothing.
use std::io::{Read, Write};
use std::path::PathBuf;

use onicode_protocol::terminal::TerminalSize;
use portable_pty::{CommandBuilder, MasterPty, PtySize};

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
    #[cfg(unix)]
    if pgrp > 1 {
        // SAFETY: `kill` takes any pid and signal number; a stale group fails harmlessly.
        unsafe {
            libc::kill(-pgrp, libc::SIGHUP);
        }
    }
    #[cfg(not(unix))]
    let _ = pgrp;
}

/// Login shells on the platform's PTYs, through portable-pty.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativePty;

fn pty_size(size: TerminalSize) -> PtySize {
    PtySize {
        rows: size.rows.max(1),
        cols: size.cols.max(2),
        pixel_width: size.cell_width.saturating_mul(size.cols),
        pixel_height: size.cell_height.saturating_mul(size.rows),
    }
}

impl Spawner for NativePty {
    fn spawn(&self, spec: &Spec, size: TerminalSize) -> Result<Spawned, String> {
        let pair = portable_pty::native_pty_system()
            .openpty(pty_size(size))
            .map_err(|error| error.to_string())?;
        let mut command = CommandBuilder::new(&spec.program);
        command.args(&spec.args);
        command.cwd(&spec.cwd);
        for key in &spec.unset {
            command.env_remove(key);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| error.to_string())?;
        // The shell holds the only slave end now: the reader sees the end when it exits.
        drop(pair.slave);
        let output = pair
            .master
            .try_clone_reader()
            .map_err(|error| error.to_string())?;
        let input = pair
            .master
            .take_writer()
            .map_err(|error| error.to_string())?;
        let pid = child.process_id().and_then(|pid| i32::try_from(pid).ok());
        Ok(Spawned {
            process: Box::new(NativeProcess {
                master: pair.master,
                pid,
            }),
            output,
            input,
            wait: Box::new(move || match child.wait() {
                Ok(status) if status.signal().is_none() => i32::try_from(status.exit_code()).ok(),
                _ => None,
            }),
        })
    }
}

struct NativeProcess {
    master: Box<dyn MasterPty + Send>,
    pid: Option<i32>,
}

impl Process for NativeProcess {
    fn resize(&self, size: TerminalSize) {
        let _ = self.master.resize(pty_size(size));
    }

    #[cfg(unix)]
    fn foreground(&self) -> Option<i32> {
        let pgrp = self.master.process_group_leader()?;
        (pgrp > 0 && Some(pgrp) != self.pid).then_some(pgrp)
    }

    #[cfg(not(unix))]
    fn foreground(&self) -> Option<i32> {
        None
    }

    fn hangup(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid.filter(|pid| *pid > 1) {
            // SAFETY: `kill` takes any pid and signal number; an exited shell fails harmlessly.
            unsafe {
                libc::kill(pid, libc::SIGHUP);
                libc::kill(-pid, libc::SIGHUP);
            }
        }
    }
}

#[cfg(all(test, unix))]
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
