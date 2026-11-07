//! Remote displays: listing, stream setup, input, events.
//!
//! Ported from slopty's `slopty-proto::screen` (MIT, same authors). Whole displays only: window
//! streams, regions and crops are gone, and so are drags (file drag is later). The receiver
//! report, the rate verdict, the source state, the codec and the chroma are the media wire's
//! ([`onicode_desktop_media::wire`]), re-exported here; loss feedback and clock probes travel as
//! that wire's `Feedback` datagrams on channel `FEEDBACK`.

pub use onicode_desktop_media::wire::{
    Chroma, RateVerdict, ReceiverReport, SourceState, StreamId, VideoCodec,
};
use serde::{Deserialize, Serialize};

use crate::DisplayId;
use crate::input::{KeyAction, KeyCode, Mods, MouseButton};

/// A display on the worker.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// Identity.
    pub id: DisplayId,
    /// Bounds in points.
    pub w: f32,
    /// Bounds.
    pub h: f32,
    /// Backing scale.
    pub scale: f32,
    /// Refresh rate.
    pub hz: f32,
}

/// What to stream: always a whole display.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum CaptureTarget {
    /// A whole display.
    Display(DisplayId),
}

/// A client's own key for the virtual display a worker makes for it.
///
/// Sixteen random bytes the client draws once and keeps on the device, so the display has the
/// same identity every time and macOS finds the arrangement and mode it stored for it last time.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DisplayKey(pub [u8; 16]);

/// The display a client wants a worker to make for it: the tile's drawable in pixels, the
/// backing scale of the screen the tile is on, and that screen's fastest refresh.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct DisplayShape {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Backing scale (1 for a standard screen, 2 for Retina, 3 for an iPhone).
    pub scale: f32,
    /// The screen's maximum refresh, hertz; 0 when it is not known.
    pub refresh_hz: u16,
}

/// What a stream opened with [`ScreenRequest::OpenDisplay`] shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum VirtualDisplay {
    /// A display made for this client, at its shape.
    Made(DisplayId),
    /// A physical display, because no virtual one could be had.
    Physical {
        /// The display streamed instead.
        display: DisplayId,
        /// Why.
        why: NoVirtualDisplay,
    },
}

/// Why a worker streams a physical display where a client asked for one of its own.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum NoVirtualDisplay {
    /// This worker cannot make one (not macOS, or macOS without the classes it needs).
    Unavailable,
    /// macOS declined to create it or to take its mode.
    Refused,
    /// It never settled in its mode.
    Unsettled,
    /// ScreenCaptureKit never listed it.
    Unlisted,
}

/// One horizontal stripe of a striped stream: a session of its own on the worker, coding its
/// rows of every capture (`docs/decisions/video.md`, "Two stripes halve the encode at 3K and
/// above").
///
/// A stream larger than one encode engine's pixel rate is coded as two stripes on the worker's
/// two engines at once. Each stripe is its own media stream, with its own frames, reassembly,
/// NACKs, refreshes and reports, on [`Self::media`]. It codes [`Self::coded_rows`] rows of the
/// picture from [`Self::coded_top`], past the seam into its neighbour's rows so the seam's rows
/// keep their prediction, and shows only [`Self::shown_rows`] rows from [`Self::shown_top`].
/// Every stripe of one capture carries that capture's `capture_ts_us`, and its frame prefix names
/// the stripes coded from it ([`onicode_desktop_media::wire::FramePrefix::stripes`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Stripe {
    /// The media stream the stripe's datagrams come on ([`Self::media_of`]).
    pub media: StreamId,
    /// The first row of the picture the stripe's session codes.
    pub coded_top: u32,
    /// Rows the stripe's session codes: the height of the pictures it decodes to.
    pub coded_rows: u32,
    /// The first row of the picture the stripe shows.
    pub shown_top: u32,
    /// Rows of the picture the stripe shows.
    pub shown_rows: u32,
}

impl Stripe {
    /// The most stripes a stream is coded as: one per encode engine of an Apple silicon Mac,
    /// and four were never better than two (MEASUREMENTS.md, "stripes across the two encode
    /// engines").
    pub const MAX: usize = 2;

