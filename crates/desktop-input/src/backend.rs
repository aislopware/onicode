//! Where the injector's events go.
//!
//! [`Injector`](crate::Injector) decides *what* to post — the event kind, the global point,
//! the modifier flags, the route — and hands a [`Post`] to a [`Backend`]. [`System`] turns it
//! into a `CGEvent` and posts it; [`Recorder`] keeps it in a `Vec` so every decision the
//! injector makes can be asserted in a unit test without Accessibility access and without a
//! single real event reaching the desktop.

use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGEvent, CGEventField, CGEventFlags, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    CGEventType, CGKeyCode, CGMouseButton, CGScrollEventUnit,
};
use objc2_foundation::NSPoint;
use onicode_desktop_capture::Rect;
use onicode_desktop_proto::screen::{CaptureTarget, MediaKey, ScrollPhase, SwipeDirection};

use crate::InputError;
use crate::injector::SharedCaps;
use crate::keyboard::nx;

/// What every event this host posts carries in `kCGEventSourceUserData`, so the host can tell
/// its own events from the person's at its desk ("ONIC").
pub const ONICODE_EVENT: i64 = 0x4f4e_4943;

/// `kVK_Space` (`<HIToolbox/Events.h>`): the key committed text rides on, as Chrome Remote
/// Desktop's does. Never keycode 0, which some input methods read as the A key.
const TEXT_CARRIER: CGKeyCode = 0x31;

/// A media key's `data1`: the key in bits 16–31, its state in bits 8–15.
#[must_use]
pub const fn media_data1(key: MediaKey, down: bool) -> isize {
    let key = match key {
        MediaKey::PlayPause => nx::KEYTYPE_PLAY,
        MediaKey::Next => nx::KEYTYPE_NEXT,
        MediaKey::Previous => nx::KEYTYPE_PREVIOUS,
    };
    (key << 16) | ((if down { nx::KEYDOWN } else { nx::KEYUP }) << 8)
}

/// `CGScrollPhase` values (`IOKit/hidsystem/IOLLEvent.h`).
mod scroll_phase {
    pub(super) const BEGAN: i64 = 1;
    pub(super) const CHANGED: i64 = 2;
    pub(super) const ENDED: i64 = 4;
    pub(super) const CANCELLED: i64 = 8;
    pub(super) const MAY_BEGIN: i64 = 128;
}

/// The fields of a trackpad gesture's `CGEvent` (`NSEventTypeGesture`, 29), which the SDK does
/// not declare: AppKit reads the event as the kind its IOHID subtype names. The numbers are
/// the ones Mac Mouse Fix's `TouchSimulator.m` and `GestureScrollSimulator.m` and
/// Hammerspoon's `TouchEvents` post (`docs/decisions/input.md`, "Trackpad gestures reach the
/// remote app"); `a_gesture_reads_back_as_appkit_reads_the_trackpad` holds each to what AppKit
/// makes of it.
mod gesture_field {
    use objc2_core_graphics::CGEventField;

    /// The IOHID event type the gesture carries (`IOHIDEventType`).
    pub(super) const SUBTYPE: CGEventField = CGEventField(110);
    /// A pinch's change in magnification, a double.
    pub(super) const MAGNIFICATION: CGEventField = CGEventField(113);
    /// A rotation's change in degrees, anticlockwise positive, a double.
    pub(super) const ROTATION: CGEventField = CGEventField(114);
    /// A swipe's direction, the `kIOHIDSwipe…` mask.
    pub(super) const SWIPE: CGEventField = CGEventField(115);
    /// A gesture scroll's horizontal travel, a double.
    pub(super) const SCROLL_X: CGEventField = CGEventField(116);
    /// A gesture scroll's vertical travel, a double.
    pub(super) const SCROLL_Y: CGEventField = CGEventField(119);
    /// Where the gesture is: the `CGScrollPhase` values, which are IOHID's phase bits.
    pub(super) const PHASE: CGEventField = CGEventField(132);
}

/// `IOHIDEventType` values a gesture's subtype takes (`IOKit/hid/IOHIDEventTypes.h`).
mod hid_event {
    /// `kIOHIDEventTypeRotation`: AppKit's `NSEventTypeRotate`.
    pub(super) const ROTATION: i64 = 5;
    /// `kIOHIDEventTypeScroll`: the gesture a trackpad scroll comes with, which a fluid swipe
    /// (`trackSwipeEventWithOptions:`) follows.
    pub(super) const SCROLL: i64 = 6;
    /// `kIOHIDEventTypeZoom`: AppKit's `NSEventTypeMagnify`.
    pub(super) const ZOOM: i64 = 8;
    /// `kIOHIDEventTypeNavigationSwipe`: AppKit's `NSEventTypeSwipe`.
    pub(super) const NAVIGATION_SWIPE: i64 = 16;
    /// `kIOHIDEventTypeZoomToggle`: AppKit's `NSEventTypeSmartMagnify`.
    pub(super) const ZOOM_TOGGLE: i64 = 22;
}

