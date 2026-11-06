//! Cheap WindowServer queries the cursor channel needs: pointer position and the current
//! bounds of a display, without re-enumerating shareable content.
//!
//! Whole displays only: slopty's window geometry (window bounds, owner, on-screen state,
//! occluders, the display a window sits on) is gone with window streams.

use objc2_core_foundation::CGRect;
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventSource, CGEventSourceStateID, CGEventType,
    CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess,
};
use onicode_desktop_proto::screen::{CaptureTarget, DisplayInfo};

use crate::source::Rect;

impl Rect {
    const fn from_cg(r: CGRect) -> Self {
        Self {
            x: r.origin.x,
            y: r.origin.y,
            w: r.size.width,
            h: r.size.height,
        }
    }
}

/// Bounds of a display in global points.
#[must_use]
pub fn display_bounds(id: u32) -> Rect {
    Rect::from_cg(CGDisplayBounds(id))
}

/// A display's refresh rate in hertz, as its current mode reports it; `None` when the mode
/// reports none (some panels answer 0).
#[must_use]
pub(crate) fn display_refresh_hz(id: u32) -> Option<f64> {
    let mode = objc2_core_graphics::CGDisplayCopyDisplayMode(id)?;
    let hz = objc2_core_graphics::CGDisplayMode::refresh_rate(Some(&mode));
    (hz > 0.0).then_some(hz)
}

/// The refresh rate of the display a target is.
#[must_use]
pub(crate) fn target_refresh_hz(target: CaptureTarget) -> Option<f64> {
    let CaptureTarget::Display(id) = target;
    display_refresh_hz(id.0)
}

/// Every active display, from CoreGraphics. Unlike ScreenCaptureKit's enumeration this needs no
/// Screen Recording grant and raises no consent prompt, so a watch may read it as often as it
/// likes.
#[must_use]
pub fn active_displays() -> Vec<DisplayInfo> {
    let mut ids = [0_u32; 32];
    let mut count = 0_u32;
    let room = u32::try_from(ids.len()).unwrap_or(0);
    // SAFETY: `ids` has room for `max_displays` entries and `count` is a valid out pointer.
    let err = unsafe {
        objc2_core_graphics::CGGetActiveDisplayList(room, ids.as_mut_ptr(), &raw mut count)
    };
    if err != objc2_core_graphics::CGError::Success {
        return Vec::new();
    }
    let n = usize::try_from(count).unwrap_or(0).min(ids.len());
    ids.iter().take(n).map(|&id| display_info(id)).collect()
}

/// A display's size in points, backing scale and refresh rate.
#[must_use]
pub fn display_info(id: u32) -> DisplayInfo {
    let frame = CGDisplayBounds(id);
    let pixels_wide = objc2_core_graphics::CGDisplayPixelsWide(id);
    let scale = if frame.size.width > 0.0 {
        to_f32(f64::from(u32::try_from(pixels_wide).unwrap_or(u32::MAX)) / frame.size.width)
    } else {
        1.0
    };
    let hz = display_refresh_hz(id).unwrap_or(60.0);
    DisplayInfo {
        id: onicode_desktop_proto::DisplayId(id),
        w: to_f32(frame.size.width),
        h: to_f32(frame.size.height),
        scale,
        hz: to_f32(hz),
    }
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "geometry in points fits f32 exactly"
)]
const fn to_f32(v: f64) -> f32 {
    v as f32
}

/// The pointer's position in global display points.
#[must_use]
pub fn pointer_location() -> (f64, f64) {
    // `CGEventCreate(NULL)` snapshots the current event state, including the pointer.
    let event = CGEvent::new(None);
    let p = CGEvent::location(event.as_deref());
    (p.x, p.y)
}

/// How many pointer moves and drags the login session has seen. Wraps; compare for equality.
///
/// The number changes exactly when the pointer does, and it comes from the event system's
/// counters without asking the window server where the pointer is (MEASUREMENTS.md, "the
/// still pointer").
#[must_use]
pub fn pointer_moves() -> u32 {
    [
        CGEventType::MouseMoved,
        CGEventType::LeftMouseDragged,
        CGEventType::RightMouseDragged,
        CGEventType::OtherMouseDragged,
    ]
    .into_iter()
    .fold(0_u32, |sum, kind| {
        sum.wrapping_add(CGEventSource::counter_for_event_type(
            CGEventSourceStateID::CombinedSessionState,
            kind,
        ))
    })
}

/// Current bounds of a capture target. Always `Some` for a display; the `Option` is slopty's
/// shape, where a window could be gone.
#[must_use]
pub fn target_bounds(target: CaptureTarget) -> Option<Rect> {
    let CaptureTarget::Display(id) = target;
    Some(Rect::from_cg(CGDisplayBounds(id.0)))
}

/// Whether this process has Screen Recording (TCC) access; without it ScreenCaptureKit
/// returns no windows and every stream fails to start. Answers without prompting.
#[must_use]
pub fn can_capture() -> bool {
    CGPreflightScreenCaptureAccess()
}

/// Ask macOS for Screen Recording access; shows the system prompt once and returns the state.
///
/// Preflighting alone is not enough to get the permission: until a process *requests* it, macOS
/// neither prompts nor lists the binary under Screen Recording, so ScreenCaptureKit keeps failing
/// with `-3801` and there is nothing for anyone to switch on (`docs/decisions/input.md`).
pub fn request_capture() -> bool {
    CGRequestScreenCaptureAccess()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The displays read without ScreenCaptureKit, by a test binary that holds no Screen
    /// Recording grant: each with a size, a scale and a rate, as the stream's listing reads it.
    #[test]
    fn the_displays_are_listed_without_a_grant() {
        let displays = active_displays();
        if displays.is_empty() {
            onicode_desktop_testkit::live::skip("no display attached");
            return;
        }
        for d in &displays {
            assert!(
                d.w > 0.0 && d.h > 0.0 && d.scale >= 1.0 && d.hz > 0.0,
                "{d:?}"
            );
            assert!(
                (display_bounds(d.id.0).w - f64::from(d.w)).abs() < 0.5,
                "{d:?}"
            );
        }
    }

    #[test]
    fn rect_encloses_and_overlaps() {
        let a = Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        let inside = Rect {
            x: 2.0,
            y: 2.0,
            w: 5.0,
            h: 5.0,
        };
        let touching = Rect {
            x: 10.0,
            y: 0.0,
            w: 5.0,
            h: 5.0,
        };
        let crossing = Rect {
            x: 8.0,
            y: 8.0,
            w: 5.0,
            h: 5.0,
        };
        assert!(a.encloses(&inside));
        assert!(!a.encloses(&crossing));
        assert!(a.overlaps(&inside));
        assert!(a.overlaps(&crossing));
        assert!(!a.overlaps(&touching), "a shared edge is not an overlap");
    }

    /// What one look at the pointer costs each way: the event snapshot the cursor loop took on
    /// every tick, and the move counters it reads now. `docs/MEASUREMENTS.md` records runs.
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture in release"]
    fn pointer_read_cost() {
        let rounds = 2_000_u32;
        let started = std::time::Instant::now();
        let mut sum = 0.0_f64;
        for _ in 0..rounds {
            sum += pointer_location().0;
        }
        let location = started.elapsed() / rounds;
        let started = std::time::Instant::now();
        let mut moves = 0_u32;
        for _ in 0..rounds {
            moves = moves.wrapping_add(pointer_moves());
        }
        let counters = started.elapsed() / rounds;
        eprintln!(
            "pointer_location {location:?}, pointer_moves {counters:?} per call ({sum} {moves})"
        );
    }
}
