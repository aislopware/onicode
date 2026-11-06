//! Agents outlive a crashed engine unless something else ends them.
//!
//! Each agent runs in its own process group, which the engine ends on every
//! orderly path (`Child::shutdown`, `Drop`). None of those run when the
//! engine is SIGKILLed or aborts on a panic: the agent keeps working, keeps
//! writing its session's transcript, and the engine's boot recovery then
//! resumes that same session beside it — two writers of one conversation.
//!
//! macOS has no parent-death signal, so one watcher per engine does it: a
//! tiny `/bin/sh` loop in its own process group, reading the engine's
//! register on a pipe (`+<pgid>` when an agent starts, `-<pgid>` once it is
//! gone). The pipe's write end lives only in the engine (close-on-exec, so
//! no agent inherits it): when the engine dies for any reason, the kernel
//! closes it, the loop reads EOF, and every group still registered gets
//! SIGTERM, then SIGKILL three seconds later. A pipe was chosen over a
//! kqueue `EVFILT_PROC` watch on the engine's pid: EOF needs no pid, so it
//! cannot be fooled by pid reuse, and it fires for a crash before `exec`
//! as well as after.

use std::io::Write as _;
use std::sync::Mutex;

/// The watcher loop. Groups are kept as a space-separated set.
const SCRIPT: &str = r#"trap '' HUP INT
groups=' '
while IFS= read -r line; do
  case $line in
    +[0-9]*) groups="$groups${line#+} " ;;
    -[0-9]*)
      gone=${line#-}
      kept=' '
      for g in $groups; do [ "$g" = "$gone" ] || kept="$kept$g "; done
      groups=$kept
      ;;
  esac
done
[ "$groups" = ' ' ] && exit 0
for g in $groups; do kill -s TERM -- "-$g" 2>/dev/null; done
sleep 3
for g in $groups; do kill -s KILL -- "-$g" 2>/dev/null; done
"#;

pub(crate) struct Reaper {
    register: std::process::ChildStdin,
    _watcher: std::process::Child,
}

impl Reaper {
    pub(crate) fn spawn() -> std::io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        let mut watcher = std::process::Command::new("/bin/sh")
            .args(["-c", SCRIPT])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // Out of the engine's group: a terminal's Ctrl-C to the engine
            // must not take the watcher with it.
            .process_group(0)
            .spawn()?;
        let register = watcher
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("watcher has no stdin"))?;
        Ok(Self {
            register,
            _watcher: watcher,
        })
    }

    /// End `group` (a positive pgid) if the engine dies first.
    pub(crate) fn watch(&mut self, group: i32) -> std::io::Result<()> {
        writeln!(self.register, "+{group}")
    }

    /// `group` has ended: its pgid may be reused by anything.
    pub(crate) fn release(&mut self, group: i32) -> std::io::Result<()> {
        writeln!(self.register, "-{group}")
    }
}

static REAPER: Mutex<Option<Result<Reaper, ()>>> = Mutex::new(None);

fn with_reaper(f: impl FnOnce(&mut Reaper) -> std::io::Result<()>) {
    let mut reaper = REAPER.lock().unwrap_or_else(|e| e.into_inner());
    let slot = reaper.get_or_insert_with(|| {
        Reaper::spawn().map_err(|error| {
            tracing::warn!(%error, "no agent reaper: agents may outlive a crashed engine");
        })
    });
    if let Ok(reaper) = slot
        && let Err(error) = f(reaper)
    {
        tracing::warn!(%error, "agent reaper gone: agents may outlive a crashed engine");
        *slot = Err(());
    }
}

/// Register an agent's process group (a positive pgid) with the engine's
/// reaper, spawning it on first use.
pub(crate) fn watch(group: i32) {
    with_reaper(|reaper| reaper.watch(group));
}

/// The group has ended.
pub(crate) fn release(group: i32) {
    with_reaper(|reaper| reaper.release(group));
}

#[cfg(test)]
mod tests {
    use std::os::unix::process::CommandExt as _;
    use std::time::{Duration, Instant};

    use super::*;

    fn sleeper() -> std::process::Child {
        std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap()
    }

    fn ends_within(child: &mut std::process::Child, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The engine dying is its end of the pipe closing: every group still
    /// registered ends, a released one is left alone.
    #[test]
    fn groups_end_when_the_engine_goes_and_released_ones_do_not() {
        let mut reaper = Reaper::spawn().unwrap();
        let mut watched = sleeper();
        let mut released = sleeper();
        reaper.watch(watched.id() as i32).unwrap();
        reaper.watch(released.id() as i32).unwrap();
        reaper.release(released.id() as i32).unwrap();
        // While the engine lives, nothing is touched.
        assert!(!ends_within(&mut watched, Duration::from_millis(300)));
        drop(reaper);
        assert!(
            ends_within(&mut watched, Duration::from_secs(5)),
            "a watched group outlived the engine"
        );
        assert!(
            !ends_within(&mut released, Duration::from_millis(500)),
            "a released group was ended"
        );
        released.kill().unwrap();
        released.wait().unwrap();
    }

    /// A group that ignores SIGTERM still ends, by SIGKILL.
    #[test]
    fn a_group_ignoring_sigterm_is_killed() {
        let mut reaper = Reaper::spawn().unwrap();
        let mut stubborn = std::process::Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; sleep 30 & wait"])
            .process_group(0)
            .spawn()
            .unwrap();
        // Let the trap be set before the engine "dies".
        std::thread::sleep(Duration::from_millis(200));
        reaper.watch(stubborn.id() as i32).unwrap();
        drop(reaper);
        assert!(ends_within(&mut stubborn, Duration::from_secs(8)));
    }
}
