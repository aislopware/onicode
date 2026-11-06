//! The text field that has the keyboard on the host, as the accessibility API reads it: which
//! process, its caret's box, and whether it is a password field. The client raises its own
//! keyboard and places a hint by it.
//!
//! The rest of slopty's `ax.rs` (hearing a window go, resizing a window) went with window
//! streams.

use std::ptr::{self, NonNull};

use objc2_application_services::{AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{CFRetained, CFString, CFType};

use crate::source::{FocusedField, Rect};

// The names below are `#define kAX… CFSTR("…")` macros in the SDK's
// `HIServices/AXAttributeConstants.h`. No symbol is exported for a `CFSTR` macro, so no objc2
// static exists, and this is the one place in the workspace that spells them.

/// `kAXFocusedApplicationAttribute`: the frontmost application, asked of the system-wide
/// element.
const FOCUSED_APPLICATION: &str = "AXFocusedApplication";
/// `kAXFocusedUIElementAttribute`: the element that has the keyboard, asked of an application.
const FOCUSED_ELEMENT: &str = "AXFocusedUIElement";
/// `kAXSubroleAttribute`.
const SUBROLE_ATTRIBUTE: &str = "AXSubrole";
/// `kAXSecureTextFieldSubrole` (`HIServices/AXRoleConstants.h`): a password field.
const SECURE_TEXT_FIELD: &str = "AXSecureTextField";
/// `kAXSelectedTextRangeAttribute`: the selection, a `CFRange` in an `AXValue`.
const SELECTED_RANGE: &str = "AXSelectedTextRange";
/// `kAXBoundsForRangeParameterizedAttribute`: a range's box in global points, a `CGRect` in
/// an `AXValue`.
const BOUNDS_FOR_RANGE: &str = "AXBoundsForRange";

/// How long one read of the focused field waits for an application that does not answer: a
/// hung app costs the field's report, never the stream. Set on that application's own
/// elements; set on the system-wide element it would change every read in the process.
const FIELD_TIMEOUT_SECS: f32 = 0.1;

/// The text field that has the keyboard, with its caret's box and whether it is a password
/// field.
///
/// It is the frontmost application's focused element when that element has a selection (a
/// text field, a text view, a web page's field). `None` when nothing typed into has the
/// keyboard, or accessibility is not granted. Blocking: at most five round trips to the
/// frontmost application, each bounded by a tenth of a second.
#[must_use]
pub fn focused_field() -> Option<FocusedField> {
    // SAFETY: the documented no-argument query; it takes and returns nothing owned.
    if !unsafe { AXIsProcessTrusted() } {
        return None;
    }
    // SAFETY: the documented constructor for the system-wide element; it returns +1.
    let system = unsafe { AXUIElement::new_system_wide() };
    let app = attribute(&system, FOCUSED_APPLICATION)?
        .downcast::<AXUIElement>()
        .ok()?;
    // SAFETY: `app` is live; the timeout applies to calls on this element alone.
    unsafe {
        app.set_messaging_timeout(FIELD_TIMEOUT_SECS);
    }
    let mut pid: libc::pid_t = 0;
    // SAFETY: `app` is live and `pid` a valid out-pointer (`AXUIElementGetPid`).
    if unsafe { app.pid(NonNull::from(&mut pid)) } != AXError::Success {
        return None;
    }
    let focused = attribute(&app, FOCUSED_ELEMENT)?
        .downcast::<AXUIElement>()
        .ok()?;
    // SAFETY: as for `app`.
    unsafe {
        focused.set_messaging_timeout(FIELD_TIMEOUT_SECS);
    }
    // Only an element that holds a selection is typed into.
    let range = attribute(&focused, SELECTED_RANGE)?
        .downcast::<AXValue>()
        .ok()?;
    let secure = attribute(&focused, SUBROLE_ATTRIBUTE)
        .and_then(|subrole| subrole.downcast::<CFString>().ok())
        .is_some_and(|subrole| subrole.to_string() == SECURE_TEXT_FIELD);
    Some(FocusedField {
        pid,
        caret: caret_of(&focused, &range),
        secure,
    })
}

/// The box of the selection `range` of `field` collapsed to its start, in global points: the
/// insertion point's line. A field that gives no box, or an empty one at the origin (some
/// fields answer a zero rectangle for an empty line), gives none.
fn caret_of(field: &AXUIElement, range: &AXValue) -> Option<Rect> {
    let mut selected = objc2_core_foundation::CFRange {
        location: 0,
        length: 0,
    };
    // SAFETY: `selected` is a valid out-pointer for the type asked for (`AXValueGetValue`).
    let ok = unsafe { range.value(AXValueType::CFRange, NonNull::from(&mut selected).cast()) };
    if !ok {
        return None;
    }
    let mut start = objc2_core_foundation::CFRange {
        location: selected.location,
        length: 0,
    };
    // SAFETY: `start` outlives the call and is the type `AXValueType::CFRange` names
    // (`AXValueCreate` copies it).
    let start = unsafe { AXValue::new(AXValueType::CFRange, NonNull::from(&mut start).cast()) }?;
    let name = CFString::from_str(BOUNDS_FOR_RANGE);
    let mut value: *const CFType = ptr::null();
    // SAFETY: `field` is live, `start` is the `CFRange` value the attribute takes, and `value`
    // is a valid out-pointer for one `CFTypeRef` (`AXUIElementCopyParameterizedAttributeValue`,
    // +1 on success).
    let status = unsafe {
        field.copy_parameterized_attribute_value(&name, &start, NonNull::from(&mut value))
    };
    let value = NonNull::new(value.cast_mut())?;
    // SAFETY: a non-null result is the +1 reference the call stored.
    let value = unsafe { CFRetained::from_raw(value) };
    if status != AXError::Success {
        return None;
    }
    let value = value.downcast::<AXValue>().ok()?;
    let mut rect = objc2_core_foundation::CGRect::default();
    // SAFETY: `rect` is a valid out-pointer for the type asked for (`AXValueGetValue`).
    let ok = unsafe { value.value(AXValueType::CGRect, NonNull::from(&mut rect).cast()) };
    let empty = rect.size.height <= 0.0 && rect.origin.x == 0.0 && rect.origin.y == 0.0;
    (ok && !empty).then_some(Rect {
        x: rect.origin.x,
        y: rect.origin.y,
        w: rect.size.width,
        h: rect.size.height,
    })
}

/// One attribute of an element, or none if it will not say.
fn attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let attribute = CFString::from_str(name);
    let mut value: *const CFType = ptr::null();
    // SAFETY: `element` is live and `value` is a valid out-pointer for one `CFTypeRef`
    // (`AXUIElementCopyAttributeValue`, +1 on success).
    let status = unsafe { element.copy_attribute_value(&attribute, NonNull::from(&mut value)) };
    let value = NonNull::new(value.cast_mut())?;
    // SAFETY: a non-null result is the +1 reference the call stored.
    let value = unsafe { CFRetained::from_raw(value) };
    (status == AXError::Success).then_some(value)
}
