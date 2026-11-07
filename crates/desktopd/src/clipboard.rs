//! The Mac's clipboard in desktop sessions (`docs/onicode/decisions/remote-desktop.md`, "The
//! clipboard follows the window"; the logic is `onicode-clipboard`'s).
//!
//! desktopd follows the general pasteboard for the owner's viewer whose window has focus
//! ([`crate::session`] gives each owner's link a clipboard session). It reads the pasteboard
//! on the Mac the person is not sitting at, which macOS 15.4 and later gate behind "Paste from
//! Other Apps" for Onicode Desktop. Until that is set to Allow, an offer carries types alone and
//! desktopd reads a copy's bytes only when the viewer pastes it, so nothing prompts that the
//! person did not cause; denied, copies on this Mac stay here. The viewer hears which it is
//! ([`onicode_clipboard::Clip::Access`]) and offers the guide to Paste from Other Apps.

use onicode_clipboard::{Hub, PasteAccess};

/// What a programmatic read of the general pasteboard does for desktopd, in the session's terms.
#[must_use]
pub const fn paste_access(behavior: onicode_grants::paste::PasteBehavior) -> PasteAccess {
    use onicode_grants::paste::PasteBehavior;
    match behavior {
        PasteBehavior::Default => PasteAccess::NotAsked,
        PasteBehavior::Ask => PasteAccess::Ask,
        PasteBehavior::AlwaysAllow => PasteAccess::Allowed,
        PasteBehavior::AlwaysDeny => PasteAccess::Denied,
    }
}

/// The general pasteboard's hub, as `device` (desktopd's id in its hello), its access read
/// every [`onicode_clipboard::ACCESS_EVERY`] without a prompt. Call it inside the runtime.
#[must_use]
pub fn general(device: &str) -> Option<Hub> {
    use std::sync::Arc;

    use onicode_clipboard::{ClipboardSettings, HubConfig, MacBoard, SystemClock};

    let runtime = tokio::runtime::Handle::try_current().ok()?;
    let read = || paste_access(onicode_grants::paste::behavior());
    let (tx, rx) = tokio::sync::watch::channel(read());
    runtime.spawn(async move {
        let mut every = tokio::time::interval(onicode_clipboard::ACCESS_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            if tx.is_closed() {
                break;
            }
            let Ok(now) = tokio::task::spawn_blocking(read).await else {
                break;
            };
            tx.send_if_modified(|was| {
                let changed = *was != now;
                if changed {
                    tracing::info!(?was, ?now, "Paste from Other Apps changed");
                    *was = now;
                }
                changed
            });
        }
    });
    let cache = cache_dir();
    if let Err(error) = std::fs::remove_dir_all(&cache)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!(cache = %cache.display(), %error, "clipboard cache kept");
    }
    Some(Hub::start(
        HubConfig {
            device: device.to_owned(),
            board: Arc::new(MacBoard::general()),
            clock: Arc::new(SystemClock),
            access: Arc::new(move || *rx.borrow()),
            cache,
            settings: ClipboardSettings::default(),
        },
        &runtime,
    ))
}

/// Where files pasted from a viewer land: `~/Library/Caches/Onicode Desktop/clipboard`, emptied
/// at each start and kept to the newest copy.
fn cache_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from)
        .join("Library/Caches/Onicode Desktop/clipboard")
}

#[cfg(test)]
mod tests {
    use onicode_grants::paste::PasteBehavior;

    use super::*;

    #[test]
    fn only_allow_reads_freely_and_deny_stops() {
        assert!(paste_access(PasteBehavior::AlwaysAllow).reads_freely());
        for asks in [PasteBehavior::Default, PasteBehavior::Ask] {
            let access = paste_access(asks);
            assert!(!access.reads_freely() && !access.stops(), "{asks:?}");
        }
        assert!(paste_access(PasteBehavior::AlwaysDeny).stops());
    }
}
