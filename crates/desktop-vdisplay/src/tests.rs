//! The runtime lookup and the objects built before a display exists. Nothing here creates a
//! display: that would rearrange the screens of whoever is using this Mac (`tests/live.rs`).

use core::ffi::CStr;

use objc2::encode::EncodeReturn;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, MessageReceiver as _, Sel};
use objc2_foundation::{NSArray, NSSize};

use crate::display::{Classes, WANTED, Wanted};
use crate::{ClientKey, DisplayError, Request, VirtualDisplay, plan};

fn retina_plan() -> crate::Plan {
    plan(&Request {
        pixels: (2752, 2064),
        scale: 2.0,
        refresh_hz: 120,
        client: ClientKey::new(b"unit test"),
    })
}

#[test]
fn the_private_classes_resolve_on_this_mac() {
    Classes::resolve().unwrap();
    assert!(crate::available());
}

#[test]
fn a_missing_class_or_selector_is_unavailable() {
    let mut no_class = WANTED;
    no_class[3] = Wanted {
        class: c"CGVirtualDisplayThatIsNot",
        selectors: &[],
    };
    assert!(
        matches!(Classes::resolve_from(&no_class), Err(DisplayError::Unavailable(m)) if m.contains("no class"))
    );
    let mut no_selector = WANTED;
    no_selector[2] = Wanted {
        class: c"CGVirtualDisplaySettings",
        selectors: &[c"setNothing:"],
    };
    assert!(
        matches!(Classes::resolve_from(&no_selector), Err(DisplayError::Unavailable(m)) if m.contains("setNothing:"))
    );
}

/// A getter's value, after checking the class answers it; objc2 checks the return type in
/// debug builds.
fn get<R: EncodeReturn>(object: &AnyObject, getter: &CStr) -> R {
    let sel = Sel::register(getter);
    assert!(object.class().responds_to(sel), "{getter:?}");
    // SAFETY: the class answers `sel`, a getter the header declares returning `R`.
    unsafe { object.send_message(sel, ()) }
}

#[test]
fn the_descriptor_carries_the_plan() {
    let plan = retina_plan();
    let descriptor = Classes::resolve().unwrap().descriptor(&plan).unwrap();
    let d = &plan.descriptor;
    let ids: [u32; 3] = [c"vendorID", c"productID", c"serialNum"].map(|g| get(&descriptor, g));
    assert_eq!(ids, [d.vendor_id, d.product_id, d.serial]);
    let max: [u32; 2] = [c"maxPixelsWide", c"maxPixelsHigh"].map(|g| get(&descriptor, g));
    assert_eq!(<(u32, u32)>::from(max), d.max_pixels);
    let size: NSSize = get(&descriptor, c"sizeInMillimeters");
    assert_eq!((size.width, size.height), d.size_mm);
}

#[test]
fn the_settings_offer_the_mode_in_points_at_2x() {
    let plan = retina_plan();
    let settings = Classes::resolve().unwrap().settings(&plan.mode).unwrap();
    assert_eq!(get::<u32>(&settings, c"hiDPI"), 1);
    // SAFETY: the header declares `modes` as an `NSArray *`.
    let modes: Option<Retained<NSArray<AnyObject>>> = unsafe { msg_send![&*settings, modes] };
    let modes = modes.unwrap();
    assert_eq!(modes.len(), 1);
    let mode = modes.objectAtIndex(0);
    let size: [u32; 2] = [c"width", c"height"].map(|g| get(&mode, g));
    assert_eq!(<(u32, u32)>::from(size), plan.mode.points);
    assert!((get::<f64>(&mode, c"refreshRate") - 120.0).abs() < f64::EPSILON);
}

#[test]
fn creating_off_the_main_thread_is_refused_before_any_display_exists() {
    // libtest runs each test on its own thread; were this the main thread, `create` would make
    // a real display, so the check stands down.
    if objc2::MainThreadMarker::new().is_some() {
        return;
    }
    assert_eq!(
        VirtualDisplay::create(&retina_plan()).unwrap_err(),
        DisplayError::NotMainThread
    );
}