/// The `kIOHIDSwipe…` mask for a direction (`IOKit/hid/IOHIDEventTypes.h`).
const fn swipe_mask(direction: SwipeDirection) -> i64 {
    match direction {
        SwipeDirection::Up => 1,
        SwipeDirection::Down => 2,
        SwipeDirection::Left => 4,
        SwipeDirection::Right => 8,
    }
}

/// A trackpad scroll's gesture travels this much further than the scroll's points, as Mac
/// Mouse Fix's `GestureScrollSimulator.m` scales it: what a fluid swipe measures its progress
/// in.
const GESTURE_SCROLL_SCALE: f64 = 1.67;

/// `CGMomentumScrollPhase` values.
mod momentum_phase {
    pub(super) const NONE: i64 = 0;
    pub(super) const BEGIN: i64 = 1;
    pub(super) const CONTINUE: i64 = 2;
    pub(super) const END: i64 = 3;
}

/// Where an event goes.
///
/// A whole display is the target, so everything goes through the HID tap and moves the host's
/// real pointer. Kept as a type so a [`Post`] says where it went, as the host's other routes (a
/// session tap at a lock screen, a curtain) will.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Route {
    /// The HID tap: the system, as if from real hardware.
    Hid,
}

/// One event the injector decided on, before CoreGraphics builds it.
#[derive(Clone, PartialEq, Debug)]
pub enum Event {
    /// Pointer move, drag, press or release.
    Mouse {
        /// `MouseMoved`, `LeftMouseDragged`, `LeftMouseDown`…
        kind: CGEventType,
        /// Global display point.
        at: CGPoint,
        /// The button the event is about.
        button: CGMouseButton,
        /// `MouseEventButtonNumber` (0 left, 1 right, 2 middle, 3 back, 4 forward).
        number: i64,
        /// `MouseEventClickState`; 0 for moves.
        clicks: i64,
        /// `MouseEventNumber`: the press this event belongs to, the same on the press, its
        /// drags and its release (AppKit follows a drag by it, and loses one whose numbers
        /// differ); 0 for a move with nothing held, which CoreGraphics numbers itself.
        press: i64,
    },
    /// Wheel or trackpad scroll.
    Scroll {
        /// Global display point.
        at: CGPoint,
        /// Horizontal delta as the client saw it.
        dx: f32,
        /// Vertical delta as the client saw it.
        dy: f32,
        /// Pixel (trackpad) rather than line (wheel) units.
        precise: bool,
        /// Gesture phase.
        phase: ScrollPhase,
        /// Momentum phase.
        momentum: ScrollPhase,
        /// Its timestamp on this Mac's event clock, nanoseconds of uptime; 0 for the moment it
        /// is posted.
        stamp: u64,
    },
    /// Key press, repeat or release, by position only: the target's layout makes the
    /// character.
    Key {
        /// Virtual keycode.
        vk: CGKeyCode,
        /// Press or repeat (`true`) vs release.
        down: bool,
        /// A bare modifier: posts as `FlagsChanged`.
        modifier: bool,
        /// Auto-repeat.
        repeat: bool,
    },
    /// Committed text, at most [`crate::text::MOST_UNITS`] UTF-16 units, on the Space key's
    /// press or release with `CGEventKeyboardSetUnicodeString`.
    Text {
        /// The text.
        text: String,
        /// The press (`true`) or the release.
        down: bool,
    },
    /// A media key: a system-defined event (`NSSystemDefined`, subtype 8).
    Media {
        /// Which.
        key: MediaKey,
        /// Pressed, or let go.
        down: bool,
    },
    /// A trackpad gesture: `NSEventTypeGesture` with the subtype AppKit reads as `gesture`.
    Gesture {
        /// Global display point.
        at: CGPoint,
        /// Which, and how much.
        gesture: Gesture,
        /// Where it is in its gesture; smart zoom has none.
        phase: ScrollPhase,
        /// Its timestamp on this Mac's event clock, nanoseconds of uptime; 0 for the moment it
        /// is posted.
        stamp: u64,
    },
}

/// What a [`Event::Gesture`] is.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Gesture {
    /// A pinch: the change in magnification.
    Magnify(f32),
    /// A rotation: the change in degrees, anticlockwise positive.
    Rotate(f32),
    /// Smart zoom, a two-finger double tap.
    SmartMagnify,
    /// A navigation swipe, one event: AppKit makes a swipe of every one posted.
    Swipe(SwipeDirection),
    /// The gesture a trackpad scroll comes with, in the scroll's points.
    Scroll {
        /// Horizontal travel.
        dx: f32,
        /// Vertical travel.
        dy: f32,
    },
}

