//! The macOS side: each privilege read without a prompt and asked on a click, the helper's grants
//! in a fresh process, System Settings' deep links, and the AppKit side of the guide.

pub mod access;
pub mod agent;
mod automation;
mod files;
pub mod microphone;
mod notifications;
pub(crate) mod panel;
pub(crate) mod paste;
pub(crate) mod probe;
pub(crate) mod settings;

use std::io;
use std::path::Path;
use std::process::Command;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationOptions, NSWorkspace};
use objc2_application_services::AXIsProcessTrusted;
use objc2_core_graphics::{CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess};
use objc2_foundation::{NSFileManager, NSString, NSURL};

use crate::{Grants, Install, Privilege};

impl Grants {
    /// What this process may do now, by the preflights alone: no prompt, no listing.
    ///
    /// Screen Recording reads as it did when the process started, so a grant made since is seen
    /// only by a new process ([`crate::probe_fresh`]); Accessibility is live.
    #[must_use]
    pub fn current() -> Self {
        Self {
            screen_recording: CGPreflightScreenCaptureAccess(),
            // SAFETY: the documented no-argument query; it takes and returns nothing owned.
            accessibility: unsafe { AXIsProcessTrusted() },
            post_event: CGPreflightPostEventAccess(),
        }
    }

    /// What desktopd should report: [`Self::current`], and when something is off, whatever a new
    /// process sees on top, so a grant made since this process started reads as on at once.
    /// Falls back to [`Self::current`] when the probe cannot run.
    #[must_use]
    pub fn fresh() -> Self {
        let live = Self::current();
        if live.complete() {
            return live;
        }
        crate::probe_fresh().map_or(live, |fresh| live.union(fresh))
    }
}

/// Opens System Settings at `privilege`'s list (Login Items for the login item). Opening a link
/// activates nothing by itself when System Settings is already running; [`activate_settings`]
/// brings it forward. `false` when the link did not open.
pub fn open_settings(privilege: Privilege) -> bool {
    if privilege == Privilege::BackgroundActivity {
        agent::open_login_items();
        return true;
    }
    let Some(url) = NSURL::URLWithString(&NSString::from_str(&privilege.settings_url())) else {
        return false;
    };
    NSWorkspace::sharedWorkspace().openURL(&url)
}

/// Brings a running System Settings forward with all its windows, yielding this app's
/// activation to it as macOS 14's cooperative activation asks. `false` when it is not running.
pub fn activate_settings(mtm: MainThreadMarker) -> bool {
    let Some(settings) = settings::settings_app() else {
        return false;
    };
    NSApplication::sharedApplication(mtm).yieldActivationToApplication(&settings);
    settings.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows)
}

/// Where the app bundle at `app` runs from, with its folder's writability read from the file
/// system: a disk image's is read-only.
#[must_use]
pub fn install_of(app: &Path) -> Install {
    let folder = app.parent().unwrap_or(app);
    let writable = NSFileManager::defaultManager()
        .isWritableFileAtPath(&NSString::from_str(&folder.to_string_lossy()));
    Install::of(app, writable)
}

/// Arranges a "Quit & Reopen": a detached shell waits for this process to exit, then starts the
/// app bundle at `app` again through Launch Services (`open -n`). The caller quits once this
/// returns, so the two copies never overlap.
///
/// # Errors
///
/// The shell could not start.
pub fn reopen_after_quit(app: &Path) -> io::Result<()> {
    let _waiter = Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"while /bin/kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec /usr/bin/open -n "$2""#)
        .arg("onicode-reopen")
        .arg(std::process::id().to_string())
        .arg(app)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}
