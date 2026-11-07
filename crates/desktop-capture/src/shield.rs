//! The curtain's shield: a black window over each of the Mac's own displays, which no display
//! capture of this process shows.
//!
//! Each window is borderless, opaque black, at the shielding level (`CGShieldingWindowLevel`,
//! above every app, the menu bar and the Dock), on every Space, and lets the mouse through, so
//! the controller's injected clicks reach the apps beneath while the person at the Mac sees
//! black. Its window number goes to [`crate::leave_out`], so every display capture resolved
//! after it leaves the windows out of the picture.
//!
//! Raising is two steps, so a viewer never sees black: [`Shield::raise`] makes the windows
//! without putting them on screen and leaves them out; the caller then has every running capture
//! resolve its filter again (a fresh enumeration lists the new windows), and only then calls
//! [`Shield::show`]. Dropping the shield takes the windows down. A process that dies takes its
//! windows with it: the window server removes them, so a crashed desktopd never leaves a Mac
//! black.
//!
//! AppKit windows are the main thread's: desktopd's main thread runs the run loop that serves the
//! main queue (`onicode_desktop_vdisplay::park_main`), and each step here runs on it.

use dispatch2::{DispatchQueue, MainThreadBound, run_on_main};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly as _};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSScreen, NSWindow, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowLevel, NSWindowStyleMask,
};
use objc2_core_graphics::CGShieldingWindowLevel;

/// The shield over the Mac's own displays, while it lives.
pub struct Shield {
    windows: Option<MainThreadBound<Vec<Retained<NSWindow>>>>,
}

impl std::fmt::Debug for Shield {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shield").finish_non_exhaustive()
    }
}

impl Shield {
    /// Make a black window over every display of the Mac's but those `skip` names (displays
    /// made for clients, which no one sits in front of), not yet on screen, and leave them out of
    /// every display capture from now on.
    ///
    /// # Errors
    ///
    /// Why there is no shield, in words for the client: the Mac has no display of its own, or
    /// AppKit would not make a window.
    pub fn raise(skip: impl Fn(u32) -> bool + Send) -> Result<Self, String> {
        let (windows, numbers) = run_on_main(move |mtm| {
            let windows = make_windows(mtm, &skip);
            let numbers = windows
                .iter()
                .map(|window| u32::try_from(window.windowNumber()).ok().filter(|n| *n > 0))
                .collect::<Option<Vec<u32>>>();
            (MainThreadBound::new(windows, mtm), numbers)
        });
        let shield = Self {
            windows: Some(windows),
        };
        let Some(numbers) = numbers else {
            return Err("the Mac's window server gave the curtain no window".to_owned());
        };
        if numbers.is_empty() {
            return Err("this Mac has no screen of its own to cover".to_owned());
        }
        crate::leave_out(numbers);
        Ok(shield)
    }

    /// Put the windows on screen, in front of everything.
    pub fn show(&self) {
        let Some(windows) = &self.windows else {
            return;
        };
        run_on_main(|mtm| {
            for window in windows.get(mtm) {
                window.orderFrontRegardless();
            }
        });
    }
}

impl Drop for Shield {
    /// Take the windows down on the main queue, then stop leaving them out: a window number
    /// still left out once its window is gone would fail every later resolve.
    fn drop(&mut self) {
        let Some(windows) = self.windows.take() else {
            return;
        };
        DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            for window in windows.into_inner(mtm) {
                window.orderOut(None);
                window.close();
            }
            crate::leave_out(Vec::new());
        });
    }
}

/// One shield window over each screen `skip` does not name, off screen.
fn make_windows(mtm: MainThreadMarker, skip: &dyn Fn(u32) -> bool) -> Vec<Retained<NSWindow>> {
    // The process's connection to the window server, which a window needs.
    let _app = NSApplication::sharedApplication(mtm);
    let black = NSColor::blackColor();
    let level = NSWindowLevel::try_from(CGShieldingWindowLevel()).unwrap_or(NSWindowLevel::MAX);
    NSScreen::screens(mtm)
        .iter()
        .filter(|screen| !skip(screen.CGDirectDisplayID()))
        .map(|screen| {
            // SAFETY: a fresh allocation on the main thread; borderless, buffered, made at once
            // (not deferred) so it has its window number before it is ever shown.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    screen.frame(),
                    NSWindowStyleMask::Borderless,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            // SAFETY: the window is held by its `Retained` until it is closed, so closing must
            // not release it a second time.
            unsafe { window.setReleasedWhenClosed(false) };
            window.setBackgroundColor(Some(&black));
            window.setOpaque(true);
            window.setHasShadow(false);
            window.setIgnoresMouseEvents(true);
            window.setCanHide(false);
            window.setLevel(level);
            window.setAnimationBehavior(NSWindowAnimationBehavior::None);
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle
                    | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            window
        })
        .collect()
}
