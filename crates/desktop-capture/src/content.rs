//! Shareable content: what can be captured. Whole displays only; slopty's window listing is
//! gone with window streams. Listing displays for a picker goes through CoreGraphics
//! ([`crate::active_displays`]), which needs no grant and raises no prompt; this enumeration
//! runs only when a stream opens.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{SCDisplay, SCRunningApplication, SCShareableContent, SCWindow};
use onicode_desktop_proto::screen::DisplayInfo;
use parking_lot::Mutex;

use crate::CaptureError;

/// A snapshot of the displays (and the windows, for leaving some out) on the host, with the
/// ScreenCaptureKit objects needed to open a stream on any of them.
pub struct Shareable {
    inner: Retained<SCShareableContent>,
}

// SAFETY: `SCShareableContent`, `SCWindow` and `SCDisplay` are immutable value objects that
// ScreenCaptureKit itself hands to arbitrary queues; nothing here mutates them.
unsafe impl Send for Shareable {}
// SAFETY: as above; all accessors are reads.
unsafe impl Sync for Shareable {}

impl std::fmt::Debug for Shareable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shareable")
            .field("displays", &self.displays().len())
            .finish()
    }
}

impl Shareable {
    /// Attached displays.
    #[must_use]
    pub fn displays(&self) -> Vec<DisplayInfo> {
        // SAFETY: valid object; the array is a snapshot.
        let displays = unsafe { self.inner.displays() };
        displays.iter().map(|d| display_info(&d)).collect()
    }

    /// The window numbered `number` (a `CGWindowID`), for leaving it out of a display capture.
    pub(crate) fn window(&self, number: u32) -> Option<Retained<SCWindow>> {
        // SAFETY: valid object.
        let windows = unsafe { self.inner.windows() };
        // SAFETY: `windowID` is a plain getter.
        windows.iter().find(|w| unsafe { w.windowID() } == number)
    }

    /// The running applications of processes `pids`; one that has quit is left out.
    pub(crate) fn applications(
        &self,
        pids: &std::collections::BTreeSet<i32>,
    ) -> Vec<Retained<SCRunningApplication>> {
        // SAFETY: valid object; the array is a snapshot.
        let apps = unsafe { self.inner.applications() };
        // SAFETY: `processID` is a plain getter.
        apps.iter()
            .filter(|app| pids.contains(&unsafe { app.processID() }))
            .collect()
    }

    pub(crate) fn display(&self, id: u32) -> Option<Retained<SCDisplay>> {
        // SAFETY: valid object.
        let displays = unsafe { self.inner.displays() };
        // SAFETY: `displayID` is a plain getter.
        displays.iter().find(|d| unsafe { d.displayID() } == id)
    }
}

fn display_info(d: &SCDisplay) -> DisplayInfo {
    // SAFETY: plain property read on a valid object.
    crate::geometry::display_info(unsafe { d.displayID() })
}

/// Ask ScreenCaptureKit for the current shareable content. `done` runs on a ScreenCaptureKit
/// queue. The first call in a process triggers the Screen Recording permission prompt.
pub fn enumerate(done: impl FnOnce(Result<Shareable, CaptureError>) + Send + 'static) {
    crate::ensure_core_graphics();
    let done = Mutex::new(Some(done));
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let Some(done) = done.lock().take() else {
                return;
            };
            // SAFETY: ScreenCaptureKit passes a live, autoreleased object (or NULL);
            // retaining it gives us our own reference.
            let content = unsafe { Retained::retain(content) };
            let result = match (content, NonNull::new(error)) {
                (Some(inner), _) => Ok(Shareable { inner }),
                // SAFETY: a live error object for the callback's duration.
                (None, Some(error)) => Err(CaptureError::from_ns(unsafe { error.as_ref() })),
                (None, None) => Err(CaptureError::Stopped("no content, no error".to_owned())),
            };
            done(result);
        },
    );
    // SAFETY: the block is copied by the framework before this returns; it captures only
    // `Send` data.
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, false, &block,
        );
    }
}