    /// The media stream of stripe `index` of `stream`: the stream's own id for the top stripe,
    /// and the id with its top bit set for the one under it. A receiver routes a stripe's
    /// datagrams to its stream by the id alone ([`Self::stream_of`]), before it has read the
    /// event that names the stripe: a stream's first datagrams often beat its `Opened`.
    #[must_use]
    pub const fn media_of(stream: StreamId, index: usize) -> StreamId {
        if index == 0 {
            stream
        } else {
            StreamId(stream.0 | STRIPE_BIT)
        }
    }

    /// The stream a stripe's media stream belongs to, and the stripe's index in it.
    #[must_use]
    pub const fn stream_of(media: StreamId) -> (StreamId, usize) {
        if media.0 & STRIPE_BIT == 0 {
            (media, 0)
        } else {
            (StreamId(media.0 & !STRIPE_BIT), 1)
        }
    }

    /// Rows of the stripe's own picture above the ones it shows: the rows it codes past the
    /// seam above it, 0 for the top stripe.
    #[must_use]
    pub const fn shown_from(&self) -> u32 {
        self.shown_top.saturating_sub(self.coded_top)
    }
}

/// The bit of a stripe's media stream id that says it is the lower stripe of the stream the
/// other bits name. A worker numbers its streams upwards from 1 and never reaches it.
const STRIPE_BIT: u32 = 1 << 31;

/// Stream quality request.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Quality {
    /// Target frames per second.
    pub fps: u16,
    /// Target average bitrate, bits per second.
    pub bitrate_bps: u32,
    /// Scale factor applied to the capture (1.0 = native points × scale).
    pub scale: f32,
    /// Preferred codec.
    pub codec: VideoCodec,
    /// The most colour the client wants. [`Chroma::Full`] is an ask, not an order: the worker
    /// streams 4:4:4 only while its bitrate target is high enough for 4:4:4 to beat 4:2:0, and
    /// 4:2:0 below that (`docs/decisions/video.md`, "Full chroma follows the rate"). The client
    /// learns which from the stream itself: its decoder follows the SPS.
    pub chroma: Chroma,
}

impl Default for Quality {
    fn default() -> Self {
        Self {
            fps: 60,
            bitrate_bps: 30_000_000,
            scale: 1.0,
            codec: VideoCodec::Hevc,
            chroma: Chroma::Subsampled,
        }
    }
}

