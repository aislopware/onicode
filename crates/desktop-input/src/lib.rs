//! Remote-desktop input on the host: turn a client's [`ScreenInput`] into `CGEvent`s.
//!
//! One `Injector` per display stream. It knows the stream's display and its pixels-per-point
//! scale, maps stream pixels back to global display points, and posts each event through the
//! HID event tap: the whole display is the target, so the real pointer follows. Posting itself
//! is a `Backend`: `System` for the host, `Recorder` for tests, so every decision here is
//! unit-tested without Accessibility access and without a real event. Posting needs the host
//! process to be granted *Accessibility* (post-event access); `can_post` and `request_post`
//! wrap the preflight and prompt.
//!
//! Keys go by position and carry no text: the host's layout makes the character, the client's
//! own once the host has taken the client's input source ([`sources`]). Text the client
//! composed itself goes as its own event, in pieces a key event can carry (`text`). Caps Lock
//! is set as a lock, and media keys go to the system as system-defined events. Trackpad
//! gestures (a pinch, a rotation, smart zoom, a swipe, and, while the view sends its gestures,
//! the gesture a trackpad scroll comes with) have no public `CGEvent` constructor: they are
//! built as the trackpad's are, from a blank event and the fields AppKit reads (`backend`). A
//! press, its drags and its release carry one event number, as AppKit follows a drag by it.
//!
//! [`InputSink`] is the host's input seam, compiled on every target; `CgEvents` is the macOS
//! implementation: the stream's `Injector` on an `InputThread`, so no window-server call is
//! made on the stream's task. The display's bounds come from the stream's geometry probe
//! ([`InputSink::set_bounds`]), and where the input put the pointer goes back to the stream's
//! cursor samples ([`PointerWatch`]). The clipboard is `onicode-clipboard`'s.
//!
//! The keyboard pieces both ends share live here too: the host's input source by its TIS id
//! (`input_source`, macOS), and the client's raw keys taken ahead of its text system with
//! the modifier and media-key constants both ends read and write ([`keyboard`]).
//!
//! Ported from slopty's `slopty-input` and the keyboard half of `slopty-platform` (MIT, same
//! authors). Window streams are gone: one stream is one whole display, so every event goes
//! through the HID tap and nothing is activated, bound to a window or posted to a pid. Drag
//! and drop of files between the two ends comes later, with its own seam.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

#[cfg(target_os = "macos")]
pub mod backend;
pub mod chords;
#[cfg(target_os = "macos")]
mod injector;
#[cfg(target_os = "macos")]
pub mod input_source;
pub mod keyboard;
#[cfg(target_os = "macos")]
pub mod keymap;
mod pointer;
pub mod sources;
#[cfg(target_os = "macos")]
pub mod text;
#[cfg(target_os = "macos")]
mod thread;

use std::time::Instant;

#[cfg(target_os = "macos")]
pub use backend::{Backend, Event, Gesture, Post, Recorder, Route, System};
#[cfg(target_os = "macos")]
pub use injector::{
    CapsClaims, CapsKept, Injector, SharedCaps, can_post, flags_for, keep_caps, request_post,
    to_point,
};
use onicode_desktop_capture::Rect;
use onicode_desktop_proto::screen::{CaptureTarget, ScreenInput};
pub use pointer::{Pointer, PointerChanges, PointerWatch};
#[cfg(target_os = "macos")]
pub use thread::{CgEvents, InputThread, let_go_everywhere};

/// Raise the calling thread to the user-interactive QoS class: every click and key into the
/// host's display is posted from one, so a busy Mac never queues input behind a background
/// thread.
///
/// The class decides which threads the scheduler runs first when every core is taken: under an
/// all-core spin at `USER_INITIATED`, slopty's host's share of an echo went from p90 5–6 ms
/// unclassed to 0.2–0.5 ms classed (slopty MEASUREMENTS, "the keystroke path under an all-core
/// spin"). Never called on a thread that is already realtime (audio) or on the main thread of an
/// app, which the system runs user-interactive already.
#[cfg(target_os = "macos")]
pub fn user_interactive_thread() {
    // SAFETY: `pthread_set_qos_class_self_np` (pthread/qos.h) changes only the calling thread's
    // own class, and a relative priority of 0 is within every class's range
    // (`QOS_MIN_RELATIVE_PRIORITY` is -15).
    let refused = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
    };
    if refused != 0 {
        tracing::warn!(error = refused, "user-interactive QoS refused");
    }
}

/// Raise the calling thread's QoS class: no classes off macOS.
#[cfg(not(target_os = "macos"))]
pub const fn user_interactive_thread() {}

/// What went wrong posting an event.
#[derive(Clone, Copy, thiserror::Error, Debug)]
pub enum InputError {
    /// The target display is gone (no bounds for it).
    #[error("target has no bounds; display gone?")]
    NoBounds,
    /// `CGEventCreate*` returned null.
    #[error("CGEvent creation failed")]
    Create,
    /// The stream's input thread is gone.
    #[error("input thread stopped")]
    Stopped,
    /// This platform injects no input: the host is a Mac.
    #[error("input injection is unsupported on this platform")]
    Unsupported,
}

/// Where a screen stream's client input goes: the host's input seam.
///
/// One per stream, owned by the stream's task. It maps the stream's pixels back to the
/// display's place on the host's screens and delivers the event to it. The calls return
/// without waiting on the window server; an implementation that must wait does so elsewhere.
pub trait InputSink: Send + Sized + 'static {
    /// A sink for `target`, whose stream has `scale` pixels per display point.
    fn new(target: CaptureTarget, scale: f64) -> Self;
    /// The stream was re-scaled: `scale` stream pixels per display point from now on.
    fn set_scale(&mut self, scale: f64);
    /// The display's bounds in global display points as read at `at` (`None`: the display is
    /// gone): what the pointer events after this one map through. The stream's geometry probe
    /// hands every read over, so the sink need not read them in front of an event.
    fn set_bounds(&mut self, bounds: Option<Rect>, at: Instant);
    /// Deliver one input event.
    ///
    /// # Errors
    ///
    /// The target is gone, or the system would not take the event.
    fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError>;
    /// Give the target keyboard focus. A whole display has it already: the host's own front
    /// app takes the keys, as it would from a keyboard there, so nothing is asked.
    ///
    /// # Errors
    ///
    /// An implementation whose target must be asked, when it would not take focus.
    fn focus(&mut self) -> Result<(), InputError>;
    /// Let go of every key and button held down through this sink: the stream is ending.
    /// Dropping the sink does the same.
    fn release_all(&mut self);
    /// Where this sink's input puts the host's pointer, readable from any thread.
    fn pointer(&self) -> PointerWatch;
}
