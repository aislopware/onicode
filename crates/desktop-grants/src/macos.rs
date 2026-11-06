//! The grants as this process sees them, System Settings' deep links, and the AppKit side of the
//! guide.

pub mod agent;
pub(crate) mod panel;
pub(crate) mod probe;
pub(crate) mod settings;

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationOptions, NSWorkspace};
use objc2_application_services::AXIsProcessTrusted;
use objc2_core_graphics::{CGPreflightPostEventAccess, CGPreflightScreenCaptureAccess};
use objc2_foundation::{NSString, NSURL};

use crate::{Grants, Pane};

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

/// Opens System Settings at `pane`'s list. Opening a link activates nothing by itself when
/// System Settings is already running; [`activate_settings`] brings it forward.
pub fn open_pane(pane: Pane) -> bool {
    let Some(url) = NSURL::URLWithString(&NSString::from_str(&pane.settings_url())) else {
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
