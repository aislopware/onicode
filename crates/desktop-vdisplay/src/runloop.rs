//! The main thread's side of a process that owns virtual displays: its run loop, and the display
//! reconfiguration notice that says when to [`VirtualDisplay::enforce`](crate::VirtualDisplay)
//! again.

use core::ffi::c_void;

use objc2_core_foundation::CFRunLoop;
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayChangeSummaryFlags, CGDisplayRegisterReconfigurationCallback,
    CGError,
};

use crate::DisplayError;

/// Serve the main thread's run loop, and with it the main dispatch queue, for good.
///
/// A display is created, changed and released on the main queue, and CoreGraphics delivers its
/// reconfiguration notices through the main run loop.
pub fn park_main() -> ! {
    CFRunLoop::run();
    // The run loop returns only when it has no source at all; the main queue still needs a
    // thread to drain it.
    dispatch2::dispatch_main()
}

type Changed = Box<dyn Fn() + Send + Sync>;

/// Call `changed` after every display reconfiguration (a display added, removed, moved, put in
/// another mode or mirror set), for the life of the process, from the thread CoreGraphics
/// calls back on.
///
/// # Errors
///
/// [`DisplayError::Configure`] when CoreGraphics refuses the registration.
pub fn on_reconfiguration(changed: impl Fn() + Send + Sync + 'static) -> Result<(), DisplayError> {
    let boxed: Box<Changed> = Box::new(Box::new(changed));
    let info = Box::into_raw(boxed).cast::<c_void>();
    // SAFETY: `reconfigured` has the `CGDisplayReconfigurationCallBack` signature, and `info`
    // is leaked here, so it outlives the registration, which is never removed.
    let error = unsafe { CGDisplayRegisterReconfigurationCallback(Some(reconfigured), info) };
    if error == CGError::Success {
        Ok(())
    } else {
        Err(DisplayError::Configure(error.0))
    }
}

/// CoreGraphics calls this once before a reconfiguration (with the begin flag) and once per
/// display after it; only the after matters.
unsafe extern "C-unwind" fn reconfigured(
    _display: CGDirectDisplayID,
    flags: CGDisplayChangeSummaryFlags,
    info: *mut c_void,
) {
    if flags.0 & CGDisplayChangeSummaryFlags::BeginConfigurationFlag.0 != 0 {
        return;
    }
    // SAFETY: `info` is the `Box<Changed>` `on_reconfiguration` leaked for this callback.
    let changed = unsafe { &*info.cast::<Changed>() };
    changed();
}
