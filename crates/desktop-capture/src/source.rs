//! The host's capture seam.
//!
//! [`CaptureSource`] and the plain types that cross it, compiled on every target. macOS
//! implements it with ScreenCaptureKit and the window server (`ScreenCaptureKit` in this
//! crate). Whole displays only: slopty's window targets, crops, regions and the accessibility
//! window watch are gone.

use onicode_desktop_proto::screen::{CaptureTarget, CursorShape, DisplayInfo};

/// Capture failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CaptureError {
    /// ScreenCaptureKit reported an error (`SCStreamErrorDomain` codes; -3801 is "user
    /// declined", i.e. no Screen Recording permission).
    #[error("ScreenCaptureKit: {message} (code {code})")]
    Sck {
        /// `NSError.code`.
        code: i64,
        /// `NSError.localizedDescription`.
        message: String,
    },
    /// The display is not in the shareable content list.
    #[error("no such capture target: {0:?}")]
    NotFound(CaptureTarget),
    /// The stream stopped on its own (display unplugged, permission revoked).
    #[error("stream stopped: {0}")]
    Stopped(String),
    /// A window this process leaves out of every display capture (`crate::leave_out`, the
    /// curtain's shield) is not in the enumeration a display was resolved against: one taken
    /// before the window was made. Enumerate again rather than show it.
    #[error("window {0}, left out of every display capture, is not in this enumeration")]
    Unlisted(u32),
}

/// A rectangle in global display points (origin top-left, y down, as `CGWindow` reports).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// Whether `(x, y)` lies inside.
    #[must_use]
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }

    /// Whether `other` lies entirely inside.
    #[must_use]
    pub fn encloses(&self, other: &Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.x + other.w <= self.x + self.w
            && other.y + other.h <= self.y + self.h
    }

    /// Whether the interiors overlap (touching edges do not count).
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

/// Pixel layout of captured frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PixelFormat {
    /// 8-bit 4:2:0 bi-planar, full range (`420f`), BT.709 matrix: what the encoder takes and
    /// the client's decoder hands its Metal surface path unconverted.
    Nv12Full,
    /// 10-bit 4:4:4 bi-planar, full range (`xf44`), BT.709 matrix: what a 4:4:4 encoder takes,
    /// and the only 4:4:4 format ScreenCaptureKit delivers (`SCStream.h`, macOS 27.0 SDK).
    /// Each sample is 16 bits, the value in the high 10; the chroma plane is full size.
    Yuv444Full10,
    /// 8-bit BGRA; for debugging and screenshots.
    Bgra,
}

/// Stream settings.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CaptureConfig {
    /// Width of the picture in pixels (even).
    pub width: u32,
    /// Height of the picture in pixels (even).
    pub height: u32,
    /// The surface's sides are the picture's rounded up to a multiple of this; the picture is
    /// drawn at its top-left and the rest is black ([`Self::surface`]). The low-latency HEVC
    /// encoder queues any picture whose sides are not multiples of 16
    /// (`docs/decisions/video.md`, "Stream sides padded to 16"). 1 or 0 pads nothing.
    pub align: u32,
    /// Frame rate ceiling; 0 captures at the display's own refresh, whatever it is
    /// (`minimumFrameInterval` = `kCMTimeZero`).
    pub fps: u16,
    /// Pixel layout.
    pub format: PixelFormat,
    /// Surfaces in ScreenCaptureKit's pool: the ones the stream and the encoder hold plus one to
    /// render the next capture into (the worker asks for 3).
    pub queue_depth: u8,
}

impl CaptureConfig {
    /// The size of the surface each frame is delivered in: the picture's sides rounded up to
    /// [`Self::align`].
    #[must_use]
    pub const fn surface(&self) -> (u32, u32) {
        (pad(self.width, self.align), pad(self.height, self.align))
    }
}

/// `side` rounded up to a multiple of `align`; `side` itself when that would overflow.
const fn pad(side: u32, align: u32) -> u32 {
    match side.checked_next_multiple_of(align) {
        Some(padded) => padded,
        None => side,
    }
}

/// Audio sample rate ScreenCaptureKit is asked for.
pub const AUDIO_RATE: u32 = 48_000;
/// Audio channels ScreenCaptureKit is asked for.
pub const AUDIO_CHANNELS: u32 = 2;

/// A run of audio from the stream: interleaved stereo float at [`AUDIO_RATE`].
#[derive(Debug)]
pub struct CapturedAudio {
    /// Presentation time on the host clock, microseconds.
    pub pts_us: u64,
    /// Interleaved L/R samples.
    pub samples: Vec<f32>,
}