/// An [`Event`] with its route and modifier flags.
#[derive(Clone, PartialEq, Debug)]
pub struct Post {
    /// Where it goes.
    pub route: Route,
    /// Modifier flags on the event.
    pub flags: CGEventFlags,
    /// The event.
    pub event: Event,
}

/// The host-side services an injector needs: display geometry, posting, and the Caps Lock
/// state.
pub trait Backend {
    /// Current bounds of the target in global display points.
    fn bounds(&mut self, target: CaptureTarget) -> Option<Rect>;
    /// Post one event.
    fn post(&mut self, post: Post) -> Result<(), InputError>;
    /// Where the host's real pointer is, in global display points; `None` from a stand-in
    /// that keeps no pointer.
    fn pointer(&mut self) -> Option<CGPoint> {
        None
    }
    /// Now on the clock events are stamped with, nanoseconds of uptime: the host time clock,
    /// which `NSEvent.timestamp` reads in seconds.
    fn event_clock(&mut self) -> u64 {
        onicode_desktop_capture::host_now_us().saturating_mul(1000)
    }
    /// Whether Caps Lock is on; `None` when the HID system would not say, as a stand-in that
    /// keeps no lock answers.
    fn caps_lock(&mut self) -> Option<bool> {
        None
    }
    /// Set Caps Lock's state, the lock itself rather than a key. A stand-in that keeps no lock
    /// takes it and does nothing.
    fn set_caps_lock(&mut self, _on: bool) -> Result<(), InputError> {
        Ok(())
    }
    /// The streams holding this backend's Caps Lock: the host's one lock, shared by every
    /// injector.
    fn caps_claims(&self) -> SharedCaps {
        std::sync::Arc::clone(&HOST_CAPS)
    }
}

/// The host's Caps Lock claim ([`crate::CapsClaims`]).
static HOST_CAPS: std::sync::LazyLock<SharedCaps> = std::sync::LazyLock::new(SharedCaps::default);

/// The real thing: CoreGraphics events through the HID tap, display bounds from CoreGraphics.
///
/// Its events come from a `CGEventSource` with its own private state, one per input thread (so
/// one per stream): the modifiers the client holds never mix with the keys the person at the
/// host holds, and theirs never leak into the client's.
#[derive(Debug, Default, Clone, Copy)]
pub struct System;

thread_local! {
    /// This thread's event source, made on its first post.
    static SOURCE: Option<CFRetained<CGEventSource>> =
        CGEventSource::new(CGEventSourceStateID::Private);
}

impl Backend for System {
    fn bounds(&mut self, target: CaptureTarget) -> Option<Rect> {
        onicode_desktop_capture::target_bounds(target)
    }

    fn post(&mut self, post: Post) -> Result<(), InputError> {
        let event = SOURCE.with(|source| build(&post, source.as_deref()))?;
        match post.route {
            Route::Hid => CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event)),
        }
        Ok(())
    }

    fn pointer(&mut self) -> Option<CGPoint> {
        let (x, y) = onicode_desktop_capture::pointer_location();
        Some(CGPoint { x, y })
    }

    fn caps_lock(&mut self) -> Option<bool> {
        hid::caps_lock()
    }

    fn set_caps_lock(&mut self, on: bool) -> Result<(), InputError> {
        hid::set_caps_lock(on)
    }
}

