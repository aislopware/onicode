//! The microphone, through `AVCaptureDevice`: read without a prompt, asked on a click (the
//! checklist's row, or the person starting voice).

use std::sync::Mutex;

use block2::RcBlock;
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::{NSBundle, ns_string};

use crate::Status;

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeAudio: *const AnyObject;
}

fn device() -> Option<&'static AnyClass> {
    AnyClass::get(c"AVCaptureDevice")
}

/// The app's state for the microphone (`authorizationStatusForMediaType:`), never prompting.
#[must_use]
pub fn status() -> Status {
    let Some(device) = device() else {
        return Status::Unavailable;
    };
    // SAFETY: a class method taking the audio media type and returning `NSInteger`.
    let raw: isize =
        unsafe { msg_send![device, authorizationStatusForMediaType: AVMediaTypeAudio] };
    match raw {
        0 => Status::NotAsked,
        // Restricted, denied.
        1 | 2 => Status::Denied,
        3 => Status::Granted,
        _ => Status::Unknown,
    }
}

/// Whether the bundle says why it wants the microphone. Asking without the purpose string ends
/// the process.
#[must_use]
pub fn described() -> bool {
    NSBundle::mainBundle()
        .objectForInfoDictionaryKey(ns_string!("NSMicrophoneUsageDescription"))
        .is_some()
}

/// Raises the microphone alert; `done` hears the answer, on any thread. `false`, with `done`
/// dropped unheard, when there is no `AVCaptureDevice` or no purpose string. Only from the
/// person's click.
pub fn request(done: impl FnOnce(bool) + Send + 'static) -> bool {
    let Some(device) = device() else {
        return false;
    };
    if !described() {
        return false;
    }
    // AVFoundation copies the block and may call it on any queue; the answer is taken once.
    let done = Mutex::new(Some(done));
    let completion = RcBlock::new(move |granted: Bool| {
        let taken = done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(done) = taken {
            done(granted.as_bool());
        }
    });
    // SAFETY: a class method taking the media type and a `void (^)(BOOL)` block, which it copies.
    unsafe {
        let () = msg_send![device,
            requestAccessForMediaType: AVMediaTypeAudio,
            completionHandler: &*completion
        ];
    }
    true
}