/// Where a sound capture's audio goes.
pub type AudioSink = Box<dyn Fn(CapturedAudio) + Send + Sync>;

/// Whose sound a worker's one sound for a client is ([`CaptureSource::start_sound`]).
///
/// ScreenCaptureKit filters sound by application, not by window: a window's stream carries all
/// of its application's sound, and a display's every application's. So a client's streams from
/// one worker together hear the union of what each would.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Heard {
    /// Every application: a display is among the streams.
    Every,
    /// These processes' applications.
    Apps(std::collections::BTreeSet<i32>),
}

impl Heard {
    /// What `self` and `other` hear together.
    #[must_use]
    pub fn with(self, other: &Self) -> Self {
        match (self, other) {
            (Self::Every, _) | (_, Self::Every) => Self::Every,
            (Self::Apps(mut ours), Self::Apps(theirs)) => {
                ours.extend(theirs);
                Self::Apps(ours)
            }
        }
    }
}

/// What a macOS frame carries: an `IOSurface`-backed pixel buffer.
pub type DefaultImage = onicode_desktop_codec::PixelBuffer;

/// A frame from the stream.
#[derive(Debug)]
pub struct CapturedFrame<I = DefaultImage> {
    /// The picture; from a capture `IOSurface`-backed, handed straight to the encoder.
    pub image: I,
    /// Capture time on the host clock (`CMClockGetHostTimeClock`), microseconds: the
    /// sample's presentation timestamp.
    pub capture_ts_us: u64,
    /// When the window server displayed the frame (`SCStreamFrameInfoDisplayTime`), on the
    /// same clock; `None` when the attachment is missing.
    pub display_ts_us: Option<u64>,
    /// Time between the presentation timestamp and this callback, microseconds.
    pub age_us: u64,
    /// Capture latency: display time → this callback, microseconds (falls back to `age_us`
    /// without a display time). What ScreenCaptureKit itself adds.
    pub latency_us: u64,
}

/// What the Mac's screens show of the session the host runs in.
///
/// A capture of a locked Mac or of a session moved off the screens shows nothing of the
/// session's windows, and nothing the client does can change that: someone has to unlock the
/// Mac, or switch back to the session, at the Mac itself. So the client is told, and shows it
/// (slopty's video decisions, "The client is told when the Mac is locked").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Console {
    /// The session is on the screens, unlocked.
    Shown,
    /// The session is on the screens behind the lock screen.
    Locked,
    /// Another session is on the screens: the login window, or another user's session after a
    /// fast user switch.
    Away,
}

impl Console {
    /// The state a session dictionary's three flags make: whether the session is on the
    /// console, whether its login has finished, and whether its screens are locked. An absent
    /// flag reads as the unremarkable value (on the console, logged in, unlocked): the lock
    /// flag is only in the dictionary while the screens are locked.
    #[must_use]
    pub fn of(on_console: Option<bool>, login_done: Option<bool>, locked: Option<bool>) -> Self {
        if on_console == Some(false) || login_done == Some(false) {
            Self::Away
        } else if locked == Some(true) {
            Self::Locked
        } else {
            Self::Shown
        }
    }
}