/// Build the `CGEvent` for a [`Post`] from `source`, tagged [`ONICODE_EVENT`].
///
/// # Errors
///
/// CoreGraphics or AppKit would not make the event.
pub fn build(
    post: &Post,
    source: Option<&CGEventSource>,
) -> Result<CFRetained<CGEvent>, InputError> {
    let event = match &post.event {
        Event::Mouse {
            kind,
            at,
            button,
            number,
            clicks,
            press,
        } => {
            let event =
                CGEvent::new_mouse_event(source, *kind, *at, *button).ok_or(InputError::Create)?;
            CGEvent::set_integer_value_field(
                Some(&event),
                CGEventField::MouseEventButtonNumber,
                *number,
            );
            if *clicks > 0 {
                CGEvent::set_integer_value_field(
                    Some(&event),
                    CGEventField::MouseEventClickState,
                    *clicks,
                );
            }
            if *press != 0 {
                CGEvent::set_integer_value_field(
                    Some(&event),
                    CGEventField::MouseEventNumber,
                    *press,
                );
            }
            event
        }
        Event::Scroll {
            at,
            dx,
            dy,
            precise,
            phase,
            momentum,
            stamp,
        } => {
            let units = if *precise {
                CGScrollEventUnit::Pixel
            } else {
                CGScrollEventUnit::Line
            };
            let (wheel1, wheel2) = (round(*dy), round(*dx));
            let event = CGEvent::new_scroll_wheel_event2(source, units, 2, wheel1, wheel2, 0)
                .ok_or(InputError::Create)?;
            CGEvent::set_location(Some(&event), *at);
            let set = |field: CGEventField, value: i64| {
                CGEvent::set_integer_value_field(Some(&event), field, value);
            };
            if *precise {
                set(CGEventField::ScrollWheelEventIsContinuous, 1);
                set(
                    CGEventField::ScrollWheelEventPointDeltaAxis1,
                    i64::from(wheel1),
                );
                set(
                    CGEventField::ScrollWheelEventPointDeltaAxis2,
                    i64::from(wheel2),
                );
                CGEvent::set_double_value_field(
                    Some(&event),
                    CGEventField::ScrollWheelEventFixedPtDeltaAxis1,
                    f64::from(*dy),
                );
                CGEvent::set_double_value_field(
                    Some(&event),
                    CGEventField::ScrollWheelEventFixedPtDeltaAxis2,
                    f64::from(*dx),
                );
            }
            set(
                CGEventField::ScrollWheelEventScrollPhase,
                scroll_phase_value(*phase),
            );
            set(
                CGEventField::ScrollWheelEventMomentumPhase,
                momentum_phase_value(*momentum),
            );
            stamped(&event, *stamp);
            event
        }
        Event::Key {
            vk,
            down,
            modifier,
            repeat,
        } => {
            let event =
                CGEvent::new_keyboard_event(source, *vk, *down).ok_or(InputError::Create)?;
            if *modifier {
                CGEvent::set_type(Some(&event), CGEventType::FlagsChanged);
            }
            if *repeat {
                CGEvent::set_integer_value_field(
                    Some(&event),
                    CGEventField::KeyboardEventAutorepeat,
                    1,
                );
            }
            event
        }
        Event::Text { text, down } => {
            let event = CGEvent::new_keyboard_event(source, TEXT_CARRIER, *down)
                .ok_or(InputError::Create)?;
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let len = u64::try_from(utf16.len()).map_err(|_too_long| InputError::Create)?;
            // SAFETY: `utf16` outlives the call and `len` is its exact length, as the function
            // requires; the event copies the string.
            unsafe {
                CGEvent::keyboard_set_unicode_string(Some(&event), len, utf16.as_ptr());
            }
            event
        }
        Event::Gesture {
            at,
            gesture,
            phase,
            stamp,
        } => {
            let event = gesture_event(source, *at, *gesture, *phase)?;
            stamped(&event, *stamp);
            event
        }
        Event::Media { key, down } => {
            let flags = NSEventModifierFlags(
                usize::try_from(post.flags.0).map_err(|_too_wide| InputError::Create)?,
            );
            NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                NSEventType::SystemDefined,
                NSPoint::ZERO,
                flags,
                0.0,
                0,
                None,
                nx::SUBTYPE_AUX_CONTROL_BUTTONS,
                media_data1(*key, *down),
                -1,
            )
            .and_then(|ns| ns.CGEvent())
            .map(CFRetained::from)
            .ok_or(InputError::Create)?
        }
    };
    if !matches!(post.event, Event::Media { .. }) {
        CGEvent::set_flags(Some(&event), post.flags);
    }
    CGEvent::set_integer_value_field(
        Some(&event),
        CGEventField::EventSourceUserData,
        ONICODE_EVENT,
    );
    Ok(event)
}

/// Give `event` the timestamp `stamp`, unless it is 0: then the system stamps it as it is
/// posted, as it does every event made with none.
fn stamped(event: &CGEvent, stamp: u64) {
    if stamp != 0 {
        CGEvent::set_timestamp(Some(event), stamp);
    }
}

