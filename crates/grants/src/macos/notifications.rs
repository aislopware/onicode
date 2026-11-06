//! Notifications, through `UNUserNotificationCenter`: read without a prompt
//! (`getNotificationSettingsWithCompletionHandler:`), asked on a click
//! (`requestAuthorizationWithOptions:completionHandler:`).
//!
//! The center throws for a process outside an app bundle (`cargo run`), so both answer
//! [`Status::Unavailable`] there without touching it.

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::NSBundle;

use crate::Status;

/// How long the center has to answer a read.
const READ_WAIT: Duration = Duration::from_secs(2);
/// How long the person has to answer the alert before the request stops waiting.
const ASK_WAIT: Duration = Duration::from_secs(120);
/// `UNAuthorizationOptionBadge | Sound | Alert`.
const OPTIONS: usize = 1 | 2 | 4;

#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}

/// The current center, when this process is an app bundle with an id.
fn center() -> Option<Retained<AnyObject>> {
    let bundle = NSBundle::mainBundle();
    let in_app =
        bundle.bundleIdentifier().is_some() && bundle.bundlePath().to_string().ends_with(".app");
    if !in_app {
        return None;
    }
    let class = AnyClass::get(c"UNUserNotificationCenter")?;
    // SAFETY: `+currentNotificationCenter` returns the shared center.
    unsafe { msg_send![class, currentNotificationCenter] }
}

/// The app's notification authorization, never prompting.
#[must_use]
pub(super) fn status() -> Status {
    let Some(center) = center() else {
        return Status::Unavailable;
    };
    let (tx, rx) = mpsc::channel::<isize>();
    let completion = RcBlock::new(move |settings: *mut AnyObject| {
        // SAFETY: the center hands a live `UNNotificationSettings` for the block's duration.
        let Some(settings) = (unsafe { settings.as_ref() }) else {
            return;
        };
        // SAFETY: `authorizationStatus` returns `NSInteger`.
        let raw: isize = unsafe { msg_send![settings, authorizationStatus] };
        let _sent = tx.send(raw);
    });
    // SAFETY: takes a `void (^)(UNNotificationSettings *)` block, which it copies.
    unsafe {
        let () = msg_send![&*center, getNotificationSettingsWithCompletionHandler: &*completion];
    }
    match rx.recv_timeout(READ_WAIT) {
        Ok(0) => Status::NotAsked,
        Ok(1) => Status::Denied,
        // Authorized, provisional, ephemeral.
        Ok(2..=4) => Status::Granted,
        _ => Status::Unknown,
    }
}

/// Raises the notifications alert and waits for the answer. Only from the person's click.
pub(super) fn request() {
    let Some(center) = center() else {
        return;
    };
    let (tx, rx) = mpsc::channel::<bool>();
    let completion = RcBlock::new(move |granted: Bool, _error: *mut AnyObject| {
        let _sent = tx.send(granted.as_bool());
    });
    // SAFETY: takes `UNAuthorizationOptions` and a `void (^)(BOOL, NSError *)` block, which it
    // copies.
    unsafe {
        let () = msg_send![&*center,
            requestAuthorizationWithOptions: OPTIONS,
            completionHandler: &*completion
        ];
    }
    let _answer = rx.recv_timeout(ASK_WAIT);
}
