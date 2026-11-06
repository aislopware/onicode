//! desktopd's grants, read through `onicode_desktop_grants` and told in the session's terms.
//!
//! The reads never prompt (preflights only). A running process keeps seeing the Screen Recording
//! state it started with, so a grant turned on later is picked up by a restart:
//! [`guide_open`] has `onicode_desktop_grants` wait for it and exit for launchd to start desktopd
//! again.

use std::time::Duration;

use onicode_desktop_link::{HostGrants, Pane};
use tokio::sync::watch;

/// How often the grants are read again while desktopd runs: Accessibility and posting events
/// read live, so a grant turned on (or off) shows within this.
pub const READ_EVERY: Duration = Duration::from_secs(2);

/// How often the guide's watch looks whether a new process sees the grant.
pub const WATCH_EVERY: Duration = Duration::from_secs(1);

/// How long the guide's watch waits for the grant before it gives up.
pub const WATCH_FOR: Duration = Duration::from_mins(10);

/// The one adapter between the grants crate and the session's wire.
#[must_use]
pub const fn host_grants(grants: onicode_desktop_grants::Grants) -> HostGrants {
    HostGrants {
        screen_recording: grants.screen_recording,
        accessibility: grants.accessibility,
        post_event: grants.post_event,
    }
}

/// The grants crate's pane for the session's.
#[must_use]
pub const fn pane(pane: Pane) -> onicode_desktop_grants::Pane {
    match pane {
        Pane::ScreenRecording => onicode_desktop_grants::Pane::ScreenRecording,
        Pane::Accessibility => onicode_desktop_grants::Pane::Accessibility,
    }
}

/// The grants as of now, without a prompt: this process's, and when something is off, what a new
/// process sees on top (`Grants::fresh`). None off a Mac.
#[must_use]
pub fn current() -> HostGrants {
    #[cfg(target_os = "macos")]
    {
        host_grants(onicode_desktop_grants::Grants::fresh())
    }
    #[cfg(not(target_os = "macos"))]
    {
        HostGrants::default()
    }
}

/// The guide to `pane` is open on this Mac's screen: wait for the grant, and once a new process
/// sees it run `before_exit` and exit for launchd to start desktopd again. Off a Mac there is
/// nothing to wait for.
///
/// # Errors
///
/// The watch could not start, or off a Mac, in words for the engine.
pub fn guide_open(pane: Pane, before_exit: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let pane = self::pane(pane).name();
    tracing::info!(pane, "the guide is open: restarting once the grant is on");
    #[cfg(target_os = "macos")]
    {
        onicode_desktop_grants::restart_when_granted(WATCH_EVERY, WATCH_FOR, before_exit)
            .map(drop)
            .map_err(|e| format!("no watch for the grant: {e}"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        drop(before_exit);
        Err("remote desktop hosts only on macOS".to_owned())
    }
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
            let grants = onicode_desktop_grants::Grants {
                screen_recording: bits & 1 != 0,
                accessibility: bits & 2 != 0,
                post_event: bits & 4 != 0,
            };
            let host = host_grants(grants);
            assert_eq!(host.complete(), grants.complete());
            let missing: Vec<_> = host.missing().into_iter().map(pane).collect();
            assert_eq!(missing, grants.missing());
        }
    }
}
