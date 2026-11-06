//! The general pasteboard's access behavior, and the guide's steps for it.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSPasteboard, NSPasteboardAccessBehavior};
use objc2_foundation::NSString;

use crate::Privilege;
use crate::paste::PasteBehavior;

/// What a programmatic read of the general pasteboard does for this app now. Reading the
/// behavior never prompts.
#[must_use]
pub fn behavior() -> PasteBehavior {
    match NSPasteboard::generalPasteboard().accessBehavior() {
        NSPasteboardAccessBehavior::Ask => PasteBehavior::Ask,
        NSPasteboardAccessBehavior::AlwaysAllow => PasteBehavior::AlwaysAllow,
        NSPasteboardAccessBehavior::AlwaysDeny => PasteBehavior::AlwaysDeny,
        _ => PasteBehavior::Default,
    }
}

/// Reads the general pasteboard once, which raises the paste prompt when the app was never
/// asked, so it is listed in System Settings after. Only from the person's click: never from a
/// test, never unasked.
pub fn ask_once() {
    let text = NSString::from_str("public.utf8-plain-text");
    let _read = NSPasteboard::generalPasteboard().dataForType(&text);
}

/// Opens System Settings at Paste from Other Apps and, given the main thread, brings it forward,
/// as the drag guide does. `false` when the link did not open.
pub fn open_settings(mtm: Option<MainThreadMarker>) -> bool {
    let opened = super::open_settings(Privilege::Pasteboard);
    if opened && let Some(mtm) = mtm {
        let _front = super::activate_settings(mtm);
    }
    opened
}
