//! `onicode-desktopd`: remote desktop's host, the login-item agent that streams a Mac's displays
//! to the owner's other devices (`docs/onicode/decisions/remote-desktop.md`).
//!
//! It runs from `Onicode.app/Contents/Library/LoginItems/Onicode Desktop.app` as an
//! `SMAppService` agent, so the Screen Recording and Accessibility grants are that bundle's own
//! and it outlives the app. The engine finds it on a Unix socket in the data directory ([`ctl`])
//! and hands clients its address; clients dial its own QUIC endpoint on the tailnet
//! ([`session`]), plaintext, admitted by tailscale whois.
//!
//! - [`session`]: one task per client link. A join, the grants gate, the controller seat
//!   ([`seat`]), the screen requests, the feedback datagrams and the numbered input copies.
//! - [`pipeline`]: one capture and encode per client and stream (per-viewer encodes): slopty's
//!   whole-display pipeline with its repair loop, long-term references, rate control, chroma gate,
//!   temporal layers, refinement, the lane that keeps sound ahead of video, the cursor and the
//!   heartbeat. [`stripes`] and [`engines`] share the two encode engines between streams.
//! - [`sized`] and [`holder`]: displays made for a client at its own size, held by a process of
//!   their own so a restart of desktopd moves no window.
//! - [`sound`]: a client's one sound, Opus in 10 ms packets with copies while the path loses.
//! - [`clipboard`]: the Mac's general pasteboard, followed by the owner's viewer while its window
//!   has focus (`onicode-clipboard`), read unasked only once Paste from Other Apps allows it.
//! - [`curtain`]: the curtain over the Mac's own screens while the controller drives it: a black
//!   shield window over each display, left out of every capture, so the person at the Mac sees
//!   black and the viewer the desktop.
//! - [`platform`]: the seams (capture, encoders, input, the curtain's drapes, keeping the displays
//!   awake while a viewer is joined, displays): macOS's ([`platform::Native`]) and the drawn one of
//!   the tests ([`synthetic`]).
//!
//! Nothing here prompts for a grant: no `CGRequest*`, no Accessibility prompt, and no
//! ScreenCaptureKit call before Screen Recording reads granted ([`grants`]), since listing
//! shareable content prompts too. Displays are listed with CoreGraphics.

#![deny(unsafe_code)]

pub mod clipboard;
pub mod ctl;
pub mod curtain;
pub mod engines;
pub mod grants;
pub mod holder;
pub mod pipeline;
pub mod platform;
pub mod seat;
pub mod session;
pub mod sized;
pub mod sound;
pub mod stripes;
pub mod synthetic;

use std::path::{Path, PathBuf};
use std::time::Instant;

/// Now, on tokio's clock: the system's, or a test's paused one, so a stream's timers and its
/// clock readings agree in virtual time. Off a runtime, the system's.
#[must_use]
pub fn instant() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// The data directory, the one the app and its engine keep (the app's `data_dir`), so the
/// engine finds the control socket and the engine's `device-id` there: `$ONICODE_DATA_DIR`, else
/// `~/Library/Application Support/Onicode`, or `Onicode Development` for a development build,
/// whose app is built in the same profile.
#[must_use]
pub fn data_dir() -> PathBuf {
    data_dir_from(
        std::env::var_os("ONICODE_DATA_DIR").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
        cfg!(debug_assertions),
    )
}

/// [`data_dir`] from the two variables it reads, for a development build when `development`.
#[must_use]
pub fn data_dir_from(dir: Option<PathBuf>, home: Option<PathBuf>, development: bool) -> PathBuf {
    dir.filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| {
            home.unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("Library/Application Support")
                .join(if development {
                    "Onicode Development"
                } else {
                    "Onicode"
                })
        })
}

/// desktopd's device-id in its hello: the engine's (`{data_dir}/device-id`), or `desktop@{host}`
/// before the engine wrote one, since a peer refuses an empty id.
#[must_use]
pub fn device_id(data_dir: &Path) -> String {
    std::fs::read_to_string(data_dir.join("device-id"))
        .ok()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| format!("desktop@{}", host_name()))
}

/// The helper app bundle `exe` runs from (`…/LoginItems/Onicode Desktop.app`): the nearest
/// enclosing `.app`; `None` for a bare build.
#[must_use]
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .skip(1)
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
}

/// The helper app bundle this process runs from, for the guide ([`bundle_of`]).
#[must_use]
pub fn bundle() -> Option<PathBuf> {
    std::env::current_exe().ok().as_deref().and_then(bundle_of)
}

/// What this Mac calls itself: its computer name, else its host name.
#[must_use]
pub fn host_name() -> String {
    let named = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|name| !name.is_empty());
    if let Some(name) = named {
        return name;
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mac".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_dir_is_the_variable_else_the_apps() {
        assert_eq!(
            data_dir_from(Some("/d".into()), Some("/h".into()), false),
            PathBuf::from("/d")
        );
        assert_eq!(
            data_dir_from(Some(String::new().into()), Some("/h".into()), false),
            PathBuf::from("/h/Library/Application Support/Onicode")
        );
        assert_eq!(
            data_dir_from(None, Some("/h".into()), true),
            PathBuf::from("/h/Library/Application Support/Onicode Development")
        );
    }

    #[test]
    fn the_device_id_is_the_engines_and_never_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(device_id(dir.path()), format!("desktop@{}", host_name()));
        std::fs::write(dir.path().join("device-id"), "dev-mac\n").unwrap();
        assert_eq!(device_id(dir.path()), "dev-mac");
    }
}
