//! Reading the grants in a new process, and restarting desktopd once that process sees one this
//! process cannot.
//!
//! A process reads its Screen Recording state once: a grant made while it runs stays invisible
//! until it starts again. So desktopd starts itself again with [`PROBE_ARG`]: the child is the
//! same signed executable, and under launchd desktopd is its own responsible process, so TCC
//! attributes the child to desktopd's identity and the child reads today's state. When the child
//! sees a grant desktopd does not, desktopd exits with [`RESTART_EXIT_CODE`] and launchd's
//! `KeepAlive` (`SuccessfulExit = false`) starts it again.
//!
//! The app reads and asks for the helper's grants the same way, by running the helper: a UI
//! process cannot see another bundle's grants, and a prompt is the asking process's. It starts
//! the helper through Launch Services (`open -n -g -W`), never as its own child, since TCC would
//! judge a child of the app as the app ([`probe_helper`], [`request_in_helper`]).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use std::{io, thread};

use objc2_application_services::{AXIsProcessTrustedWithOptions, kAXTrustedCheckOptionPrompt};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};
use objc2_core_graphics::{CGRequestPostEventAccess, CGRequestScreenCaptureAccess};

use crate::helper::PROBE_ARG;
use crate::{Grants, Privilege, needs_restart};

/// How long a requesting helper stays up after raising its alert, so the alert is up before the
/// process that asked goes.
const REQUEST_LINGER: Duration = Duration::from_secs(2);

/// Names the files a helper's answer lands in.
static ANSWERS: AtomicU64 = AtomicU64::new(0);

/// desktopd's exit status when it leaves for launchd to start it again: `EX_TEMPFAIL`, non-zero,
/// so `KeepAlive = { SuccessfulExit = false }` restarts it.
pub const RESTART_EXIT_CODE: i32 = 75;

/// When the running watch gives up; `None` while none runs.
static WATCH_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// The helper's answer to [`PROBE_ARG`]: its grants' line, printed. desktopd calls it first thing
/// in `main`, before logging or anything that could touch a grant, then exits.
pub fn answer_probe() {
    println!("{}", Grants::current().to_line());
}

/// The helper's answer to [`crate::helper::REQUEST_ARG`]: raises `privilege`'s alert in this
/// process, waits a moment, and prints its grants' line. A privilege that is not the helper's
/// raises nothing. Only ever started from the person's click in the app.
pub fn answer_request(privilege: Privilege) {
    match privilege {
        Privilege::ScreenRecording => {
            let _on = CGRequestScreenCaptureAccess();
        }
        Privilege::Accessibility => {
            // SAFETY: a static key, read once.
            let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
            let options =
                CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[CFBoolean::new(true)]);
            // SAFETY: the options dictionary maps the prompt key to a boolean, as documented.
            let _on = unsafe { AXIsProcessTrustedWithOptions(Some(options.as_opaque())) };
        }
        Privilege::PostEvent => {
            let _on = CGRequestPostEventAccess();
        }
        _ => {}
    }
    if Privilege::HELPER.contains(&privilege) {
        thread::sleep(REQUEST_LINGER);
    }
    answer_probe();
}

/// The grants of the helper bundle at `helper`, read by a fresh copy of it started through
/// Launch Services, so they are its own and current. Never prompts, never takes focus.
///
/// # Errors
///
/// `open` failed, or the helper answered something other than a grants line.
pub fn probe_helper(helper: &Path) -> io::Result<Grants> {
    run_helper(helper, &[PROBE_ARG])
}

/// Has the helper bundle at `helper` raise `privilege`'s alert in its own process, and answers
/// its grants after. Only from the person's click.
///
/// # Errors
///
/// `privilege` is not the helper's, `open` failed, or the answer is not a grants line.
pub fn request_in_helper(helper: &Path, privilege: Privilege) -> io::Result<Grants> {
    if !Privilege::HELPER.contains(&privilege) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not the helper's", privilege.key()),
        ));
    }
    run_helper(helper, &crate::helper::request_args(privilege))
}

/// Runs a fresh, background, hidden copy of `helper` with `args` and reads the grants line it
/// prints.
fn run_helper(helper: &Path, args: &[&str]) -> io::Result<Grants> {
    let answer = answer_path();
    std::fs::write(&answer, b"")?;
    let status = Command::new("/usr/bin/open")
        .args(["-n", "-g", "-j", "-W", "--stdout"])
        .arg(&answer)
        .arg("-a")
        .arg(helper)
        .arg("--args")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let line = std::fs::read_to_string(&answer);
    let _removed = std::fs::remove_file(&answer);
    let status = status?;
    if !status.success() {
        return Err(io::Error::other(format!("open exited with {status}")));
    }
    Grants::parse_line(&line?).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn answer_path() -> PathBuf {
    let n = ANSWERS.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("onicode-grants-{}-{n}.txt", std::process::id()))
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
