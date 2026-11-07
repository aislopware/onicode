//! The machine's power assertion: IOKit's `PreventUserIdleSystemSleep`.
use super::PowerAssertion;

/// This machine's assertion. Only the engine process takes it; tests use their own.
#[must_use]
pub fn system() -> Box<dyn PowerAssertion> {
    Box::new(macos::Assertion::default())
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;

    /// `kIOPMAssertionTypePreventUserIdleSystemSleep`: idle system sleep only; the display still
    /// sleeps, and a closed lid or the Sleep menu still sleeps the Mac.
    const PREVENT_IDLE_SYSTEM_SLEEP: &str = "PreventUserIdleSystemSleep";
    /// `kIOPMAssertionLevelOn`.
    const LEVEL_ON: u32 = 255;
    const SUCCESS: i32 = 0;
    /// `kCFStringEncodingUTF8`.
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: *const c_void,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external_representation: u8,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void,
            level: u32,
            name: *const c_void,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    /// A CFString, released when dropped.
    struct CfString(*const c_void);

    impl CfString {
        fn new(text: &str) -> Option<Self> {
            let length = isize::try_from(text.len()).ok()?;
            // SAFETY: `text` is valid UTF-8 for `length` bytes; a null result is handled.
            let string = unsafe {
                CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), length, UTF8, 0)
            };
            (!string.is_null()).then_some(Self(string))
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            // SAFETY: made by `CFStringCreateWithBytes` (the create rule), released once.
            unsafe { CFRelease(self.0) };
        }
    }

    /// The IOKit assertion: released on drop, and by the system when the process dies.
    #[derive(Default)]
    pub(super) struct Assertion {
        id: Option<u32>,
    }

    impl super::PowerAssertion for Assertion {
        fn take(&mut self, reason: &str) -> bool {
            if self.id.is_some() {
                return true;
            }
            let (Some(kind), Some(name)) = (
                CfString::new(PREVENT_IDLE_SYSTEM_SLEEP),
                CfString::new(reason),
            ) else {
                return false;
            };
            let mut id = 0u32;
            // SAFETY: both strings live across the call; IOKit copies what it keeps.
            let status =
                unsafe { IOPMAssertionCreateWithName(kind.0, LEVEL_ON, name.0, &raw mut id) };
            if status != SUCCESS {
                return false;
            }
            self.id = Some(id);
            true
        }

        fn release(&mut self) {
            if let Some(id) = self.id.take() {
                // SAFETY: `id` came from a successful `IOPMAssertionCreateWithName`.
                unsafe { IOPMAssertionRelease(id) };
            }
        }
    }

    impl Drop for Assertion {
        fn drop(&mut self) {
            super::PowerAssertion::release(self);
        }
    }
}