/// Input aimed at a streamed display, in the stream's pixel coordinates.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ScreenInput {
    /// Pointer moved.
    Move {
        /// X in stream pixels.
        x: f32,
        /// Y.
        y: f32,
    },
    /// Button.
    Button {
        /// Which.
        button: MouseButton,
        /// Down or up.
        down: bool,
        /// Position.
        x: f32,
        /// Position.
        y: f32,
        /// Click count (double-click detection is client-side, like a real mouse).
        clicks: u8,
        /// Modifiers.
        mods: Mods,
    },
    /// Scroll with trackpad phases so the worker can synthesise momentum-faithful events.
    Scroll {
        /// Pixel delta x.
        dx: f32,
        /// Pixel delta y.
        dy: f32,
        /// Precise (trackpad) vs. line (wheel).
        precise: bool,
        /// Gesture phase.
        phase: ScrollPhase,
        /// Momentum phase.
        momentum: ScrollPhase,
        /// Position.
        x: f32,
        /// Position.
        y: f32,
        /// Modifiers.
        mods: Mods,
        /// When the client read it: see [`Self::time_us`].
        time_us: u32,
    },
    /// A key by its position on the keyboard, never by its character: the worker posts the
    /// position and its own keyboard layout makes the character, composing dead keys and input
    /// methods in the remote app (`docs/decisions/input.md`, "Keys go by position"). A ⌘ chord
    /// is the key at the same place, which is the same shortcut while both Macs run one layout.
    Key {
        /// Physical key.
        code: KeyCode,
        /// Action.
        action: KeyAction,
        /// Modifiers, with their side when the client knows it.
        mods: Mods,
    },
    /// A trackpad pinch, for the app under the pointer (`NSEventTypeMagnify`): the worker posts
    /// it as the trackpad would (`docs/decisions/input.md`, "Trackpad gestures reach the remote
    /// app").
    Magnify {
        /// The change in magnification since the last event of the pinch, as
        /// `NSEvent.magnification` reads it (0.1 is ten percent larger).
        delta: f32,
        /// Where the pinch is in its gesture.
        phase: ScrollPhase,
        /// Where the fingers are, in stream pixels.
        x: f32,
        /// Where the fingers are.
        y: f32,
        /// When the client read it: see [`Self::time_us`].
        time_us: u32,
    },
    /// Caps Lock's state on the client: the worker sets its own lock to it. Sent when the tile
    /// takes the keyboard and whenever it changes, never as a key.
    Lock {
        /// Caps Lock is on.
        caps: bool,
    },
    /// A media key: the worker's Now Playing app takes it. Volume keys stay on the client,
    /// where the stream's sound plays.
    Media {
        /// Which.
        key: MediaKey,
        /// Pressed, or let go.
        down: bool,
    },
    /// A trackpad rotation, for the app under the pointer (`NSEventTypeRotate`).
    Rotate {
        /// The change in angle since the last event of the rotation, degrees, anticlockwise
        /// positive, as `NSEvent.rotation` reads it.
        degrees: f32,
        /// Where the rotation is in its gesture.
        phase: ScrollPhase,
        /// Where the fingers are, in stream pixels.
        x: f32,
        /// Where the fingers are.
        y: f32,
        /// When the client read it: see [`Self::time_us`].
        time_us: u32,
    },
    /// Smart zoom, a two-finger double tap, for the app under the pointer
    /// (`NSEventTypeSmartMagnify`): the app zooms to what is under it, or back.
    SmartMagnify {
        /// Where the fingers tapped, in stream pixels.
        x: f32,
        /// Where the fingers tapped.
        y: f32,
    },
    /// A discrete swipe, for the app under the pointer (`NSEventTypeSwipe`, what
    /// `-swipeWithEvent:` takes: three fingers, or two where the Mac is set to swipe pages with
    /// two and the app does not track the scroll itself). A two-finger swipe that an app tracks
    /// as it moves, as Safari's back and forward do, is a [`Self::Scroll`] with its phases.
    Swipe {
        /// Which way.
        direction: SwipeDirection,
        /// Where the pointer is, in stream pixels.
        x: f32,
        /// Where the pointer is.
        y: f32,
    },
    /// Whether the tile sends its trackpad gestures to the remote app (`true`) or keeps them
    /// (`false`, how a stream starts). While it sends them, a trackpad scroll comes with the
    /// gesture a trackpad's does, which is what lets an app follow a two-finger swipe between
    /// pages as it moves (`docs/decisions/input.md`, "Trackpad gestures reach the remote app").
    Gestures {
        /// Sent to the remote app.
        remote: bool,
    },
}

/// Which way a [`ScreenInput::Swipe`] went.
///
/// Named as the trackpad's swipe mask names it (`kIOHIDSwipeLeft` and its siblings,
/// `IOKit/hid/IOHIDEventTypes.h`) and read by the app as `NSEvent.deltaX` / `deltaY`: a left
/// swipe reads `deltaX` 1, a right one −1, an up swipe `deltaY` 1, a down one −1.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum SwipeDirection {
    /// `deltaX` 1.
    Left,
    /// `deltaX` −1.
    Right,
    /// `deltaY` 1.
    Up,
    /// `deltaY` −1.
    Down,
}

/// A media key the worker takes (`NX_KEYTYPE_*`, `<IOKit/hidsystem/ev_keymap.h>`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MediaKey {
    /// Play or pause.
    PlayPause,
    /// Next track.
    Next,
    /// Previous track.
    Previous,
}

impl ScreenInput {
    /// Whether this input applies only in its turn. A move sets where the pointer is, so a
    /// newer one may overtake an older one that has not arrived; everything else is an event
    /// the target sees once, in order (a key, a button, a scroll's delta, a pinch's or a
    /// rotation's, a tap, a swipe).
    #[must_use]
    pub const fn in_order(&self) -> bool {
        !matches!(self, Self::Move { .. })
    }

