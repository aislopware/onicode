//! desktopd's grants, read through `onicode_grants` and told in the session's terms.
//!
//! The reads never prompt (preflights only). A running process keeps seeing the Screen Recording
//! state it started with, so a grant turned on later is picked up by a restart:
//! [`guide_open`] has `onicode_grants` wait for it and exit for launchd to start desktopd again.
//!
//! The app's onboarding checklist reads and asks for these grants by starting the helper with
//! [`GrantsCommand`]'s arguments: a prompt belongs to the process that raises it, and only the
//! helper's own process sees its grants.

use std::ffi::OsStr;
use std::time::Duration;

use onicode_desktop_link::{HostGrants, Pane};
use onicode_grants::Privilege;
use onicode_grants::helper::{PROBE_ARG, REQUEST_ARG};
use tokio::sync::watch;

/// What desktopd was started to do about its grants, read from its arguments before anything
/// else runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GrantsCommand {
    /// Print the grants' line and exit ([`PROBE_ARG`]).
    Probe,
    /// Raise one alert in this process, then print the grants' line and exit ([`REQUEST_ARG`]).
    Request(Privilege),
}

impl GrantsCommand {
    /// Reads the arguments after the program's name; `None` when they ask for neither.
    ///
    /// # Errors
    ///
    /// A request that names no privilege, one that is not the helper's, or anything after it.
    pub fn parse<S: AsRef<OsStr>>(args: &[S]) -> Result<Option<Self>, String> {
        let mut args = args.iter().map(AsRef::as_ref);
        let Some(first) = args.next() else {
            return Ok(None);
        };
        let command = if first == PROBE_ARG {
            Self::Probe
        } else if first == REQUEST_ARG {
            let key = args
                .next()
                .and_then(OsStr::to_str)
                .ok_or_else(|| format!("{REQUEST_ARG} names no privilege"))?;
            let privilege = Privilege::from_key(key)
                .filter(|privilege| Privilege::HELPER.contains(privilege))
                .ok_or_else(|| format!("{key} is not one of Onicode Desktop's grants"))?;
            Self::Request(privilege)
        } else {
            return Ok(None);
        };
        match args.next() {
            None => Ok(Some(command)),
            Some(extra) => Err(format!("unexpected {}", extra.to_string_lossy())),
        }
    }
}

/// How often the grants are read again while desktopd runs: Accessibility and posting events
/// read live, so a grant turned on (or off) shows within this.
pub const READ_EVERY: Duration = Duration::from_secs(2);

/// How often the guide's watch looks whether a new process sees the grant.
pub const WATCH_EVERY: Duration = Duration::from_secs(1);

/// How long the guide's watch waits for the grant before it gives up.
pub const WATCH_FOR: Duration = Duration::from_mins(10);

/// The one adapter between the grants crate and the session's wire.
#[must_use]
pub const fn host_grants(grants: onicode_grants::Grants) -> HostGrants {
    HostGrants {
        screen_recording: grants.screen_recording,
        accessibility: grants.accessibility,
        post_event: grants.post_event,
    }
}

/// The privilege a session's pane opens the list of. Posting events is listed under
/// Accessibility, so its pane is Accessibility's.
#[must_use]
pub const fn privilege(pane: Pane) -> Privilege {
    match pane {
        Pane::ScreenRecording => Privilege::ScreenRecording,
        Pane::Accessibility => Privilege::Accessibility,
        Pane::PasteFromOtherApps => Privilege::Pasteboard,
    }
}

/// The grants as of now, without a prompt: this process's, and when something is off, what a new
/// process sees on top (`Grants::fresh`).
#[must_use]
pub fn current() -> HostGrants {
    host_grants(onicode_grants::Grants::fresh())
}

/// The guide to `pane` is open on this Mac's screen: wait for the grant, and once a new process
/// sees it run `before_exit` and exit for launchd to start desktopd again.
///
/// # Errors
///
/// The watch could not start, in words for the engine.
pub fn guide_open(pane: Pane, before_exit: impl FnOnce() + Send + 'static) -> Result<(), String> {
    if pane == Pane::PasteFromOtherApps {
        // Read again every few seconds by the clipboard (`crate::clipboard`): nothing to restart.
        drop(before_exit);
        tracing::info!("the guide to Paste from Other Apps is open");
        return Ok(());
    }
    let pane = privilege(pane).list_name();
    tracing::info!(pane, "the guide is open: restarting once the grant is on");
    onicode_grants::restart_when_granted(WATCH_EVERY, WATCH_FOR, before_exit)
        .map(drop)
        .map_err(|e| format!("no watch for the grant: {e}"))
}

/// The grants, read now and every [`READ_EVERY`] by `read`, as a watch whose value changes only
/// when they do. The reads end when every receiver is gone.
pub fn follow(read: fn() -> HostGrants) -> watch::Receiver<HostGrants> {
    let (tx, rx) = watch::channel(read());
    tokio::spawn(async move {
        let mut every = tokio::time::interval(READ_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if tx.is_closed() {
                break;
            }
            let now = tokio::task::spawn_blocking(read)
                .await
                .unwrap_or_else(|_| read());
            tx.send_if_modified(|was| {
                let changed = *was != now;
                if changed {
                    tracing::info!(?was, ?now, "the grants changed");
                    *was = now;
                }
                changed
            });
        }
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adapter_keeps_every_grant() {
        for bits in 0_u8..8 {
            let grants = onicode_grants::Grants {
                screen_recording: bits & 1 != 0,
                accessibility: bits & 2 != 0,
                post_event: bits & 4 != 0,
            };
            let host = host_grants(grants);
            assert_eq!(host.complete(), grants.complete());
            let panes = host.missing();
            let missing = grants.missing();
            assert_eq!(
                panes.contains(&Pane::ScreenRecording),
                missing.contains(&Privilege::ScreenRecording)
            );
            assert_eq!(
                panes.contains(&Pane::Accessibility),
                missing.contains(&Privilege::Accessibility)
                    || missing.contains(&Privilege::PostEvent)
            );
            for pane in panes {
                assert!(missing.contains(&privilege(pane)) || pane == Pane::Accessibility);
            }
        }
    }

    #[test]
    fn the_grants_arguments_parse() {
        let none: [&str; 0] = [];
        assert_eq!(GrantsCommand::parse(&none), Ok(None));
        assert_eq!(GrantsCommand::parse(&["--synthetic"]), Ok(None));
        assert_eq!(GrantsCommand::parse(&["doctor"]), Ok(None));
        assert_eq!(
            GrantsCommand::parse(&["--probe-grants"]),
            Ok(Some(GrantsCommand::Probe))
        );
        for privilege in Privilege::HELPER {
            let args = onicode_grants::helper::request_args(privilege);
            assert_eq!(
                GrantsCommand::parse(&args),
                Ok(Some(GrantsCommand::Request(privilege)))
            );
        }
        assert_eq!(
            GrantsCommand::parse(&["--request-grant", "screen-recording"]),
            Ok(Some(GrantsCommand::Request(Privilege::ScreenRecording)))
        );
        // Refused: no privilege, one the app holds, an unknown one, or more after it.
        assert!(GrantsCommand::parse(&["--request-grant"]).is_err());
        assert!(GrantsCommand::parse(&["--request-grant", "microphone"]).is_err());
        assert!(GrantsCommand::parse(&["--request-grant", "camera"]).is_err());
        assert!(GrantsCommand::parse(&["--probe-grants", "x"]).is_err());
        assert!(GrantsCommand::parse(&["--request-grant", "accessibility", "x"]).is_err());
    }
}
