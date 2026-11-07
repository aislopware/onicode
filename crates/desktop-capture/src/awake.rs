//! Keeping the Mac's displays awake while someone watches them: a power assertion
//! (`IOPMAssertionCreateWithName`, `IOKit/pwr_mgt/IOPMLib.h`) of the kind that holds off the
//! display's idle sleep, as a video player's does. A display asleep draws nothing to capture.

use objc2_core_foundation::CFString;
use objc2_io_kit::{
    IOPMAssertionCreateWithName, IOPMAssertionID, IOPMAssertionRelease, kIOPMAssertionLevelOn,
};

/// `kIOPMAssertPreventUserIdleDisplaySleep`, a `#define` in `IOPMLib.h` that objc2 does not
/// bind.
const PREVENT_USER_IDLE_DISPLAY_SLEEP: &str = "PreventUserIdleDisplaySleep";

/// The displays held awake until this is dropped.
#[derive(Debug)]
pub struct KeepAwake {
    id: IOPMAssertionID,
}

impl KeepAwake {
    /// Hold the displays awake, `why` naming the holder in `pmset -g assertions`; `None` when
    /// the power manager refused.
    #[must_use]
    pub fn hold(why: &str) -> Option<Self> {
        let kind = CFString::from_static_str(PREVENT_USER_IDLE_DISPLAY_SLEEP);
        let name = CFString::from_str(why);
        let mut id: IOPMAssertionID = 0;
        // SAFETY: both strings are live for the call, and `id` is a valid place for the answer.
        let refused = unsafe {
            IOPMAssertionCreateWithName(
                Some(&kind),
                kIOPMAssertionLevelOn,
                Some(&name),
                &raw mut id,
            )
        };
        if refused != 0 {
            tracing::warn!(error = refused, "the displays could not be kept awake");
            return None;
        }
        Some(Self { id })
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        let refused = IOPMAssertionRelease(self.id);
        if refused != 0 {
            tracing::debug!(error = refused, "the keep-awake assertion would not go");
        }
    }
}