    /// When the client read a scroll, a pinch or a rotation, in microseconds on the client's
    /// own monotonic clock, the low 32 bits (they wrap every 71 minutes, and only the spacing
    /// between two of one gesture counts); `None` for the rest.
    ///
    /// The app the gesture reaches judges it by its events' timestamps: whether a swipe between
    /// pages turns the page on lifting, and how fast a pinch was going. Stamped as it is posted,
    /// an event carries the path's delay with it, so a report held up by the network or the
    /// worker's scheduler and then posted with the next turns a steady swipe into a stop and a
    /// jerk; the worker instead posts each at the client's spacing (`docs/decisions/input.md`,
    /// "A gesture's events keep the client's spacing").
    #[must_use]
    pub const fn time_us(&self) -> Option<u32> {
        match self {
            Self::Scroll { time_us, .. }
            | Self::Magnify { time_us, .. }
            | Self::Rotate { time_us, .. } => Some(*time_us),
            _ => None,
        }
    }
}

/// Trackpad gesture phase, mirroring `NSEventPhase`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
pub enum ScrollPhase {
    /// Not in a gesture.
    #[default]
    None,
    /// Fingers touched.
    Began,
    /// Moving.
    Changed,
    /// Fingers lifted.
    Ended,
    /// Cancelled.
    Cancelled,
    /// May begin.
    MayBegin,
}

/// Client → worker.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ScreenRequest {
    /// Enumerate displays.
    List,
    /// Start streaming.
    Open {
        /// What.
        target: CaptureTarget,
        /// How.
        quality: Quality,
    },
    /// Stop.
    Close(StreamId),
    /// Change quality on a live stream.
    SetQuality {
        /// Stream.
        stream: StreamId,
        /// New quality.
        quality: Quality,
    },
    /// Input.
    Input {
        /// Stream.
        stream: StreamId,
        /// Event.
        input: ScreenInput,
    },
    /// Periodic receiver report (every ~50 ms while a stream is open).
    Report {
        /// Stream.
        stream: StreamId,
        /// Report.
        report: ReceiverReport,
    },
    /// Give the display made for the client this size on the host, in the stream's native
    /// pixels (what `Opened` / `Geometry` report). A physical display stream ignores it; the worker
    /// answers, when the target did change, with a `Geometry` event from its geometry poll.
    Resize {
        /// Stream.
        stream: StreamId,
        /// Wanted width in native pixels.
        width: u32,
        /// Wanted height in native pixels.
        height: u32,
        /// The backing scale of the screen the tile is on now, for a display made for the
        /// client; `None` keeps the one it has.
        scale: Option<f32>,
    },
    /// Make a display sized to the client and stream it; the worker answers with
    /// [`ScreenEvent::Display`] and then `Opened`. Later sizes and scales come as
    /// [`ScreenRequest::Resize`].
    OpenDisplay {
        /// The client's key: one display per key.
        key: DisplayKey,
        /// Its size, scale and refresh.
        shape: DisplayShape,
        /// How.
        quality: Quality,
    },
    /// Draw the curtain over the worker's Mac, or let it go: its physical screens show a black
    /// shield that no capture shows, for as long as the controller holds it. Only the
    /// controller may draw it; it lifts when the controller lets go of it or of the controls,
    /// or goes. Answered, to every client, with [`ScreenEvent::Curtain`].
    Curtain {
        /// Hold it, or let it go.
        on: bool,
    },
}

impl ScreenRequest {
    /// The stream this request is numbered for, and whether it applies only in its turn, when
    /// it is one both ends number (an input datagram copy): input, and
    /// a quality change, which sets the scale the input after it is mapped at.
    #[must_use]
    pub const fn numbered(&self) -> Option<(StreamId, bool)> {
        match self {
            Self::Input { stream, input } => Some((*stream, input.in_order())),
            Self::SetQuality { stream, .. } => Some((*stream, true)),
            Self::List
            | Self::Open { .. }
            | Self::OpenDisplay { .. }
            | Self::Close(_)
            | Self::Report { .. }
            | Self::Curtain { .. }
            | Self::Resize { .. } => None,
        }
    }
}

/// Cursor appearance, sent when it changes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CursorShape {
    /// Width in pixels.
    pub w: u16,
    /// Height in pixels.
    pub h: u16,
    /// Hotspot x.
    pub hot_x: u16,
    /// Hotspot y.
    pub hot_y: u16,
    /// Premultiplied BGRA pixels, written as one byte string.
    #[serde(with = "serde_bytes")]
    pub bgra: Vec<u8>,
    /// Backing scale of the pixels.
    pub scale: u8,
}