/// A trackpad gesture's `CGEvent`: made blank, typed `NSEventTypeGesture`, and given the
/// subtype and the fields AppKit reads for it.
fn gesture_event(
    source: Option<&CGEventSource>,
    at: CGPoint,
    gesture: Gesture,
    phase: ScrollPhase,
) -> Result<CFRetained<CGEvent>, InputError> {
    let event = CGEvent::new(source).ok_or(InputError::Create)?;
    let gesture_type = u32::try_from(NSEventType::Gesture.0).map_err(|_wide| InputError::Create)?;
    CGEvent::set_type(Some(&event), CGEventType(gesture_type));
    CGEvent::set_location(Some(&event), at);
    let int = |field, value| CGEvent::set_integer_value_field(Some(&event), field, value);
    let double = |field, value| CGEvent::set_double_value_field(Some(&event), field, value);
    match gesture {
        Gesture::Magnify(delta) => {
            int(gesture_field::SUBTYPE, hid_event::ZOOM);
            double(gesture_field::MAGNIFICATION, f64::from(delta));
        }
        Gesture::Rotate(degrees) => {
            int(gesture_field::SUBTYPE, hid_event::ROTATION);
            double(gesture_field::ROTATION, f64::from(degrees));
        }
        Gesture::SmartMagnify => int(gesture_field::SUBTYPE, hid_event::ZOOM_TOGGLE),
        Gesture::Swipe(direction) => {
            int(gesture_field::SUBTYPE, hid_event::NAVIGATION_SWIPE);
            int(gesture_field::SWIPE, swipe_mask(direction));
        }
        Gesture::Scroll { dx, dy } => {
            int(gesture_field::SUBTYPE, hid_event::SCROLL);
            double(
                gesture_field::SCROLL_X,
                f64::from(dx) * GESTURE_SCROLL_SCALE,
            );
            double(
                gesture_field::SCROLL_Y,
                f64::from(dy) * GESTURE_SCROLL_SCALE,
            );
        }
    }
    if gesture != Gesture::SmartMagnify {
        int(gesture_field::PHASE, scroll_phase_value(phase));
    }
    Ok(event)
}

/// Caps Lock through the HID system's parameter connection (`IOHIDSetModifierLockState`,
/// `<IOKit/hidsystem/IOHIDLib.h>`): the lock itself, as a real key's press would leave it, and
/// its light.
mod hid {
    use objc2_core_foundation::{CFDictionary, CFRetained};
    use objc2_io_kit::{
        IOHIDGetModifierLockState, IOHIDSetModifierLockState, IOObjectRelease,
        IOServiceGetMatchingService, IOServiceMatching, IOServiceOpen, io_connect_t,
        kIOHIDCapsLockState, kIOHIDParamConnectType, kIOMainPortDefault,
    };

    use crate::InputError;

    /// `kIOHIDCapsLockState` as the selector argument, an `int` in `IOHIDLib.h`.
    const CAPS_LOCK: i32 = kIOHIDCapsLockState.cast_signed();

    /// The process's parameter connection to the HID system, opened on first use and kept for
    /// the process's life, so a Caps Lock change costs its call alone. A failed open is tried
    /// again next time.
    static CONNECTION: parking_lot::Mutex<Option<io_connect_t>> = parking_lot::Mutex::new(None);

    /// Open a parameter connection to the HID system.
    fn open() -> Result<io_connect_t, InputError> {
        // SAFETY: a NUL-terminated class name; the answer is a +1 dictionary or none.
        let matching = unsafe { IOServiceMatching(c"IOHIDSystem".as_ptr()) };
        // SAFETY: `kIOMainPortDefault` is IOKit's constant, written once at load.
        let main_port = unsafe { kIOMainPortDefault };
        // SAFETY: a mutable dictionary is a dictionary (CoreFoundation's own subtyping).
        let matching = matching.map(|m| unsafe { CFRetained::cast_unchecked::<CFDictionary>(m) });
        // SAFETY: the matching dictionary is consumed by the call, as IOKit documents; none
        // matches nothing.
        let service = unsafe { IOServiceGetMatchingService(main_port, matching) };
        if service == 0 {
            return Err(InputError::Create);
        }
        let mut connect = 0;
        #[expect(
            deprecated,
            reason = "libc points at the mach2 crate for this one call"
        )]
        // SAFETY: reads this task's own port, which the kernel set at process start.
        let task = unsafe { libc::mach_task_self() };
        // SAFETY: a live service, this task's own port, and an out pointer that outlives the
        // call.
        let opened =
            unsafe { IOServiceOpen(service, task, kIOHIDParamConnectType, &raw mut connect) };
        // The service reference `IOServiceGetMatchingService` handed over, released once; the
        // connection keeps what it needs.
        IOObjectRelease(service);
        if opened != 0 {
            return Err(InputError::Create);
        }
        Ok(connect)
    }

    /// The kept connection, opened now if it is not yet. The lock covers the open only; the
    /// calls on the connection, Mach messages, need none.
    fn connection() -> Result<io_connect_t, InputError> {
        let mut kept = CONNECTION.lock();
        let connect = match *kept {
            Some(connect) => connect,
            None => *kept.insert(open()?),
        };
        drop(kept);
        Ok(connect)
    }

    /// Run `f` on the kept connection.
    fn with_connection<T>(
        f: impl FnOnce(io_connect_t) -> Result<T, InputError>,
    ) -> Result<T, InputError> {
        f(connection()?)
    }

    pub(super) fn caps_lock() -> Option<bool> {
        with_connection(|connect| {
            let mut on = false;
            // SAFETY: an open parameter connection and an out pointer that outlives the call.
            let got = unsafe { IOHIDGetModifierLockState(connect, CAPS_LOCK, &raw mut on) };
            if got == 0 {
                Ok(on)
            } else {
                Err(InputError::Create)
            }
        })
        .ok()
    }

    pub(super) fn set_caps_lock(on: bool) -> Result<(), InputError> {
        with_connection(|connect| {
            let set = IOHIDSetModifierLockState(connect, CAPS_LOCK, on);
            if set == 0 {
                Ok(())
            } else {
                Err(InputError::Create)
            }
        })
    }
}

