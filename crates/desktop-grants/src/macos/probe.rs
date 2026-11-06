//! Reading the grants in a new process, and restarting desktopd once that process sees one this
//! process cannot.
//!
//! A process reads its Screen Recording state once: a grant made while it runs stays invisible
//! until it starts again. So desktopd starts itself again with [`PROBE_ARG`]: the child is the
//! same signed executable, and under launchd desktopd is its own responsible process, so TCC
//! attributes the child to desktopd's identity and the child reads today's state. When the child
//! sees a grant desktopd does not, desktopd exits with [`RESTART_EXIT_CODE`] and launchd's
//! `KeepAlive` (`SuccessfulExit = false`) starts it again.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::{io, thread};

use crate::{Grants, needs_restart};

/// The argument that makes desktopd print its grants and exit ([`answer_probe_if_asked`]).
pub const PROBE_ARG: &str = "--probe-grants";

/// desktopd's exit status when it leaves for launchd to start it again: `EX_TEMPFAIL`, non-zero,
/// so `KeepAlive = { SuccessfulExit = false }` restarts it.
pub const RESTART_EXIT_CODE: i32 = 75;

/// When the running watch gives up; `None` while none runs.
static WATCH_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Answers a probe and exits when this process was started with [`PROBE_ARG`]; returns at once
/// otherwise. desktopd calls it first thing in `main`, before logging or anything that could
/// touch a grant.
pub fn answer_probe_if_asked() {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == PROBE_ARG)
    {
        println!("{}", Grants::current().to_line());
        std::process::exit(0);
    }
}

/// The grants a new copy of this executable reads.
///
/// # Errors
///
/// The executable cannot be found or started, or its answer is not a grants line.
pub fn probe_fresh() -> io::Result<Grants> {
    probe_with(&std::env::current_exe()?)
}

/// The grants `program`, started with [`PROBE_ARG`], reads.
///
/// # Errors
///
/// `program` cannot be started, fails, or answers something other than a grants line.
pub fn probe_with(program: &Path) -> io::Result<Grants> {
    let output = Command::new(program)
        .arg(PROBE_ARG)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "the grants probe exited with {}",
            output.status
        )));
    }
    let line = String::from_utf8_lossy(&output.stdout);
    Grants::parse_line(&line).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

/// Watches, every `every` until `give_up_after` from now, for a grant a new process sees and this
/// one does not ([`needs_restart`]); then runs `before_exit` and exits with
/// [`RESTART_EXIT_CODE`] for launchd to start desktopd again. Ends quietly once every grant is
/// on here or the time is up.
///
/// desktopd calls it when the guide opens (`CtlRequest::GuideOpen`). A call while a watch runs
/// only moves that watch's end; it answers `false` then, `true` when it started one.
///
/// # Errors
///
/// The watch's thread could not start.
pub fn restart_when_granted(
    every: Duration,
    give_up_after: Duration,
    before_exit: impl FnOnce() + Send + 'static,
) -> io::Result<bool> {
    let until = Instant::now() + give_up_after;
    {
        let mut watch = WATCH_UNTIL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(current) = watch.as_mut() {
            *current = (*current).max(until);
            return Ok(false);
        }
        *watch = Some(until);
    }
    let spawned = thread::Builder::new()
        .name("grants-watch".to_owned())
        .spawn(move || watch(every, before_exit));
    if let Err(err) = spawned {
        *WATCH_UNTIL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        return Err(err);
    }
    Ok(true)
}

fn watch(every: Duration, before_exit: impl FnOnce()) {
    loop {
        let live = Grants::current();
        let fresh = if live.complete() {
            None
        } else {
            probe_fresh().ok()
        };
        if fresh.is_some_and(|fresh| needs_restart(live, fresh)) {
            before_exit();
            std::process::exit(RESTART_EXIT_CODE);
        }
        {
            let mut until = WATCH_UNTIL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if live.complete() || until.is_none_or(|at| Instant::now() >= at) {
                *until = None;
                return;
            }
        }
        thread::sleep(every);
    }
}