/// Worker → client.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum ScreenEvent {
    /// Reply to `List`.
    Listing {
        /// Displays.
        displays: Vec<DisplayInfo>,
    },
    /// Streaming started.
    Opened {
        /// Stream.
        stream: StreamId,
        /// Target.
        target: CaptureTarget,
        /// Codec in use.
        codec: VideoCodec,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// Points-to-pixels scale.
        scale: f32,
        /// How the video is coded: empty for one picture on `stream`, else its stripes, top
        /// first, each on a media stream of its own ([`Stripe`]).
        stripes: Vec<Stripe>,
    },
    /// Stream ended.
    Closed {
        /// Stream.
        stream: StreamId,
        /// Why.
        reason: String,
    },
    /// The target was resized, or the stream turned its stripes on or off: the video that
    /// follows is this size, coded this way.
    Geometry {
        /// Stream.
        stream: StreamId,
        /// Pixel width.
        width: u32,
        /// Pixel height.
        height: u32,
        /// How the video is coded from now on, as in `Opened`.
        stripes: Vec<Stripe>,
    },
    /// The cursor image changed.
    Cursor {
        /// Stream.
        stream: StreamId,
        /// Shape, or `None` when hidden.
        shape: Option<CursorShape>,
    },
    /// The capture target started or stopped producing pictures. Sent when the state changes,
    /// so a client that never gets one treats the stream as `Live` (the old behaviour).
    Source {
        /// Stream.
        stream: StreamId,
        /// What the target is doing.
        state: SourceState,
    },
    /// The bitrate controller decided (about twice a second per stream).
    Rate {
        /// Stream.
        stream: StreamId,
        /// Encoder target after the decision, bits per second.
        target_bps: u32,
        /// What the decision was.
        verdict: RateVerdict,
        /// The QUIC congestion window, not the verdict, is what holds the target down.
        capped: bool,
    },
    /// What a stream opened with [`ScreenRequest::OpenDisplay`] shows: sent before its `Opened`
    /// (whose target is then that display), and again whenever it changes.
    Display {
        /// Stream.
        stream: StreamId,
        /// The key the client asked with.
        key: DisplayKey,
        /// The display it got.
        display: VirtualDisplay,
    },
    /// An [`ScreenRequest::Open`] or [`ScreenRequest::OpenDisplay`] that made no stream. It
    /// names what was asked, since the client learns a stream's id only from its `Opened`.
    OpenFailed {
        /// What was asked for.
        asked: OpenAsk,
        /// Why it did not open.
        why: ScreenFailure,
    },
    /// A [`ScreenRequest::List`] the worker could not answer with a [`Self::Listing`].
    ListFailed {
        /// Why.
        why: ScreenFailure,
    },
    /// Where the curtain over the worker's Mac stands: to every client linked whenever it
    /// changes, and to one that asks ([`ScreenRequest::Curtain`]).
    Curtain(CurtainState),
}

/// Where the curtain over a worker's Mac stands ([`ScreenEvent::Curtain`]).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CurtainState {
    /// Not drawn: the Mac's screens and input are its own.
    Down,
    /// Drawn, held by `holders` clients.
    Up {
        /// The clients holding it.
        holders: u32,
        /// Its own keyboard and pointer are held off too; `false` while only its screens are
        /// covered.
        input_held: bool,
    },
    /// The worker would not draw it, in its words: no screens to cover, a client that does not
    /// hold the controls, or a shield the window server refused.
    Refused {
        /// Why.
        why: String,
    },
}

/// What an open asked for, as [`ScreenEvent::OpenFailed`] names it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum OpenAsk {
    /// A display, by its id ([`ScreenRequest::Open`]).
    Target(CaptureTarget),
    /// A display made for the client, by its key ([`ScreenRequest::OpenDisplay`]).
    Made(DisplayKey),
}

/// Why the worker could not open a stream or list what it can stream. Each kind is one the
/// client answers differently; anything else is the worker's own words.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ScreenFailure {
    /// The worker may not record its screen: its Screen Recording grant is off, and only
    /// someone at that Mac can turn it on.
    NotPermitted,
    /// What was asked for is not there any more: the display went.
    Gone,
    /// Anything else, in the worker's words.
    Failed(String),
}