/// A fake backend for tests: fixed geometry, every post kept.
///
/// Nothing here touches CoreGraphics or AppKit, so the injector's whole decision path —
/// point mapping, drag tracking, gestures, Caps Lock, text — runs under `cargo nextest` with no
/// permissions and no side effects.
#[derive(Debug, Default, Clone)]
pub struct Recorder {
    /// What [`Backend::bounds`] answers.
    pub bounds: Option<Rect>,
    /// Calls to [`Backend::bounds`]: window-server reads, on the real backend.
    pub bounds_reads: usize,
    /// Everything posted, in order.
    pub posts: Vec<Post>,
    /// Caps Lock as it stands; `None`, the HID system would not say.
    pub caps: Option<bool>,
    /// Every Caps Lock state set, in order.
    pub locks: Vec<bool>,
    /// What [`Backend::pointer`] answers: the host's real pointer.
    pub pointer: Option<CGPoint>,
    /// What [`Backend::event_clock`] answers, nanoseconds.
    pub clock_ns: u64,
    /// The Caps Lock claim this recorder's injectors share: its own, and its clones'.
    pub caps_claims: SharedCaps,
}

impl Recorder {
    /// A display with these bounds.
    #[must_use]
    pub fn display(bounds: Rect) -> Self {
        Self {
            bounds: Some(bounds),
            ..Self::default()
        }
    }

    /// The events posted so far, without route and flags.
    #[must_use]
    pub fn events(&self) -> Vec<&Event> {
        self.posts.iter().map(|p| &p.event).collect()
    }
}

impl Backend for Recorder {
    fn bounds(&mut self, _target: CaptureTarget) -> Option<Rect> {
        self.bounds_reads = self.bounds_reads.saturating_add(1);
        self.bounds
    }

    fn post(&mut self, post: Post) -> Result<(), InputError> {
        self.posts.push(post);
        Ok(())
    }

    fn pointer(&mut self) -> Option<CGPoint> {
        self.pointer
    }

    fn event_clock(&mut self) -> u64 {
        self.clock_ns
    }

    fn caps_lock(&mut self) -> Option<bool> {
        self.caps
    }

    fn set_caps_lock(&mut self, on: bool) -> Result<(), InputError> {
        self.locks.push(on);
        self.caps = Some(on);
        Ok(())
    }

    fn caps_claims(&self) -> SharedCaps {
        std::sync::Arc::clone(&self.caps_claims)
    }
}

/// `ScrollPhase` → `CGScrollPhase`.
#[must_use]
pub const fn scroll_phase_value(phase: ScrollPhase) -> i64 {
    match phase {
        ScrollPhase::None => 0,
        ScrollPhase::Began => scroll_phase::BEGAN,
        ScrollPhase::Changed => scroll_phase::CHANGED,
        ScrollPhase::Ended => scroll_phase::ENDED,
        ScrollPhase::Cancelled => scroll_phase::CANCELLED,
        ScrollPhase::MayBegin => scroll_phase::MAY_BEGIN,
    }
}

/// `ScrollPhase` → `CGMomentumScrollPhase`.
#[must_use]
pub const fn momentum_phase_value(phase: ScrollPhase) -> i64 {
    match phase {
        ScrollPhase::None | ScrollPhase::MayBegin => momentum_phase::NONE,
        ScrollPhase::Began => momentum_phase::BEGIN,
        ScrollPhase::Changed => momentum_phase::CONTINUE,
        ScrollPhase::Ended | ScrollPhase::Cancelled => momentum_phase::END,
    }
}

