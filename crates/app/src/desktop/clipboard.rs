//! This Mac's clipboard while a desktop window is key (`docs/onicode/decisions/remote-desktop.md`,
//! "The clipboard follows the window"; the logic is `onicode-clipboard`'s).
//!
//! The app keeps one [`Hub`] on the general pasteboard, started with the first desktop window
//! on the desktop runtime. Each session follows it ([`attach`]); while a window is key, its
//! session follows the Mac it shows both ways: this Mac's pasteboard is watched and its copies
//! offered to that Mac, and that Mac's copies are written here at once, small ones as bytes and
//! the rest as pasteboard item providers that fetch only when something pastes them. The app
//! reads its own pasteboard only while it is in front, which macOS does not prompt for; the
//! host's reads are gated by Paste from Other Apps there, which [`paste_banner`] explains.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use onicode_clipboard::{ClipboardSettings, Hub, HubConfig, MacBoard, PasteAccess, SystemClock};
use onicode_desktop_client::DesktopSession;

/// The app's one hub on the general pasteboard.
static HUB: OnceLock<Hub> = OnceLock::new();

/// Follow this Mac's clipboard in `session` as `device` (the id the desktop client binds as),
/// unless the session shows this Mac itself: one pasteboard needs no following.
pub(super) fn attach(
    session: &DesktopSession,
    runtime: &tokio::runtime::Handle,
    device: &str,
    local: bool,
) {
    if local {
        return;
    }
    let hub = HUB.get_or_init(|| {
        Hub::start(
            HubConfig {
                device: device.to_owned(),
                board: Arc::new(MacBoard::general()),
                clock: Arc::new(SystemClock),
                // The app reads its pasteboard only while a desktop window is key: in front.
                access: Arc::new(|| PasteAccess::Ungated),
                cache: cache_dir(),
                settings: ClipboardSettings::default(),
            },
            runtime,
        )
    });
    session.clipboard(Some(hub.clone()));
}

/// Where files copied on another Mac land: `~/Library/Caches/Onicode/clipboard`, kept to the
/// newest copy.
fn cache_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(std::env::temp_dir, PathBuf::from)
        .join("Library/Caches/Onicode/clipboard")
}

/// The banner that says why copies on `host` wait for a paste or stay there, while its
/// Paste from Other Apps for Onicode Desktop is not Allow; `None` once it is.
pub(super) fn paste_banner(paste: Option<PasteAccess>, host: &str) -> Option<String> {
    match paste? {
        PasteAccess::Ungated | PasteAccess::Allowed => None,
        PasteAccess::NotAsked | PasteAccess::Ask => Some(format!(
            "{host} asks before Onicode Desktop sends its copies: allow the paste there, then set \
             Onicode Desktop to Allow in Paste from Other Apps."
        )),
        PasteAccess::Denied => Some(format!(
            "Paste from Other Apps is off for Onicode Desktop on {host}, so copies there stay \
             there."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_shows_until_the_host_allows() {
        assert_eq!(paste_banner(None, "mac"), None, "not following yet");
        assert_eq!(paste_banner(Some(PasteAccess::Allowed), "mac"), None);
        assert_eq!(paste_banner(Some(PasteAccess::Ungated), "mac"), None);
        for waits in [PasteAccess::NotAsked, PasteAccess::Ask, PasteAccess::Denied] {
            let text = paste_banner(Some(waits), "mac-studio").unwrap();
            assert!(text.contains("mac-studio"), "{text}");
        }
    }
}
