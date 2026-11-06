//! Screen capture for the remote-desktop host: list the displays, stream one of them as
//! pictures ready for the encoder. Whole displays only; slopty's window streams, crops and
//! regions, and the accessibility window watch, are gone.
//!
//! [`CaptureSource`] is the host's capture seam and compiles everywhere ([`source`]). On
//! macOS, `ScreenCaptureKit` implements it with ScreenCaptureKit and CoreGraphics, streaming
//! `IOSurface`-backed pixel buffers.
//!
//! `synthetic::Canvas` implements it too, drawing its own pictures instead of capturing: the
//! stand-in the frame-path measurements stream from where no Screen Recording grant exists.
//!
//! Everything is callback driven and thread-agnostic. ScreenCaptureKit runs its own queues;
//! completion handlers and frames arrive on them, and every callback type here is `Send`.
//! Frames skip the `Idle` status (nothing changed), so an unchanged screen costs nothing.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

#[cfg(target_os = "macos")]
mod content;
#[cfg(target_os = "macos")]
mod cursor;
#[cfg(target_os = "macos")]
mod focus;
#[cfg(target_os = "macos")]
mod geometry;
#[cfg(target_os = "macos")]
mod sck;
#[cfg(target_os = "macos")]
mod session;
#[cfg(target_os = "macos")]
mod snapshot;
pub mod source;
#[cfg(target_os = "macos")]
mod stream;
#[cfg(target_os = "macos")]
pub mod synthetic;

#[cfg(target_os = "macos")]
pub use content::{Shareable, enumerate};
#[cfg(target_os = "macos")]
pub use cursor::{
    AlphaAt, CursorWatch, Layout, bgra_premultiplied, cursor_seed, read_cursor, warm_cursor,
};
#[cfg(target_os = "macos")]
pub use focus::focused_field;
#[cfg(target_os = "macos")]
pub use geometry::{
    active_displays, can_capture, display_bounds, display_info, pointer_location, pointer_moves,
    request_capture, target_bounds,
};
#[cfg(target_os = "macos")]
pub use sck::ScreenCaptureKit;
#[cfg(target_os = "macos")]
pub use session::console;
#[cfg(target_os = "macos")]
pub use snapshot::{Picture, PixelOrder};
pub use source::{
    AUDIO_CHANNELS, AUDIO_RATE, AudioSink, CaptureConfig, CaptureError, CaptureSource,
    CapturedAudio, CapturedFrame, Console, DefaultImage, FocusedField, Heard, PixelFormat, Rect,
};
#[cfg(target_os = "macos")]
pub use stream::{Capture, SckDefaults, Target, host_now_us, leave_out, sck_defaults};

#[cfg(target_os = "macos")]
impl CaptureError {
    fn from_ns(error: &objc2_foundation::NSError) -> Self {
        Self::Sck {
            code: i64::try_from(error.code()).unwrap_or(i64::MIN),
            message: error.localizedDescription().to_string(),
        }
    }
}

/// Connect the process to the WindowServer once. `SCContentFilter` init calls into SkyLight,
/// which asserts (`CGS_REQUIRE_INIT`) if nothing has initialised CoreGraphics yet; that is the
/// case in a daemon whose first ScreenCaptureKit call builds a filter, not a display query.
#[cfg(target_os = "macos")]
pub(crate) fn ensure_core_graphics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _main = objc2_core_graphics::CGMainDisplayID();
    });
}