/// Where a host's pictures come from: the displays it can stream and a stream of frames from
/// one of them.
///
/// A platform is a type with no values; every operation is an associated function, so a
/// stream generic over it compiles to direct calls. What ScreenCaptureKit hands back
/// (an enumeration, a resolved target, a live stream) is an associated type the host holds and
/// passes back without looking inside.
pub trait CaptureSource: 'static {
    /// A captured picture, as the encoder takes it.
    type Image: Send + 'static;
    /// Everything shareable at one moment.
    type Content: Send + Sync + 'static;
    /// A display, resolved from a [`Self::Content`].
    type Target: Send + 'static;
    /// A running capture.
    type Stream: Send + Sync + 'static;
    /// A running capture of sound alone ([`Self::start_sound`]).
    type Sound: Send + Sync + 'static;

    /// Whether this process may capture the screen.
    fn can_capture() -> bool;

    /// Enumerate the shareable displays; `done` runs once, on any thread.
    fn enumerate(done: impl FnOnce(Result<Self::Content, CaptureError>) + Send + 'static);
    /// The displays of an enumeration, for a listing.
    fn displays(content: &Self::Content) -> Vec<DisplayInfo>;
    /// Resolve `kind` against `content`.
    ///
    /// # Errors
    ///
    /// The target is not in the enumeration.
    fn resolve(content: &Self::Content, kind: CaptureTarget) -> Result<Self::Target, CaptureError>;
    /// The target's native size in pixels.
    fn pixel_size(target: &Self::Target) -> (u32, u32);
    /// The target's pixels per point.
    fn point_scale(target: &Self::Target) -> f32;

    /// Start capturing `target`'s pictures, which go to `sink` from the platform's own threads;
    /// `on_stop` hears the stream end by itself; `done` runs once it is live or has failed. A
    /// picture's stream carries no sound: a client hears a worker's through one
    /// [`Self::start_sound`].
    ///
    /// # Errors
    ///
    /// The stream could not be built.
    fn start(
        target: &Self::Target,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame<Self::Image>) + Send + Sync + 'static,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<Self::Stream, CaptureError>;
    /// Start capturing the sound of `heard` (48 kHz stereo, this process's own left out), which
    /// goes to `sink` from the platform's threads; `on_stop` hears it end by itself; `done`
    /// gets the running capture, or why there is none.
    fn start_sound(
        heard: &Heard,
        sink: AudioSink,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<Self::Sound, CaptureError>) + Send + 'static,
    );
    /// Hear `heard` on a running sound capture from now on, without stopping it.
    fn hear(
        sound: &Self::Sound,
        heard: &Heard,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    );
    /// Stop a sound capture; `done` runs once it is torn down.
    fn stop_sound(
        sound: &Self::Sound,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    );
    /// Change size, rate or format on a live stream.
    fn update(
        stream: &Self::Stream,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    );
    /// Swap what a live stream captures (another display, or the same one with other windows
    /// left out).
    fn retarget(
        stream: &Self::Stream,
        target: &Self::Target,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    );
    /// Stop; `done` runs once the stream is torn down.
    fn stop(stream: &Self::Stream, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static);
    /// Now on the clock frames are stamped with, microseconds.
    fn now_us() -> u64;
    /// What the Mac's screens show of this process's session now; `None` when it cannot be
    /// told, as on a platform with no screens of its own. Blocking: one window-server round
    /// trip.
    fn console() -> Option<Console> {
        None
    }

    /// The bounds of a display, in global points.
    fn target_bounds(target: CaptureTarget) -> Option<Rect>;
    /// The refresh rate of the display, in hertz; `None` when it
    /// cannot be read.
    fn refresh_hz(target: CaptureTarget) -> Option<f64>;
    /// A display's bounds, in global points.
    fn display_bounds(id: u32) -> Rect;
    /// A counter that moves whenever the pointer does; cheap enough to read every tick.
    fn pointer_moves() -> u32;
    /// Where the pointer is, in global points. A window-server round trip.
    fn pointer_location() -> (f64, f64);
    /// A number that moves whenever the cursor on screen changes, or `None` when there is no
    /// way to tell. Cheap enough to read every tick: no round trip.
    fn cursor_seed() -> Option<i32>;
    /// The cursor's picture at its display's scale, when there is one to read. A round trip.
    fn cursor_shape() -> Option<CursorShape>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The session dictionary's flags: off the console or not logged in is another session on
    /// the screens, whatever the lock says; otherwise the lock decides, and a flag the
    /// dictionary leaves out (the lock, whenever the screens are unlocked) reads as nothing to
    /// remark on.
    /// Every application outhears any set; two sets hear both.
    #[test]
    fn what_streams_hear_together() {
        let apps = |pids: &[i32]| Heard::Apps(pids.iter().copied().collect());
        assert_eq!(apps(&[1]).with(&apps(&[2, 1])), apps(&[1, 2]));
        assert_eq!(apps(&[1]).with(&Heard::Every), Heard::Every);
        assert_eq!(Heard::Every.with(&apps(&[3])), Heard::Every);
        assert_eq!(apps(&[]).with(&apps(&[])), apps(&[]));
    }

    #[test]
    fn the_session_flags_say_what_the_screens_show() {
        assert_eq!(Console::of(Some(true), Some(true), None), Console::Shown);
        assert_eq!(Console::of(None, None, None), Console::Shown);
        assert_eq!(
            Console::of(Some(true), Some(true), Some(false)),
            Console::Shown
        );
        assert_eq!(
            Console::of(Some(true), Some(true), Some(true)),
            Console::Locked
        );
        assert_eq!(
            Console::of(Some(false), Some(true), Some(true)),
            Console::Away
        );
        assert_eq!(Console::of(Some(false), Some(true), None), Console::Away);
        assert_eq!(Console::of(Some(true), Some(false), None), Console::Away);
    }
}