const fn round(v: f32) -> i32 {
    #[expect(clippy::cast_possible_truncation, reason = "clamped")]
    let r = v.round().clamp(-1.0e6, 1.0e6) as i32;
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string an event carries.
    fn unicode(event: &CGEvent) -> String {
        let mut units = [0_u16; 32];
        let mut len = 0;
        // SAFETY: `units` holds the 32 units the call may write, `len` outlives the call.
        unsafe {
            CGEvent::keyboard_get_unicode_string(Some(event), 32, &raw mut len, units.as_mut_ptr());
        }
        String::from_utf16_lossy(&units[..usize::try_from(len).unwrap()])
    }

    fn post(event: Event, flags: CGEventFlags) -> Post {
        Post {
            route: Route::Hid,
            flags,
            event,
        }
    }

    fn private() -> CFRetained<CGEventSource> {
        CGEventSource::new(CGEventSourceStateID::Private).unwrap()
    }

    /// A key is its position, its press or release, its repeat and its flags, and carries no
    /// string, so the target's layout, dead keys and input method make the character; every
    /// event is tagged as the host's own. Built, never posted.
    #[test]
    fn keys_carry_no_unicode_string() {
        let source = private();
        let flags = CGEventFlags::MaskCommand | CGEventFlags::MaskSecondaryFn;
        let key = Event::Key {
            vk: 0x0c,
            down: true,
            modifier: false,
            repeat: true,
        };
        let event = build(&post(key, flags), Some(&source)).unwrap();
        let field = |f| CGEvent::integer_value_field(Some(&event), f);
        assert_eq!(
            field(CGEventField::KeyboardEventKeycode),
            0x0c,
            "kVK_ANSI_Q"
        );
        assert_eq!(field(CGEventField::KeyboardEventAutorepeat), 1);
        assert_eq!(field(CGEventField::EventSourceUserData), ONICODE_EVENT);
        assert_eq!(CGEvent::r#type(Some(&event)), CGEventType::KeyDown);
        assert_eq!(CGEvent::flags(Some(&event)), flags);
        assert_eq!(unicode(&event), "", "no string: the layout decides");

        let shift = Event::Key {
            vk: 0x3c,
            down: false,
            modifier: true,
            repeat: false,
        };
        let event = build(&post(shift, CGEventFlags::empty()), Some(&source)).unwrap();
        assert_eq!(CGEvent::r#type(Some(&event)), CGEventType::FlagsChanged);
        let code = CGEvent::integer_value_field(Some(&event), CGEventField::KeyboardEventKeycode);
        assert_eq!(code, 0x3c, "the right shift key");
    }

    /// Committed text rides on the Space key's press and release, carrying the whole piece.
    #[test]
    fn text_rides_on_space_with_its_string() {
        for (text, down) in [("tiếng Việt", true), ("日本語", false), ("🇻🇳", true)] {
            let piece = Event::Text {
                text: text.to_owned(),
                down,
            };
            let event = build(&post(piece, CGEventFlags::empty()), None).unwrap();
            let code =
                CGEvent::integer_value_field(Some(&event), CGEventField::KeyboardEventKeycode);
            assert_eq!(code, 0x31, "kVK_Space, never keycode 0");
            let kind = if down {
                CGEventType::KeyDown
            } else {
                CGEventType::KeyUp
            };
            assert_eq!(CGEvent::r#type(Some(&event)), kind);
            assert_eq!(unicode(&event), text);
        }
    }

    /// A media key is a system-defined event of subtype 8 whose `data1` names the key and its
    /// state, as AppKit reads it back.
    #[test]
    fn media_keys_post_system_defined_subtype_8() {
        for (key, nx) in [
            (MediaKey::PlayPause, 16),
            (MediaKey::Next, 17),
            (MediaKey::Previous, 18),
        ] {
            for (down, state) in [(true, 0x0a), (false, 0x0b)] {
                let event = build(
                    &post(Event::Media { key, down }, CGEventFlags::empty()),
                    None,
                )
                .unwrap();
                let ns = NSEvent::eventWithCGEvent(&event).unwrap();
                assert_eq!(ns.r#type(), NSEventType::SystemDefined);
                assert_eq!(ns.subtype().0, 8);
                assert_eq!(ns.data1(), (nx << 16) | (state << 8), "{key:?} {down}");
                let tag =
                    CGEvent::integer_value_field(Some(&event), CGEventField::EventSourceUserData);
                assert_eq!(tag, ONICODE_EVENT);
            }
        }
    }

    /// Each gesture, built and never posted, reads back through AppKit as the trackpad's
    /// own does: the event type an app's `-magnifyWithEvent:`, `-rotateWithEvent:`,
    /// `-smartMagnifyWithEvent:` and `-swipeWithEvent:` are called for, its amount, its phase,
    /// and a swipe's direction as `deltaX` / `deltaY`; a scroll's gesture stays a gesture. All
    /// tagged as the host's own, at the point given.
    #[test]
    fn a_gesture_reads_back_as_appkit_reads_the_trackpad() {
        use objc2_app_kit::NSEventPhase;
        let at = CGPoint { x: 120.0, y: 80.0 };
        let read = |gesture, phase| {
            let event = build(
                &post(
                    Event::Gesture {
                        at,
                        gesture,
                        phase,
                        stamp: 0,
                    },
                    CGEventFlags::empty(),
                ),
                None,
            )
            .unwrap();
            let tag = CGEvent::integer_value_field(Some(&event), CGEventField::EventSourceUserData);
            assert_eq!(tag, ONICODE_EVENT);
            assert_eq!(CGEvent::location(Some(&event)), at);
            NSEvent::eventWithCGEvent(&event).unwrap()
        };
        let pinch = read(Gesture::Magnify(0.25), ScrollPhase::Began);
        assert_eq!(pinch.r#type(), NSEventType::Magnify);
        assert!(
            (pinch.magnification() - 0.25).abs() < 1e-6,
            "{}",
            pinch.magnification()
        );
        assert_eq!(pinch.phase(), NSEventPhase::Began);
        let pinch = read(Gesture::Magnify(-0.1), ScrollPhase::Ended);
        assert!((pinch.magnification() + 0.1).abs() < 1e-6);
        assert_eq!(pinch.phase(), NSEventPhase::Ended);

        let turn = read(Gesture::Rotate(12.5), ScrollPhase::Changed);
        assert_eq!(turn.r#type(), NSEventType::Rotate);
        assert!((turn.rotation() - 12.5).abs() < 1e-4, "{}", turn.rotation());
        assert_eq!(turn.phase(), NSEventPhase::Changed);
        assert_eq!(
            read(Gesture::Rotate(1.0), ScrollPhase::Cancelled).phase(),
            NSEventPhase::Cancelled
        );

        assert_eq!(
            read(Gesture::SmartMagnify, ScrollPhase::None).r#type(),
            NSEventType::SmartMagnify
        );

        for (direction, dx, dy) in [
            (SwipeDirection::Left, 1.0, 0.0),
            (SwipeDirection::Right, -1.0, 0.0),
            (SwipeDirection::Up, 0.0, 1.0),
            (SwipeDirection::Down, 0.0, -1.0),
        ] {
            let swipe = read(Gesture::Swipe(direction), ScrollPhase::Ended);
            assert_eq!(swipe.r#type(), NSEventType::Swipe, "{direction:?}");
            assert_eq!((swipe.deltaX(), swipe.deltaY()), (dx, dy), "{direction:?}");
        }

        // A scroll's gesture stays a gesture, in each phase a trackpad's goes through. AppKit
        // has no accessor for its travel (`deltaX` and `deltaY` read 0, `gestureAmount` raises);
        // what it does with it, following a swipe between pages, is the live
        // `a_swipe_between_pages_follows_the_fingers_only_with_their_gesture`.
        for (phase, appkit) in [
            (ScrollPhase::MayBegin, NSEventPhase::MayBegin),
            (ScrollPhase::Began, NSEventPhase::Began),
            (ScrollPhase::Changed, NSEventPhase::Changed),
            (ScrollPhase::Ended, NSEventPhase::Ended),
            (ScrollPhase::Cancelled, NSEventPhase::Cancelled),
        ] {
            let scroll = read(Gesture::Scroll { dx: 3.0, dy: -2.0 }, phase);
            assert_eq!(scroll.r#type(), NSEventType::Gesture, "{phase:?}");
            assert_eq!(scroll.phase(), appkit, "{phase:?}");
        }
    }

    /// A scroll and a gesture given a timestamp carry it, in nanoseconds as `NSEvent` reads
    /// it in seconds; given none they carry none, which the system fills in as it posts them.
    #[test]
    fn a_stamped_scroll_or_gesture_carries_its_time() {
        let at = CGPoint { x: 1.0, y: 2.0 };
        let phase = ScrollPhase::Changed;
        let seconds = |stamp| {
            let scroll = Event::Scroll {
                at,
                dx: -10.0,
                dy: 0.0,
                precise: true,
                phase,
                momentum: ScrollPhase::None,
                stamp,
            };
            let gesture = Event::Gesture {
                at,
                gesture: Gesture::Scroll { dx: -10.0, dy: 0.0 },
                phase,
                stamp,
            };
            [scroll, gesture].map(|event| {
                let built = build(&post(event, CGEventFlags::empty()), None).unwrap();
                NSEvent::eventWithCGEvent(&built).unwrap().timestamp()
            })
        };
        assert!(
            seconds(0).iter().all(|s| s.abs() < f64::EPSILON),
            "left to the system"
        );
        let [scroll, gesture] = seconds(1_234_567_890_123);
        assert!((scroll - 1_234.567_890_123).abs() < 1e-9, "{scroll}");
        assert!((gesture - scroll).abs() < 1e-12, "one time for both");
    }

    #[test]
    fn phases_use_the_iokit_values() {
        assert_eq!(scroll_phase_value(ScrollPhase::Began), 1);
        assert_eq!(scroll_phase_value(ScrollPhase::Ended), 4);
        assert_eq!(scroll_phase_value(ScrollPhase::MayBegin), 128);
        assert_eq!(momentum_phase_value(ScrollPhase::Changed), 2);
        assert_eq!(momentum_phase_value(ScrollPhase::Ended), 3);
    }
}
