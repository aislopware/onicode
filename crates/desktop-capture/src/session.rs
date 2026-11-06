//! Whether the Mac's screens show this process's session: the session dictionary
//! CoreGraphics keeps for the login session the process runs in.
//!
//! The keys below are `#define … CFSTR("…")` macros, so no objc2 static exists for them, and
//! this is the one place in the workspace that spells them (DECISIONS.md, "Constants the SDK
//! defines as `CFSTR` macros").

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::CGSessionCopyCurrentDictionary;

use crate::source::Console;

/// `kCGSessionOnConsoleKey` (`CoreGraphics/CGSession.h`): the session is the one the Mac's
/// screens, keyboard and mouse belong to. False once fast user switching has moved another
/// session, or the login window, onto them.
const ON_CONSOLE: &str = "kCGSSessionOnConsoleKey";
/// `kCGSessionLoginDoneKey` (`CoreGraphics/CGSession.h`): the login has finished.
const LOGIN_DONE: &str = "kCGSessionLoginDoneKey";
/// `kIOConsoleSessionScreenIsLockedKey` (xnu `IOKit/IOKitKeysPrivate.h`): the session's
/// screens are locked. Present, and true, only while they are.
const SCREEN_IS_LOCKED: &str = "CGSSessionScreenIsLocked";

/// What the Mac's screens show of this session now; `None` when the process has no
/// window-server session to ask about. One window-server round trip.
#[must_use]
pub fn console() -> Option<Console> {
    let session = CGSessionCopyCurrentDictionary()?;
    // SAFETY: `CGSession.h` documents a dictionary keyed by the `kCGSession…` strings, and every
    // value read here is checked with `downcast` before use.
    let session: CFRetained<CFDictionary<CFString, CFType>> =
        unsafe { CFRetained::cast_unchecked(session) };
    let flag = |key: &str| {
        let key = CFString::from_str(key);
        session
            .get(&key)
            .and_then(|v| v.downcast::<CFBoolean>().ok())
            .map(|b| b.as_bool())
    };
    Some(Console::of(
        flag(ON_CONSOLE),
        flag(LOGIN_DONE),
        flag(SCREEN_IS_LOCKED),
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// What one read of the session costs, and what it says here: run by hand, since the
    /// answer depends on whoever is at this Mac (`docs/MEASUREMENTS.md`, "the Mac's lock
    /// state").
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture"]
    fn console_read_cost() {
        let mut took: Vec<u128> = std::iter::repeat_with(|| {
            let started = Instant::now();
            std::hint::black_box(console());
            started.elapsed().as_nanos()
        })
        .take(2_000)
        .collect();
        took.sort_unstable();
        eprintln!(
            "MEASURE console read: {:?}; p50 {} ns, p99 {} ns, max {} ns over {}",
            console(),
            took[took.len() / 2],
            took[took.len() * 99 / 100],
            took[took.len() - 1],
            took.len()
        );
    }
}
