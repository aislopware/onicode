//! System Settings as seen from outside: its process, its windows' bounds, and who is in front.

use std::collections::HashSet;
use std::ptr;

use objc2::rc::Retained;
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGRect,
};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGNullWindowID, kCGWindowBounds, kCGWindowLayer, kCGWindowName, kCGWindowNumber,
    kCGWindowOwnerPID,
};
use objc2_foundation::{NSProcessInfo, NSString};

use crate::guide::place::{Rect, SETTINGS_BUNDLE_ID, WindowInfo};

/// The running System Settings, if any.
pub(crate) fn settings_app() -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(
        SETTINGS_BUNDLE_ID,
    ))
    .firstObject()
}

/// System Settings' on-screen windows at the normal level, in CoreGraphics' space. Bounds need
/// no grant; titles come only with Screen Recording.
pub fn settings_windows() -> Vec<WindowInfo> {
    let pids: HashSet<i32> = NSRunningApplication::runningApplicationsWithBundleIdentifier(
        &NSString::from_str(SETTINGS_BUNDLE_ID),
    )
    .iter()
    .map(|app| app.processIdentifier())
    .collect();
    if pids.is_empty() {
        return Vec::new();
    }
    let Some(list) = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        kCGNullWindowID,
    ) else {
        return Vec::new();
    };
    // SAFETY: the list is documented as dictionaries keyed by the `kCGWindow*` strings.
    let list: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(list) };
    // SAFETY: framework-provided constant strings.
    let (bounds_key, pid_key, layer_key, number_key, name_key): (
        &CFString,
        &CFString,
        &CFString,
        &CFString,
        &CFString,
    ) = unsafe {
        (
            kCGWindowBounds,
            kCGWindowOwnerPID,
            kCGWindowLayer,
            kCGWindowNumber,
            kCGWindowName,
        )
    };
    list.iter()
        .filter_map(|info| {
            let number = |key: &CFString| info.get(key).and_then(|v| v.downcast::<CFNumber>().ok());
            let pid = number(pid_key)?.as_i32()?;
            if !pids.contains(&pid) || number(layer_key)?.as_i32()? != 0 {
                return None;
            }
            let window = u32::try_from(number(number_key)?.as_i64()?).ok()?;
            let rect = info.get(bounds_key)?.downcast::<CFDictionary>().ok()?;
            let mut cg = CGRect::default();
            // SAFETY: `rect` is the dictionary form of a rect; `cg` is a valid out pointer.
            let ok = unsafe {
                CGRectMakeWithDictionaryRepresentation(Some(&rect), ptr::from_mut(&mut cg))
            };
            let title = info
                .get(name_key)
                .and_then(|v| v.downcast::<CFString>().ok())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty());
            ok.then(|| WindowInfo {
                number: window,
                bounds: Rect::new(cg.origin.x, cg.origin.y, cg.size.width, cg.size.height),
                title,
            })
        })
        .collect()
}

/// The bundle id of the app in front.
pub(crate) fn frontmost_bundle() -> Option<String> {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|app| app.bundleIdentifier())
        .map(|id| id.to_string())
}

/// This Mac's macOS major version.
pub(crate) fn macos_major() -> i64 {
    i64::try_from(
        NSProcessInfo::processInfo()
            .operatingSystemVersion()
            .majorVersion,
    )
    .unwrap_or(0)
}
