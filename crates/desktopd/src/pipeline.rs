//! One stream of a whole display to one client: capture, encode, packetize and send, and what
//! the client's feedback asks of it (slopty's screen pipeline, whole displays only).
//!
//! A capture lands in a mailbox where a newer one replaces an older (`Mailbox`), and the
//! stream's encode thread codes the newest when the pacer has a slot for it and the congestion
//! guard lets a frame through ([`fit::frame_fits`]). What it holds back stays owed, and the
//! repair loop codes it once things clear; the same loop answers refreshes (from a long-term
//! reference the client acknowledged where it has one, [`ltr::LtrBook`]), keyframes (deferred
//! while the link could not drain one, [`fit::keyframe_fits`]) and refines a still picture
//! while there is budget. The client's reports drive the rate ([`RateController`]), the parity
//! ([`Redundancy`]), the chroma ([`ChromaGate`]) and the frame rate ([`Cadence`],
//! [`EncoderWatch`]), and a picture of [`crate::stripes::STRIPED_FROM`] pixels or more is coded
//! as two stripes on the two encode engines ([`crate::stripes`]). A heartbeat fills silences,
//! the cursor goes as its own datagrams at 120 Hz, and its shape as events.
//!
//! Every datagram goes to a [`DatagramSink`]: the client's link in [`crate::session`].
//!
//! A platform whose calls never wait ([`Platform::ON_RUNTIME`], the drawn one) codes on a task
//! of the stream's instead of the encode thread, and makes every other call on the runtime too,
//! so a paused tokio clock drives the whole stream and a seed repeats it.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use onicode_desktop_capture::{
    CaptureConfig, CaptureError, CaptureSource, CapturedFrame, PixelFormat, Rect,
};
use onicode_desktop_codec::{
    CodecError, EncodedPacket, EncoderConfig, FrameOptions, VideoEncoder as _, conformance,
};
use onicode_desktop_input::{InputError, InputSink as _, Pointer, PointerChanges, PointerWatch};
pub use onicode_desktop_media::PathSample;
use onicode_desktop_media::wire::{ClockEcho, MAX_DATAGRAM};
use onicode_desktop_media::{
    Cadence, ChromaGate, Decision, EncodedFrame, EncoderWatch, Fed, HEARTBEAT_AFTER, MediaError,
    Pace, Packetizer, RateController, Redundancy, Refine, cursor_datagram, heartbeat_datagram,
};
use onicode_desktop_proto::screen::{
    CaptureTarget, Chroma, CursorShape, DisplayInfo, Quality, ReceiverReport, ScreenEvent,
    ScreenFailure, ScreenInput, Stripe,
};
use onicode_desktop_proto::{DisplayId, StreamId};
use parking_lot::{Condvar, Mutex};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

use crate::platform::Platform;
use crate::{instant, stripes};

pub mod fit;
pub mod ltr;
pub mod source;
#[cfg(test)]
mod tests;

use fit::{
    Asks, KEYFRAME_IN_FLIGHT_US, KEYFRAME_VALVE_US, REFINE_BUDGET_MS, encoder_bps, frame_fits,
    keyframe_estimate, keyframe_fits, period_us, repair_at, send_ms_lo,
};
use ltr::LtrBook;
use source::SourceTracker;

type Source<P> = <P as Platform>::Capture;
type Image<P> = <Source<P> as CaptureSource>::Image;
type Frame<P> = CapturedFrame<Image<P>>;
type Resolved<P> = <Source<P> as CaptureSource>::Target;
type Running<P> = <Source<P> as CaptureSource>::Stream;
type CodedStripe = onicode_desktop_codec::stripes::Stripe;

fn now<P: Platform>() -> u64 {
    Source::<P>::now_us()
}

/// Where a stream's datagrams go: the client's link.
pub trait DatagramSink: Send + Sync {
    /// Send each datagram, in order.
    ///
    /// # Errors
    ///
    /// When the link is gone, or a datagram is larger than the path takes.
    fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused>;
    /// The largest datagram the path takes now; `None` when it takes none.
    fn max_size(&self) -> Option<usize>;
    /// Bytes of datagrams queued and not yet sent.
    fn held(&self) -> usize;
    /// The congestion window, bytes.
    fn cwnd(&self) -> u64;
    /// The link is gone.
    fn is_closed(&self) -> bool;
}

/// Why a datagram was not sent.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Refused {
    /// Larger than the path takes.
    TooLarge,
    /// The link is gone.
    Closed,
}

/// The cursor's position goes out at most this often: 120 Hz.
pub const CURSOR_PERIOD: Duration = Duration::from_micros(8_333);
/// A still pointer is looked at again at least this often.
const CURSOR_BACKSTOP: Duration = Duration::from_secs(1);
/// The cursor's shape is looked at this often.
pub const SHAPE_PERIOD: Duration = Duration::from_micros(8_333);
/// The display's bounds are read again this often (it can move in the arrangement).
pub const GEOMETRY_PERIOD: Duration = Duration::from_millis(100);
/// The source's state is looked at this often.
const SOURCE_PERIOD: Duration = Duration::from_millis(250);
/// The captures queued in the capture: three, so the capture never waits on a slow encode.
pub const QUEUE_DEPTH: u8 = 3;
/// The multiple an HEVC stream's sides are padded to: the low-latency encoder copies any other
/// size into a padded buffer of its own and queues it (MEASUREMENTS.md, "Stream sides padded
/// to 16").
pub const HEVC_ALIGN: u32 = 16;
/// How long the encode thread waits for a capture before it looks whether the stream closed.
const ENCODE_THREAD_IDLE: Duration = Duration::from_millis(500);
/// Submitted frames remembered per coder until they come back.
const IN_FLIGHT_MAX: usize = 16;

/// What a stream tells its session.
#[derive(Debug)]
pub enum StreamEvent {
    /// An event for the client.
    Screen(ScreenEvent),
    /// The capture stopped by itself: the stream is over.
    Stopped(CaptureError),
}

/// Why a stream could not be opened or changed.
#[derive(Debug, thiserror::Error)]
pub enum ScreenError {
    /// The capture's.
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// The encoder's.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// The packetizer's.
    #[error(transparent)]
    Media(#[from] MediaError),
    /// The input's.
    #[error(transparent)]
    Input(#[from] InputError),
    /// No Screen Recording grant.
    #[error("this Mac's desktopd has no Screen Recording grant")]
    NotPermitted,
    /// The stream is closed.
    #[error("screen pipeline closed")]
    Closed,
    /// No display to stream.
    #[error("no display to stream")]
    NoDisplay,
    /// No thread for the encoder.
    #[error("the encode thread would not start: {0}")]
    EncodeThread(std::io::Error),
}

impl ScreenError {
    /// The failure as the client is told it.
    #[must_use]
    pub fn failure(&self) -> ScreenFailure {
        /// ScreenCaptureKit's "the user declined" (`SCStreamErrorUserDeclined`).
        const DECLINED: i64 = -3801;
        match self {
            Self::NotPermitted | Self::Capture(CaptureError::Sck { code: DECLINED, .. }) => {
                ScreenFailure::NotPermitted
            }
            Self::NoDisplay | Self::Capture(CaptureError::NotFound(_)) => ScreenFailure::Gone,
            other => ScreenFailure::Failed(other.to_string()),
        }
    }
}

/// What opens a stream.
pub struct Opening {
    /// The stream's id on the link.
    pub id: StreamId,
    /// The display.
    pub target: CaptureTarget,
    /// What the client asked for.
    pub quality: Quality,
    /// Where its datagrams go.
    pub sink: Arc<dyn DatagramSink>,
    /// Where its events go.
    pub events: Arc<dyn Fn(StreamEvent) + Send + Sync>,
}

impl std::fmt::Debug for Opening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Opening")
            .field("id", &self.id)
            .field("target", &self.target)
            .field("quality", &self.quality)
            .finish_non_exhaustive()
    }
}

/// The newest capture, waiting for the encode thread (or task): a newer one replaces it.
struct Mailbox<F> {
    slot: Mutex<Option<F>>,
    wake: Condvar,
    /// Wakes the encode task of a platform that codes on the runtime.
    landed: Notify,
}

impl<F> Default for Mailbox<F> {
    fn default() -> Self {
        Self {
            slot: Mutex::new(None),
            wake: Condvar::new(),
            landed: Notify::new(),
        }
    }
}

impl<F> Mailbox<F> {
    /// Leave `frame`; the one it replaced.
    fn post(&self, frame: F) -> Option<F> {
        let superseded = self.slot.lock().replace(frame);
        self.wake.notify_one();
        self.landed.notify_one();
        superseded
    }

    /// The frame waiting, if one is.
    fn try_take(&self) -> Option<F> {
        self.slot.lock().take()
    }

    /// The frame waiting, waiting up to `patience` for one.
    fn take(&self, patience: Duration) -> Option<F> {
        let mut slot = self.slot.lock();
        if slot.is_none() {
            let _timed_out = self.wake.wait_for(&mut slot, patience);
        }
        slot.take()
    }

    fn clear(&self) {
        let dropped = self.slot.lock().take();
        drop(dropped);
    }
}

/// A frame handed to the encoder and not yet back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Submitted {
    pts_us: u64,
    at_us: u64,
    /// The stripes coded from its capture, as a bit each; zero for a whole picture.
    stripes: u8,
}

/// What the next frame of a coder is asked to be.
#[derive(Debug, Default)]
struct Pending {
    keyframe: bool,
    refresh: bool,
    acked: Vec<u64>,
}

/// One coded picture: the whole display, or one stripe of it, with its own packets, parity,
/// references and refinement.
struct Coder {
    media: StreamId,
    pending: Mutex<Pending>,
    packetizer: Mutex<Packetizer>,
    redundancy: Mutex<Redundancy>,
    ltr: Mutex<LtrBook>,
    /// The live session's number; a packet of another is a replaced session's, dropped.
    session: AtomicU64,
    keyframe_bytes: AtomicU64,
    keyframe_deferred_us: AtomicU64,
    keyframe_submitted_us: AtomicU64,
    refine: Mutex<Refine>,
    in_flight: Mutex<VecDeque<Submitted>>,
    sent_at_report: AtomicU64,
}

impl Coder {
    fn new(media: StreamId) -> Self {
        Self {
            media,
            pending: Mutex::new(Pending {
                keyframe: true,
                ..Pending::default()
            }),
            packetizer: Mutex::new(Packetizer::new(media)),
            redundancy: Mutex::new(Redundancy::new()),
            ltr: Mutex::new(LtrBook::default()),
            session: AtomicU64::new(0),
            keyframe_bytes: AtomicU64::new(0),
            keyframe_deferred_us: AtomicU64::new(0),
            keyframe_submitted_us: AtomicU64::new(0),
            refine: Mutex::new(Refine::default()),
            in_flight: Mutex::new(VecDeque::with_capacity(IN_FLIGHT_MAX)),
            sent_at_report: AtomicU64::new(0),
        }
    }

    fn submitted(&self, submitted: Submitted) {
        let mut in_flight = self.in_flight.lock();
        if in_flight.len() >= IN_FLIGHT_MAX {
            in_flight.pop_front();
        }
        in_flight.push_back(submitted);
    }

    /// The frame at `pts_us` came back at `now`: how long it took, and what it was.
    fn returned(&self, pts_us: u64, now: u64) -> Option<(u64, Submitted)> {
        let found = {
            let mut in_flight = self.in_flight.lock();
            let at = in_flight.iter().position(|frame| frame.pts_us == pts_us);
            at.and_then(|i| in_flight.remove(i))
        };
        found.map(|frame| (now.saturating_sub(frame.at_us), frame))
    }

    fn ltr_usable(&self) -> bool {
        self.ltr.lock().usable()
    }

    /// New sessions: start from a keyframe, with no reference.
    fn rebuilt(&self) {
        self.ltr.lock().reset();
        {
            let mut pending = self.pending.lock();
            pending.acked.clear();
            pending.keyframe = true;
        }
        self.refine.lock().rebuilt();
        self.in_flight.lock().clear();
        self.keyframe_bytes.store(0, Ordering::Relaxed);
        self.keyframe_deferred_us.store(0, Ordering::Relaxed);
        self.keyframe_submitted_us.store(0, Ordering::Relaxed);
    }
}

/// A striped capture's stripes coming back: the frame is whole once every stripe is.
#[derive(Clone, Copy, Default, Debug)]
struct Join {
    pts: u64,
    waiting: u8,
    took: Option<u64>,
    keyframe: bool,
}

impl Join {
    fn returned(
        &mut self,
        index: usize,
        pts: u64,
        took: Option<u64>,
        keyframe: bool,
    ) -> Option<(Option<u64>, bool)> {
        let bit = 1_u8 << index;
        if pts != self.pts || self.waiting & bit == 0 {
            return None;
        }
        self.waiting &= !bit;
        self.took = self.took.max(took);
        self.keyframe |= keyframe;
        (self.waiting == 0).then_some((self.took, self.keyframe))
    }
}

/// How a stream is coded now.
struct Coding<V> {
    sessions: [Option<Arc<V>>; Stripe::MAX],
    layout: Option<[CodedStripe; 2]>,
    capture: CaptureConfig,
    encoder: EncoderConfig,
    /// The display's own pixels.
    native: (u32, u32),
    /// Display points to its pixels.
    point_scale: f64,
}

impl<V> Coding<V> {
    const fn count(&self) -> usize {
        if self.layout.is_some() {
            Stripe::MAX
        } else {
            1
        }
    }

    /// Coder `index`'s share of the rows.
    fn share(&self, index: usize) -> (u32, u32) {
        let Some([top, lower]) = &self.layout else {
            return (1, 1);
        };
        let rows = top.shown_rows.saturating_add(lower.shown_rows).max(1);
        (
            if index == 0 {
                top.shown_rows
            } else {
                lower.shown_rows
            },
            rows,
        )
    }

    fn live(&self) -> Vec<(usize, Arc<V>)> {
        self.sessions
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|s| (i, Arc::clone(s))))
            .collect()
    }
}

/// What an encode turn did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attempt {
    Sent,
    Nothing,
    NotDue,
    NoRoom,
    Failed,
}

/// What a stream counts.
#[derive(Debug, Default)]
struct Counters {
    captured: AtomicU64,
    superseded: AtomicU64,
    dropped: AtomicU64,
    encoded: AtomicU64,
    datagrams: AtomicU64,
    heartbeats: AtomicU64,
    refreshes: AtomicU64,
    keyframes_deferred: AtomicU64,
    bitrate_bps: AtomicU64,
    encoder_bps: AtomicU64,
    repaired: AtomicU64,
    refined: AtomicU64,
    ltr_offered: AtomicU64,
    ltr_acked: AtomicU64,
    inputs: AtomicU64,
}

/// A stream's counts, for its tests and its log.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Stats {
    /// Captures delivered.
    pub captured: u64,
    /// Captures a newer one replaced before they were coded.
    pub superseded: u64,
    /// Captures the congestion guard refused.
    pub dropped: u64,
    /// Frames coded (a striped capture counts once).
    pub encoded: u64,
    /// Datagrams sent.
    pub datagrams: u64,
    /// Heartbeats sent.
    pub heartbeats: u64,
    /// Refreshes asked for.
    pub refreshes: u64,
    /// Standalone pictures deferred while the link could not drain one.
    pub keyframes_deferred: u64,
    /// The rate's target, bits a second.
    pub bitrate_bps: u64,
    /// Held captures coded again.
    pub repaired: u64,
    /// Refinements of a still picture.
    pub refined: u64,
    /// References offered, and acknowledged.
    pub ltr_offered: u64,
    /// References acknowledged.
    pub ltr_acked: u64,
    /// Inputs injected.
    pub inputs: u64,
}

/// What the client asked of the stream, from which its coding follows.
#[derive(Clone, Copy, Debug)]
struct Desired {
    quality: Quality,
}

/// A stream's state, shared by its threads and tasks.
struct Shared<P: Platform> {
    id: StreamId,
    sink: Arc<dyn DatagramSink>,
    events: Arc<dyn Fn(StreamEvent) + Send + Sync>,
    coders: [Coder; Stripe::MAX],
    coding: Mutex<Coding<P::Video>>,
    desired: Mutex<Desired>,
    target: Mutex<CaptureTarget>,
    capture: Mutex<Option<Running<P>>>,
    sessions: AtomicU64,
    join: Mutex<Join>,
    mailbox: Mailbox<Frame<P>>,
    /// Encode turns, one at a time: the encode thread's and the repair loop's.
    turn: Mutex<()>,
    held: Mutex<Option<Frame<P>>>,
    held_us: AtomicU64,
    owed: AtomicBool,
    rate: Mutex<RateController>,
    chroma: Mutex<ChromaGate>,
    watch: Mutex<EncoderWatch>,
    cadence: Mutex<Cadence>,
    fps: AtomicU16,
    fps_ceiling: AtomicU16,
    fps_asked: AtomicU16,
    fed_fps: AtomicU16,
    last_push_us: AtomicU64,
    last_encoded_us: AtomicU64,
    pace_us: AtomicU64,
    encoder_lost: AtomicU64,
    repair: Notify,
    rebuild: Notify,
    bounds: Mutex<Option<Rect>>,
    /// Stream pixels a display point, as `f64` bits.
    pixels_per_point: AtomicU64,
    input: Mutex<P::Input>,
    closed: AtomicBool,
    counters: Counters,
}

/// The scale the quality asks for, clamped.
fn scale_of(quality: &Quality) -> f64 {
    if quality.scale.is_finite() {
        f64::from(quality.scale).clamp(0.05, 1.0)
    } else {
        1.0
    }
}

/// A side of `px` native pixels at `scale`: even, and at least 2.
fn scaled_side(px: u32, scale: f64) -> u32 {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped"
    )]
    let v = (f64::from(px) * scale).round().clamp(2.0, 16_384.0) as u32;
    v.next_multiple_of(2)
}

const fn capture_format(chroma: Chroma) -> PixelFormat {
    match chroma {
        Chroma::Subsampled => PixelFormat::Nv12Full,
        Chroma::Full => PixelFormat::Yuv444Full10,
    }
}

/// The capture and the encoder for a display `native` pixels in size, refreshing at
/// `refresh_hz`, at `quality` with `chroma`: the capture padded to [`HEVC_ALIGN`], which
/// the encoder codes and the keyframes' parameter sets crop away.
fn configs(
    native: (u32, u32),
    quality: &Quality,
    refresh_hz: Option<f64>,
    chroma: Chroma,
) -> (CaptureConfig, EncoderConfig) {
    let scale = scale_of(quality);
    let (width, height) = (scaled_side(native.0, scale), scaled_side(native.1, scale));
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped"
    )]
    let display = refresh_hz
        .filter(|hz| hz.is_finite() && *hz > 0.0)
        .map(|hz| hz.round().clamp(1.0, 240.0) as u16);
    let fps = display
        .map_or(quality.fps, |hz| quality.fps.min(hz))
        .clamp(1, 240);
    let capture = CaptureConfig {
        width,
        height,
        align: HEVC_ALIGN,
        fps: if display.is_some() { 0 } else { fps },
        format: capture_format(chroma),
        queue_depth: QUEUE_DEPTH,
    };
    let (width, height) = capture.surface();
    let encoder = EncoderConfig {
        width,
        height,
        codec: quality.codec,
        fps,
        bitrate_bps: quality.bitrate_bps.max(100_000),
        chroma,
    };
    (capture, encoder)
}

/// The stripes a stream of `capture` is coded as, if any ([`stripes::striped`]).
fn striping(capture: &CaptureConfig) -> Option<[CodedStripe; 2]> {
    let (width, height) = capture.surface();
    if stripes::striped(width, height) {
        onicode_desktop_codec::stripes::layout(height)
    } else {
        None
    }
}

/// The stripes as the client is told them.
fn told_stripes(id: StreamId, layout: Option<&[CodedStripe; 2]>) -> Vec<Stripe> {
    layout.map_or_else(Vec::new, |layout| {
        layout
            .iter()
            .enumerate()
            .map(|(i, s)| Stripe {
                media: Stripe::media_of(id, i),
                coded_top: s.coded_top,
                coded_rows: s.coded_rows,
                shown_top: s.shown_top,
                shown_rows: s.shown_rows,
            })
            .collect()
    })
}

/// Ask the capture for what it can share now.
async fn enumerate<P: Platform>() -> Result<<Source<P> as CaptureSource>::Content, ScreenError> {
    let (tx, rx) = oneshot::channel();
    Source::<P>::enumerate(move |content| {
        let _gone = tx.send(content);
    });
    rx.await
        .unwrap_or_else(|_| Err(CaptureError::Stopped("never answered".to_owned())))
        .map_err(ScreenError::from)
}

/// Wait for one of the capture's callbacks.
async fn answered(
    call: impl FnOnce(Box<dyn FnOnce(Result<(), CaptureError>) + Send>),
) -> Result<(), CaptureError> {
    let (tx, rx) = oneshot::channel();
    call(Box::new(move |done| {
        let _gone = tx.send(done);
    }));
    rx.await
        .unwrap_or_else(|_| Err(CaptureError::Stopped("never answered".to_owned())))
}

/// Retire a session on a thread of its own: tearing one down waits for its frames in flight.
/// A platform whose calls never wait drops it in place.
fn retire<P: Platform>(old: Option<Arc<P::Video>>) {
    let Some(old) = old else { return };
    if P::ON_RUNTIME {
        drop(old);
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("onicode-retire-encoder".to_owned())
        .spawn(move || drop(old));
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "no thread to retire an encoder session on: dropped in place");
    }
}

/// A stream of a display to one client. Dropping it ends the stream; [`Self::close`] also
/// waits for the capture to stop.
pub struct Pipeline<P: Platform> {
    shared: Arc<Shared<P>>,
    tasks: Vec<JoinHandle<()>>,
}

impl<P: Platform> std::fmt::Debug for Pipeline<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("id", &self.shared.id)
            .finish_non_exhaustive()
    }
}

impl<P: Platform> Pipeline<P> {
    /// The displays the client may stream, as the window server lists them (never
    /// ScreenCaptureKit, which prompts), without those made for clients.
    #[must_use]
    pub fn displays() -> Vec<DisplayInfo> {
        P::displays()
            .into_iter()
            .filter(|d| !P::made_for_a_client(d.id.0))
            .collect()
    }

    /// Whether ScreenCaptureKit lists `display` yet.
    ///
    /// # Errors
    ///
    /// When the capture could not be asked.
    pub async fn lists_display(display: DisplayId) -> Result<bool, ScreenError> {
        let content = enumerate::<P>().await?;
        Ok(Source::<P>::displays(&content)
            .iter()
            .any(|d| d.id == display))
    }

    /// A physical display to stream, `other_than` it where there is another.
    ///
    /// # Errors
    ///
    /// [`ScreenError::NoDisplay`] when there is none.
    pub async fn physical_display(other_than: Option<DisplayId>) -> Result<DisplayId, ScreenError> {
        let displays = Self::displays();
        displays
            .iter()
            .map(|d| d.id)
            .find(|&id| Some(id) != other_than)
            .or_else(|| displays.first().map(|d| d.id))
            .ok_or(ScreenError::NoDisplay)
    }

    /// Open a stream: resolve the display, build the encoder, start the capture and the
    /// stream's tasks. Returns the stream and its `Opened`.
    ///
    /// # Errors
    ///
    /// When the display is gone, the capture or the encoder refused.
    pub async fn open(opening: Opening) -> Result<(Self, ScreenEvent), ScreenError> {
        let Opening {
            id,
            target,
            quality,
            sink,
            events,
        } = opening;
        let content = enumerate::<P>().await?;
        let resolved = Source::<P>::resolve(&content, target)?;
        drop(content);
        let native = Source::<P>::pixel_size(&resolved);
        let point_scale = f64::from(Source::<P>::point_scale(&resolved));
        let refresh_hz = Source::<P>::refresh_hz(target);
        let (capture, encoder) = configs(native, &quality, refresh_hz, Chroma::Subsampled);
        let shared = Arc::new(Shared::<P>::new(
            id,
            target,
            quality,
            (sink, events),
            Coding {
                sessions: [None, None],
                layout: None,
                capture,
                encoder,
                native,
                point_scale,
            },
        ));
        let built = shared.build().await?;
        let capture_config = shared.coding.lock().capture;
        let encoder = start_encoder(&shared)?;
        let (running, started) = start_capture(&shared, &resolved, &capture_config)?;
        *shared.capture.lock() = Some(running);
        started
            .await
            .unwrap_or_else(|_| Err(CaptureError::Stopped("never answered".to_owned())))?;
        shared.follow_bounds();
        let opened = ScreenEvent::Opened {
            stream: id,
            target,
            codec: quality.codec,
            width: capture_config.width,
            height: capture_config.height,
            #[expect(clippy::cast_possible_truncation, reason = "a display's scale")]
            scale: (point_scale * scale_of(&quality)) as f32,
            stripes: built,
        };
        tracing::info!(stream = %id, ?target, width = capture_config.width, height = capture_config.height, "stream opened");
        let mut tasks = vec![
            tokio::spawn(repair_loop(Arc::clone(&shared))),
            tokio::spawn(beat_loop(Arc::clone(&shared))),
            tokio::spawn(cursor_loop(Arc::clone(&shared))),
            tokio::spawn(shape_loop(Arc::clone(&shared))),
            tokio::spawn(source_loop(Arc::clone(&shared))),
            tokio::spawn(geometry_loop(Arc::clone(&shared))),
        ];
        tasks.extend(encoder);
        Ok((Self { shared, tasks }, opened))
    }

    /// The stream's id.
    #[must_use]
    pub fn id(&self) -> StreamId {
        self.shared.id
    }

    /// The display it streams.
    #[must_use]
    pub fn target(&self) -> CaptureTarget {
        *self.shared.target.lock()
    }

    /// Its counts.
    #[must_use]
    pub fn stats(&self) -> Stats {
        self.shared.stats()
    }

    /// A receiver report for coder `index` (the client's, with the path's sample).
    pub fn report(
        &self,
        index: usize,
        report: &ReceiverReport,
        path: Option<PathSample>,
    ) -> Option<Decision> {
        self.shared.report(index, report, path)
    }

    /// The client asks for a refresh of coder `index`: from a reference, or a keyframe.
    pub fn request_refresh(&self, index: usize, last_good_frame: u32, keyframe: bool) {
        self.shared
            .request_refresh(index, last_good_frame, keyframe);
    }

    /// The client misses `fragments` of coder `index`'s `frame`.
    pub fn nack(&self, index: usize, frame: u32, fragments: &[u16]) {
        self.shared.nack(index, frame, fragments);
    }

    /// The client's clock probe, sent at `sent_us` on its clock, arrived at `arrived`.
    pub fn echo_clock(&self, sent_us: u64, arrived: Instant) {
        self.shared.echo_clock(sent_us, arrived);
    }

    /// The client asks for another quality: the rate and frame rate follow at once, a new
    /// scale, codec or chroma with new sessions.
    pub fn set_quality(&self, quality: Quality) {
        self.shared.set_quality(quality);
    }

    /// Inject the controller's `input`.
    ///
    /// # Errors
    ///
    /// The input's.
    pub fn inject(&self, input: &ScreenInput) -> Result<(), InputError> {
        self.shared.counters.inputs.fetch_add(1, Ordering::Relaxed);
        self.shared.input.lock().inject(input)
    }

    /// Let go of every key and button the stream's input holds down: its client lost the
    /// controls.
    pub fn release_input(&self) {
        self.shared.input.lock().release_all();
    }

    /// Stream `display` from now on (a display made for the client was made anew). Returns the
    /// `Geometry` to tell.
    ///
    /// # Errors
    ///
    /// When the display could not be resolved or the capture refused it.
    pub async fn switch_display(&self, display: DisplayId) -> Result<ScreenEvent, ScreenError> {
        let target = CaptureTarget::Display(display);
        let content = enumerate::<P>().await?;
        let resolved = Source::<P>::resolve(&content, target)?;
        drop(content);
        let native = Source::<P>::pixel_size(&resolved);
        let point_scale = f64::from(Source::<P>::point_scale(&resolved));
        let retargeted = self.shared.capture.lock().as_ref().map(|running| {
            let (tx, rx) = oneshot::channel();
            Source::<P>::retarget(running, &resolved, move |done| {
                let _gone = tx.send(done);
            });
            rx
        });
        if let Some(retargeted) = retargeted {
            retargeted
                .await
                .unwrap_or_else(|_| Err(CaptureError::Stopped("never answered".to_owned())))?;
        }
        *self.shared.target.lock() = target;
        {
            let mut coding = self.shared.coding.lock();
            coding.native = native;
            coding.point_scale = point_scale;
        }
        *self.shared.input.lock() = <P::Input as onicode_desktop_input::InputSink>::new(
            target,
            point_scale * scale_of(&self.shared.desired.lock().quality),
        );
        self.shared.follow_bounds();
        self.shared.reconfigure().await
    }

    /// Resolve the stream's display again against a fresh enumeration and hand the capture the
    /// new filter: the windows this process leaves out changed (the curtain's shield,
    /// `onicode_desktop_capture::leave_out`), and a running capture keeps the filter it was
    /// resolved with until then. Nothing else about the stream changes.
    ///
    /// # Errors
    ///
    /// When the display could not be resolved or the capture refused the filter; the capture
    /// keeps its old one.
    pub async fn refilter(&self) -> Result<(), ScreenError> {
        let target = *self.shared.target.lock();
        let content = enumerate::<P>().await?;
        let resolved = Source::<P>::resolve(&content, target)?;
        drop(content);
        let retargeted = self.shared.capture.lock().as_ref().map(|running| {
            let (tx, rx) = oneshot::channel();
            Source::<P>::retarget(running, &resolved, move |done| {
                let _gone = tx.send(done);
            });
            rx
        });
        if let Some(retargeted) = retargeted {
            retargeted
                .await
                .unwrap_or_else(|_| Err(CaptureError::Stopped("never answered".to_owned())))?;
        }
        Ok(())
    }

    /// End the stream, waiting for its capture to stop.
    pub async fn close(mut self) {
        self.shared.closed.store(true, Ordering::Release);
        for task in self.tasks.drain(..) {
            task.abort();
        }
        let running = self.shared.capture.lock().take();
        if let Some(running) = running {
            let stopped = answered(|done| Source::<P>::stop(&running, done)).await;
            drop(running);
            if let Err(e) = stopped {
                tracing::debug!(stream = %self.shared.id, error = %e, "the capture would not stop");
            }
        }
        self.shared.end();
    }
}

impl<P: Platform> Drop for Pipeline<P> {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Release);
        for task in &self.tasks {
            task.abort();
        }
        let running = self.shared.capture.lock().take();
        if let Some(running) = running {
            Source::<P>::stop(&running, |_stopped| {});
        }
        self.shared.end();
    }
}

/// What a capture's start answers.
type Started = oneshot::Receiver<Result<(), CaptureError>>;

/// Start the capture of `resolved` into `shared`'s mailbox: the capture, and its answer to
/// wait for.
fn start_capture<P: Platform>(
    shared: &Arc<Shared<P>>,
    resolved: &Resolved<P>,
    config: &CaptureConfig,
) -> Result<(Running<P>, Started), ScreenError> {
    let posted = Arc::downgrade(shared);
    let stopped = Arc::downgrade(shared);
    let (tx, rx) = oneshot::channel();
    let running = Source::<P>::start(
        resolved,
        config,
        move |frame| {
            if let Some(shared) = posted.upgrade() {
                shared.post(frame);
            }
        },
        move |e| {
            if let Some(shared) = stopped.upgrade()
                && !shared.closed.load(Ordering::Acquire)
            {
                tracing::warn!(stream = %shared.id, error = %e, "the capture stopped");
                (shared.events)(StreamEvent::Stopped(e));
            }
        },
        move |started| {
            let _gone = tx.send(started);
        },
    )?;
    Ok((running, rx))
}

/// What codes the newest capture as it lands: the stream's encode thread, or for a platform
/// whose encoder never waits ([`Platform::ON_RUNTIME`]) a task of the stream's, returned to be
/// ended with the others.
fn start_encoder<P: Platform>(
    shared: &Arc<Shared<P>>,
) -> Result<Option<JoinHandle<()>>, ScreenError> {
    if P::ON_RUNTIME {
        return Ok(Some(tokio::spawn(encode_loop(Arc::clone(shared)))));
    }
    start_encode_thread(shared).map(|()| None)
}

/// The encode thread's loop, as a task: codes each capture the moment it lands, at the same
/// instant of the runtime's clock.
async fn encode_loop<P: Platform>(shared: Arc<Shared<P>>) {
    while !shared.closed.load(Ordering::Acquire) {
        match shared.mailbox.try_take() {
            Some(frame) => shared.on_frame(frame),
            None => shared.mailbox.landed.notified().await,
        }
    }
}

/// Run `call` on the blocking pool where the platform's calls can block, and in place where
/// they never wait ([`Platform::ON_RUNTIME`]); `None` when the pool dropped it.
async fn blocking<P: Platform, R: Send + 'static>(
    call: impl FnOnce() -> R + Send + 'static,
) -> Option<R> {
    if P::ON_RUNTIME {
        Some(call())
    } else {
        tokio::task::spawn_blocking(call).await.ok()
    }
}

/// The stream's encode thread: codes the newest capture as it lands.
fn start_encode_thread<P: Platform>(shared: &Arc<Shared<P>>) -> Result<(), ScreenError> {
    let weak = Arc::downgrade(shared);
    std::thread::Builder::new()
        .name(format!("onicode-encode-{}", shared.id.0))
        .spawn(move || {
            onicode_desktop_input::user_interactive_thread();
            while let Some(shared) = weak.upgrade() {
                if shared.closed.load(Ordering::Acquire) {
                    break;
                }
                if let Some(frame) = shared.mailbox.take(ENCODE_THREAD_IDLE) {
                    shared.on_frame(frame);
                }
            }
        })
        .map(drop)
        .map_err(ScreenError::EncodeThread)
}

/// Coder `index`'s session, made on the calling thread.
fn open_session<P: Platform>(
    shared: &Weak<Shared<P>>,
    config: EncoderConfig,
    shown: (u32, u32),
    session: u64,
    index: usize,
    stripe: Option<CodedStripe>,
) -> Result<P::Video, CodecError> {
    let weak = Weak::clone(shared);
    let rows = stripe.map_or(config.height, |stripe| stripe.coded_rows);
    let shown_rows = stripe.map_or(shown.1, |stripe| stripe.coded_rows);
    let shown = (shown.0, shown_rows);
    let padded = shown != (config.width, rows);
    let sink = move |mut packet: EncodedPacket| {
        let Some(shared) = weak.upgrade() else { return };
        if padded
            && packet.keyframe
            && let Err(e) = conformance::crop_access_unit(&mut packet.data, shown)
        {
            tracing::warn!(stream = %shared.id, index, error = %e, ?shown, "a keyframe shows its padding");
        }
        shared.on_session_packet(index, session, &packet);
    };
    match stripe {
        None => <P::Video as onicode_desktop_codec::VideoEncoder>::new(config, sink),
        Some(stripe) => {
            <P::Video as onicode_desktop_codec::VideoEncoder>::stripe(config, stripe, sink)
        }
    }
}

/// A live coder: its index, the encoder and its size.
type LiveCoder<V> = (usize, Arc<V>, (u32, u32));

/// Where a stream's datagrams and events go.
type Outlets = (
    Arc<dyn DatagramSink>,
    Arc<dyn Fn(StreamEvent) + Send + Sync>,
);

impl<P: Platform> Shared<P> {
    fn new(
        id: StreamId,
        target: CaptureTarget,
        quality: Quality,
        (sink, events): Outlets,
        coding: Coding<P::Video>,
    ) -> Self {
        let fps = coding.encoder.fps;
        let size = (coding.encoder.width, coding.encoder.height);
        let rate = RateController::new(quality.bitrate_bps.max(100_000));
        let chroma = ChromaGate::new(quality.chroma, size, rate.target_bps());
        let scale = coding.point_scale * scale_of(&quality);
        Self {
            id,
            sink,
            events,
            coders: [
                Coder::new(Stripe::media_of(id, 0)),
                Coder::new(Stripe::media_of(id, 1)),
            ],
            coding: Mutex::new(coding),
            desired: Mutex::new(Desired { quality }),
            target: Mutex::new(target),
            capture: Mutex::new(None),
            sessions: AtomicU64::new(0),
            join: Mutex::new(Join::default()),
            mailbox: Mailbox::default(),
            turn: Mutex::new(()),
            held: Mutex::new(None),
            held_us: AtomicU64::new(0),
            owed: AtomicBool::new(false),
            rate: Mutex::new(rate),
            chroma: Mutex::new(chroma),
            watch: Mutex::new(EncoderWatch::default()),
            cadence: Mutex::new(Cadence::new(fps)),
            fps: AtomicU16::new(fps),
            fps_ceiling: AtomicU16::new(fps),
            fps_asked: AtomicU16::new(fps),
            fed_fps: AtomicU16::new(fps),
            last_push_us: AtomicU64::new(0),
            last_encoded_us: AtomicU64::new(0),
            pace_us: AtomicU64::new(0),
            encoder_lost: AtomicU64::new(0),
            repair: Notify::new(),
            rebuild: Notify::new(),
            bounds: Mutex::new(Source::<P>::target_bounds(target)),
            pixels_per_point: AtomicU64::new(scale.to_bits()),
            input: Mutex::new(<P::Input as onicode_desktop_input::InputSink>::new(
                target, scale,
            )),
            closed: AtomicBool::new(false),
            counters: Counters::default(),
        }
    }

    fn coder(&self, index: usize) -> &Coder {
        self.coders.get(index).unwrap_or(&self.coders[0])
    }

    fn coders(&self, count: usize) -> impl Iterator<Item = (usize, &Coder)> {
        self.coders.iter().enumerate().take(count)
    }

    fn stats(&self) -> Stats {
        let c = &self.counters;
        let load = |a: &AtomicU64| a.load(Ordering::Relaxed);
        Stats {
            captured: load(&c.captured),
            superseded: load(&c.superseded),
            dropped: load(&c.dropped),
            encoded: load(&c.encoded),
            datagrams: load(&c.datagrams),
            heartbeats: load(&c.heartbeats),
            refreshes: load(&c.refreshes),
            keyframes_deferred: load(&c.keyframes_deferred),
            bitrate_bps: load(&c.bitrate_bps),
            repaired: load(&c.repaired),
            refined: load(&c.refined),
            ltr_offered: load(&c.ltr_offered),
            ltr_acked: load(&c.ltr_acked),
            inputs: load(&c.inputs),
        }
    }

    /// The stream is over: let go of what it holds.
    fn end(&self) {
        self.mailbox.clear();
        let held = self.held.lock().take();
        drop(held);
        let sessions = std::mem::take(&mut self.coding.lock().sessions);
        for session in sessions {
            retire::<P>(session);
        }
        self.input.lock().release_all();
    }

    fn post(&self, frame: Frame<P>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.counters.captured.fetch_add(1, Ordering::Relaxed);
        let at = frame.capture_ts_us;
        if let Some(old) = self.mailbox.post(frame) {
            self.counters.superseded.fetch_add(1, Ordering::Relaxed);
            let fps = self.fps.load(Ordering::Relaxed);
            // A capture replaced within its own slot is no loss to the encoder's rung.
            if at.saturating_sub(old.capture_ts_us) >= period_us(fps) {
                self.watch.lock().superseded(fps);
            }
        }
    }

    fn on_frame(&self, frame: Frame<P>) {
        let attempt = self.try_encode(Some(frame), now::<P>());
        if attempt != Attempt::Sent {
            self.repair.notify_one();
        }
    }

    fn try_encode(&self, fresh: Option<Frame<P>>, now: u64) -> Attempt {
        let _turn = self.turn.lock();
        self.encode_turn(fresh, now)
    }

    fn frame_fits(&self) -> bool {
        frame_fits(
            self.sink.held(),
            self.sink.cwnd(),
            self.counters.bitrate_bps.load(Ordering::Relaxed),
            self.fed_fps.load(Ordering::Relaxed),
        )
    }

    /// One decision of what to code from the held capture (`fresh` when one just landed), and
    /// coding it.
    #[expect(clippy::too_many_lines, reason = "one decision, read top to bottom")]
    fn encode_turn(&self, fresh: Option<Frame<P>>, now: u64) -> Attempt {
        let mut held = self.held.lock();
        let is_fresh = fresh.is_some();
        let mut replaced = None;
        if let Some(frame) = fresh {
            self.held_us
                .store(frame.capture_ts_us.max(1), Ordering::Relaxed);
            self.owed.store(true, Ordering::Relaxed);
            replaced = held.replace(frame);
        }
        let Some(frame) = held.as_ref() else {
            return Attempt::Nothing;
        };
        let (sessions, count) = {
            let coding = self.coding.lock();
            (coding.sessions.clone(), coding.count())
        };
        if sessions.first().is_none_or(Option::is_none) {
            return Attempt::NoRoom;
        }
        let owed = self.owed.load(Ordering::Relaxed);
        let (mut keyframes, mut refreshes) = (0_u8, 0_u8);
        for (i, coder) in self.coders(count) {
            let pending = coder.pending.lock();
            keyframes |= u8::from(pending.keyframe) << i;
            refreshes |= u8::from(pending.refresh) << i;
        }
        let every = if count > 1 { 0b11 } else { 0b01 };
        let last = self.last_encoded_us.load(Ordering::Relaxed);
        let refining = !is_fresh && !owed && keyframes == 0 && refreshes == 0;
        let coding = if refining {
            let mut due = 0_u8;
            for (i, coder) in self.coders(count) {
                if self.refine_due(coder, count).is_some_and(|at| now >= at) {
                    due |= 1 << i;
                }
            }
            due
        } else if is_fresh || owed {
            every
        } else {
            keyframes | refreshes
        };
        if coding == 0 {
            return Attempt::Nothing;
        }
        if refining && self.sink.held() > 0 {
            return Attempt::NoRoom;
        }
        let at = if is_fresh { frame.capture_ts_us } else { now };
        let fps = self.fps.load(Ordering::Relaxed);
        let mut pace = Pace::resume(self.pace_us.load(Ordering::Relaxed));
        let due = refining || pace.due(at, fps);
        if !due && keyframes & coding == 0 {
            return Attempt::NotDue;
        }
        if !self.frame_fits() {
            if is_fresh {
                self.dropped(now, count);
            }
            return Attempt::NoRoom;
        }
        let mut deferred = 0_u8;
        for (i, coder) in self.coders(count) {
            if keyframes & coding & (1 << i) != 0 && !self.keyframe_admitted(coder, now) {
                deferred |= 1 << i;
            }
        }
        if !due && keyframes & coding & !deferred == 0 {
            return Attempt::NotDue;
        }
        let mut options: [Option<(FrameOptions, bool)>; Stripe::MAX] = [None, None];
        for ((i, coder), slot) in self.coders(count).zip(&mut options) {
            if coding & (1 << i) == 0 {
                continue;
            }
            let defer = deferred & (1 << i) != 0;
            let usable = coder.ltr_usable();
            let mut pending = coder.pending.lock();
            let refresh = std::mem::take(&mut pending.refresh) || defer;
            let standalone = refresh && !usable;
            let force_keyframe = !defer && (std::mem::take(&mut pending.keyframe) || standalone);
            *slot = Some((
                FrameOptions {
                    force_keyframe,
                    force_ltr_refresh: refresh && !standalone,
                    acked_ltr: std::mem::take(&mut pending.acked),
                },
                standalone,
            ));
        }
        let pts = if refining {
            Refine::stamp(now, last, period_us(fps))
        } else {
            at.max(last.saturating_add(1))
        };
        self.last_encoded_us.store(pts, Ordering::Relaxed);
        if !refining {
            pace.sent(at, fps);
            self.pace_us.store(pace.next_us(), Ordering::Relaxed);
        }
        self.owed.store(false, Ordering::Relaxed);
        if refining {
            self.counters.refined.fetch_add(1, Ordering::Relaxed);
        } else if !is_fresh {
            self.counters.repaired.fetch_add(1, Ordering::Relaxed);
        }
        let stripes = if count > 1 { coding } else { 0 };
        if stripes.count_ones() > 1 && !refining {
            *self.join.lock() = Join {
                pts,
                waiting: coding,
                took: None,
                keyframe: false,
            };
        }
        for (i, coder) in self.coders(count) {
            if coding & (1 << i) == 0 {
                continue;
            }
            if refining {
                coder.refine.lock().refinement_sent(pts, now);
            } else if is_fresh {
                coder.refine.lock().fresh(pts, now);
            } else {
                coder.refine.lock().other_sent(now);
            }
            coder.submitted(Submitted {
                pts_us: pts,
                at_us: now,
                stripes,
            });
            if options
                .get(i)
                .and_then(Option::as_ref)
                .is_some_and(|(o, _)| o.force_keyframe)
            {
                coder
                    .keyframe_submitted_us
                    .store(now.max(1), Ordering::Relaxed);
            }
        }
        let (image, captured) = (frame.image.clone(), frame.capture_ts_us);
        drop(held);
        drop(replaced);
        let outcomes: Vec<Option<Result<(), CodecError>>> = sessions
            .iter()
            .zip(&options)
            .map(|(session, asked)| {
                session
                    .as_ref()
                    .zip(asked.as_ref())
                    .map(|(s, (o, _))| s.encode(&image, pts, o))
            })
            .collect();
        let mut stale = false;
        let mut failed = false;
        for (((i, coder), outcome), asked) in self.coders(count).zip(outcomes).zip(&options) {
            let (Some(outcome), Some((options, _standalone))) = (outcome, asked) else {
                continue;
            };
            let Err(e) = outcome else { continue };
            let _failed = coder.returned(pts, self::now::<P>());
            stale |= matches!(e, CodecError::WrongSize { .. });
            failed = true;
            match e {
                CodecError::NotFullChroma(_) | CodecError::WrongSize { .. } => {
                    tracing::debug!(stream = %self.id, i, error = %e, "a capture of the old shape dropped");
                }
                CodecError::EncoderLost { .. } => {
                    let session = coder.session.load(Ordering::Relaxed);
                    if self.encoder_lost.swap(session, Ordering::Relaxed) != session {
                        tracing::warn!(stream = %self.id, i, session, error = %e, "the encoder session is gone: new ones");
                        self.rebuild.notify_one();
                    }
                }
                _ => tracing::warn!(stream = %self.id, i, error = %e, "encode failed"),
            }
            if options.force_keyframe {
                coder.keyframe_submitted_us.store(0, Ordering::Relaxed);
            }
            let mut pending = coder.pending.lock();
            pending.keyframe |= options.force_keyframe;
            pending.refresh |= options.force_ltr_refresh;
            pending.acked.extend(options.acked_ltr.iter().copied());
        }
        if stale {
            let mut held = self.held.lock();
            let dropped = if held
                .as_ref()
                .is_some_and(|frame| frame.capture_ts_us == captured)
            {
                self.held_us.store(0, Ordering::Relaxed);
                self.owed.store(false, Ordering::Relaxed);
                held.take()
            } else {
                None
            };
            drop(held);
            drop(dropped);
            return Attempt::Nothing;
        }
        if failed {
            self.owed.store(true, Ordering::Relaxed);
            return Attempt::Failed;
        }
        if is_fresh {
            self.fed(fps);
        }
        Attempt::Sent
    }

    fn refine_due(&self, coder: &Coder, count: usize) -> Option<u64> {
        let period = period_us(self.fps.load(Ordering::Relaxed));
        let budget = (self
            .counters
            .encoder_bps
            .load(Ordering::Relaxed)
            .saturating_mul(REFINE_BUDGET_MS)
            / 8_000)
            .checked_div(u64::try_from(count).unwrap_or(1))
            .unwrap_or(0);
        coder.refine.lock().due_us(period, budget)
    }

    fn due_at(&self) -> u64 {
        Pace::resume(self.pace_us.load(Ordering::Relaxed)).due_at(self.fps.load(Ordering::Relaxed))
    }

    fn repair_at(&self) -> Option<u64> {
        let captured = self.held_us.load(Ordering::Relaxed);
        if captured == 0 {
            return None;
        }
        let count = self.coding.lock().count();
        let (mut keyframe, mut refresh, mut refine) = (false, false, None::<u64>);
        for (_i, coder) in self.coders(count) {
            {
                let pending = coder.pending.lock();
                keyframe |= pending.keyframe;
                refresh |= pending.refresh;
            }
            if let Some(due) = self.refine_due(coder, count) {
                refine = Some(refine.map_or(due, |at| at.min(due)));
            }
        }
        repair_at(
            Asks {
                owed: self.owed.load(Ordering::Relaxed),
                refresh,
                keyframe,
                refine,
            },
            captured,
            self.due_at(),
            self.fps_ceiling.load(Ordering::Relaxed),
        )
    }

    fn on_session_packet(&self, index: usize, session: u64, packet: &EncodedPacket) {
        if self.coder(index).session.load(Ordering::Relaxed) != session {
            tracing::debug!(stream = %self.id, index, session, "a replaced session's frame dropped");
            return;
        }
        self.on_packet(index, packet);
    }

    fn on_packet(&self, index: usize, packet: &EncodedPacket) {
        let now = now::<P>();
        let coder = self.coder(index);
        let bytes = u64::try_from(packet.data.len()).unwrap_or(u64::MAX);
        let refined_at =
            coder
                .refine
                .lock()
                .returned(packet.pts_us, packet.mse.map(|mse| mse.luma), bytes);
        let returned = coder.returned(packet.pts_us, now);
        let (took, stripes) =
            returned.map_or((None, 0), |(took, frame)| (Some(took), frame.stripes));
        if let Some(took) = took.filter(|_| !packet.keyframe) {
            coder.refine.lock().took(took);
        }
        self.repair.notify_one();
        if refined_at.is_none() {
            let whole = if stripes.count_ones() > 1 {
                self.join
                    .lock()
                    .returned(index, packet.pts_us, took, packet.keyframe)
            } else {
                Some((took, packet.keyframe))
            };
            if let Some((took, keyframe)) = whole {
                if let Some(took) = took {
                    self.watch_encoder(took, keyframe);
                }
                self.counters.encoded.fetch_add(1, Ordering::Relaxed);
            }
        }
        if packet.keyframe {
            coder.keyframe_submitted_us.store(0, Ordering::Relaxed);
            let estimate = keyframe_estimate(coder.keyframe_bytes.load(Ordering::Relaxed), bytes);
            coder.keyframe_bytes.store(estimate, Ordering::Relaxed);
            coder.keyframe_deferred_us.store(0, Ordering::Relaxed);
        }
        if coder
            .ltr
            .lock()
            .on_packet(packet.keyframe, packet.ltr_token, now)
        {
            self.counters.ltr_offered.fetch_add(1, Ordering::Relaxed);
        }
        #[expect(clippy::cast_possible_truncation, reason = "the low bits by design")]
        let capture_ts_us = refined_at.unwrap_or(packet.pts_us) as u32;
        let frame = EncodedFrame {
            data: &packet.data,
            keyframe: packet.keyframe,
            ltr_token: packet.ltr_token,
            ltr_refresh: packet.ltr_refresh,
            capture_ts_us,
            stripes,
        };
        let max = self
            .sink
            .max_size()
            .map_or(MAX_DATAGRAM, |m| m.min(MAX_DATAGRAM));
        let mut packetizer = coder.packetizer.lock();
        packetizer.set_max_datagram(max);
        let cut = packetizer
            .packetize(&frame, send_ms_lo(now), |datagrams| {
                let _sent = self.send(datagrams);
            })
            .map(drop);
        drop(packetizer);
        if let Err(e) = cut {
            tracing::warn!(stream = %self.id, index, error = %e, "packetize failed");
        }
    }

    /// Send `datagrams` now; the bytes sent.
    fn send(&self, datagrams: &[Bytes]) -> usize {
        if datagrams.is_empty() {
            return 0;
        }
        self.last_push_us.store(now::<P>(), Ordering::Relaxed);
        if self.sink.send(datagrams).is_err() {
            return 0;
        }
        self.counters.datagrams.fetch_add(
            u64::try_from(datagrams.len()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        datagrams.iter().map(Bytes::len).sum()
    }

    /// A client's report on coder `index`: references, parity, and (coder 0's only) the rate.
    fn report(
        &self,
        index: usize,
        report: &ReceiverReport,
        path: Option<PathSample>,
    ) -> Option<Decision> {
        let coder = self.coder(index);
        let acked = {
            let mut ltr = coder.ltr.lock();
            let acked: Vec<u64> = report
                .acked_ltr
                .iter()
                .take(usize::from(report.acked_ltr_len))
                .copied()
                .filter(|&token| ltr.on_ack(token))
                .collect();
            drop(ltr);
            let count = u64::try_from(acked.len()).unwrap_or(u64::MAX);
            coder.pending.lock().acked.extend(acked);
            count
        };
        self.counters.ltr_acked.fetch_add(acked, Ordering::Relaxed);
        let sent_total = coder.packetizer.lock().datagrams_sent();
        let previous = coder.sent_at_report.swap(sent_total, Ordering::Relaxed);
        let sent = u32::try_from(sent_total.saturating_sub(previous)).unwrap_or(u32::MAX);
        let permille = coder.redundancy.lock().on_report(report, sent);
        if index != 0 {
            coder.packetizer.lock().set_parity_permille(permille);
            return None;
        }
        let parity_moved = {
            let mut packetizer = coder.packetizer.lock();
            let moved = packetizer.parity_permille() != permille;
            packetizer.set_parity_permille(permille);
            moved
        };
        let decision = self.rate.lock().on_report(report, sent, path);
        let Some(decision) = decision else {
            if parity_moved {
                let target = self.rate.lock().target_bps();
                self.apply_bitrate(target);
            }
            return None;
        };
        tracing::debug!(
            stream = %self.id,
            verdict = ?decision.verdict,
            target_bps = decision.target_bps,
            capped = decision.capped,
            "rate decision"
        );
        if decision.changed {
            self.apply_bitrate(decision.target_bps);
            self.apply_cadence(decision.target_bps);
        }
        let moved = self.chroma.lock().update(decision.target_bps);
        if let Some(chroma) = moved {
            tracing::info!(stream = %self.id, ?chroma, target_bps = decision.target_bps, "chroma follows the rate");
            self.rebuild.notify_one();
        }
        (self.events)(StreamEvent::Screen(ScreenEvent::Rate {
            stream: self.id,
            target_bps: decision.target_bps,
            verdict: decision.verdict,
            capped: decision.capped,
        }));
        Some(decision)
    }

    /// Set the encoders' rate from the stream's target, less what parity takes.
    fn apply_bitrate(&self, target_bps: u32) {
        let parity = self.coders[0].packetizer.lock().parity_permille();
        let encoder = encoder_bps(target_bps, parity);
        self.counters
            .bitrate_bps
            .store(u64::from(target_bps), Ordering::Relaxed);
        self.counters
            .encoder_bps
            .store(u64::from(encoder), Ordering::Relaxed);
        let coding = self.coding.lock();
        let live: Vec<LiveCoder<P::Video>> = coding
            .live()
            .into_iter()
            .map(|(i, s)| (i, s, coding.share(i)))
            .collect();
        drop(coding);
        for (i, session, (rows, of)) in live {
            let share = u64::from(encoder)
                .saturating_mul(u64::from(rows))
                .checked_div(u64::from(of))
                .unwrap_or(0);
            let share = u32::try_from(share).unwrap_or(u32::MAX);
            if let Err(e) = session.set_bitrate(share) {
                tracing::debug!(stream = %self.id, i, error = %e, "bitrate refused");
            }
        }
    }

    /// Move to the frame rate `target_bps` affords, under the ceiling.
    fn apply_cadence(&self, target_bps: u32) {
        let moved = self.cadence.lock().update(target_bps);
        if let Some(fps) = moved {
            self.set_fps(fps.min(self.fps_ceiling.load(Ordering::Relaxed)));
        }
    }

    fn set_fps(&self, fps: u16) {
        let fps = fps.max(1);
        if self.fps.swap(fps, Ordering::Relaxed) == fps {
            return;
        }
        self.fed_fps.store(fps, Ordering::Relaxed);
        tracing::debug!(stream = %self.id, fps, "frame rate");
        for (i, session) in self.coding.lock().live() {
            if let Err(e) = session.set_frame_rate(fps) {
                tracing::debug!(stream = %self.id, i, error = %e, "frame rate refused");
            }
        }
    }

    fn lower_ceiling(&self, fps: u16, why: &str) {
        let ceiling = fps.max(1);
        if ceiling >= self.fps_ceiling.load(Ordering::Relaxed) {
            return;
        }
        tracing::info!(stream = %self.id, ceiling, why, "frame rate ceiling lowered");
        self.fps_ceiling.store(ceiling, Ordering::Relaxed);
        let current = self.fps.load(Ordering::Relaxed).min(ceiling);
        *self.cadence.lock() = Cadence::resume(ceiling, current);
        self.set_fps(current);
    }

    fn raise_ceiling(&self, fps: u16) {
        let ceiling = fps.min(self.fps_asked.load(Ordering::Relaxed)).max(1);
        if ceiling <= self.fps_ceiling.load(Ordering::Relaxed) {
            return;
        }
        self.fps_ceiling.store(ceiling, Ordering::Relaxed);
        let current = self.fps.load(Ordering::Relaxed);
        *self.cadence.lock() = Cadence::resume(ceiling, current);
        let target = self.rate.lock().target_bps();
        self.apply_cadence(target);
    }

    /// A frame came back `took` after it went in.
    fn watch_encoder(&self, took: u64, keyframe: bool) {
        let fps = self.fps.load(Ordering::Relaxed);
        let slower = self.watch.lock().returned(took, fps, keyframe);
        if let Some(slower) = slower {
            self.lower_ceiling(slower, "the encoder fell behind");
        }
    }

    /// A fresh capture went into the encoder at `fps`.
    fn fed(&self, fps: u16) {
        let verdict = self.watch.lock().fed(fps);
        let Some(Fed { fps: fed, ceiling }) = verdict else {
            return;
        };
        self.fed_fps.store(fed.max(1), Ordering::Relaxed);
        let Some(ceiling) = ceiling else { return };
        let now = ceiling.max(1);
        let current = self.fps_ceiling.load(Ordering::Relaxed);
        if now < current {
            self.lower_ceiling(now, "the encoder is fed less than the rung");
        } else if now > current {
            self.raise_ceiling(now);
        }
    }

    fn dropped(&self, now: u64, count: usize) {
        self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        for (_i, coder) in self.coders(count) {
            if self.standalone_fits(coder, now) {
                coder.pending.lock().refresh = true;
            }
        }
    }

    fn keyframe_admitted(&self, coder: &Coder, now: u64) -> bool {
        !coder.ltr_usable() || self.standalone_fits(coder, now)
    }

    fn standalone_fits(&self, coder: &Coder, now: u64) -> bool {
        if keyframe_fits(
            coder.keyframe_bytes.load(Ordering::Relaxed),
            self.sink.held(),
            self.counters.bitrate_bps.load(Ordering::Relaxed),
        ) {
            coder.keyframe_deferred_us.store(0, Ordering::Relaxed);
            return true;
        }
        match coder.keyframe_deferred_us.compare_exchange(
            0,
            now.max(1),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_started) => {
                self.counters
                    .keyframes_deferred
                    .fetch_add(1, Ordering::Relaxed);
                tracing::debug!(stream = %coder.media, "a standalone picture deferred: the link cannot drain one");
                false
            }
            Err(started) => now.saturating_sub(started) >= KEYFRAME_VALVE_US,
        }
    }

    fn request_refresh(&self, index: usize, last_good_frame: u32, keyframe: bool) {
        let coder = self.coder(index);
        tracing::debug!(stream = %coder.media, last_good_frame, keyframe, "refresh requested");
        self.counters.refreshes.fetch_add(1, Ordering::Relaxed);
        let submitted = coder.keyframe_submitted_us.load(Ordering::Relaxed);
        if submitted != 0 && now::<P>().saturating_sub(submitted) < KEYFRAME_IN_FLIGHT_US {
            tracing::debug!(stream = %coder.media, "the keyframe being coded answers the refresh");
            return;
        }
        if keyframe {
            coder.ltr.lock().client_lost();
            coder.pending.lock().keyframe = true;
        } else {
            coder.pending.lock().refresh = true;
        }
        self.repair.notify_one();
    }

    fn echo_clock(&self, sent_us: u64, arrived: Instant) {
        let waited = instant().saturating_duration_since(arrived);
        let echoed_us = now::<P>();
        let waited_us = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
        let received_us = echoed_us.saturating_sub(waited_us);
        let echo = ClockEcho::new(sent_us, received_us, echoed_us);
        let _sent = self.send(&[echo.datagram(self.id, send_ms_lo(echoed_us))]);
    }

    fn nack(&self, index: usize, frame: u32, fragments: &[u16]) {
        let coder = self.coder(index);
        if !self.frame_fits() {
            tracing::debug!(stream = %coder.media, frame, "a nack unanswered: the link holds frames");
            return;
        }
        let datagrams = coder.packetizer.lock().retransmit(frame, fragments);
        if datagrams.is_empty() {
            tracing::debug!(stream = %coder.media, frame, "a nack for a frame outside the history");
        }
        let _sent = self.send(&datagrams);
    }

    fn set_quality(&self, quality: Quality) {
        let refresh_hz = Source::<P>::refresh_hz(*self.target.lock());
        let native = self.coding.lock().native;
        let (_capture, encoder) = configs(native, &quality, refresh_hz, Chroma::Subsampled);
        let before = {
            let mut desired = self.desired.lock();
            std::mem::replace(&mut desired.quality, quality)
        };
        self.fps_asked.store(encoder.fps, Ordering::Relaxed);
        self.rate.lock().set_max(quality.bitrate_bps.max(100_000));
        let ceiling = self.fps_ceiling.load(Ordering::Relaxed);
        if encoder.fps < ceiling {
            self.lower_ceiling(encoder.fps, "the client asked for fewer frames");
        } else {
            self.raise_ceiling(encoder.fps);
        }
        let target = self.rate.lock().target_bps();
        self.apply_bitrate(target);
        let chroma = {
            let mut gate = self.chroma.lock();
            let was = gate.chroma();
            (
                was,
                gate.ask(quality.chroma, (encoder.width, encoder.height), target),
            )
        };
        let reshaped = before.codec != quality.codec
            || (scale_of(&before) - scale_of(&quality)).abs() > f64::EPSILON
            || chroma.0 != chroma.1;
        if reshaped {
            self.rebuild.notify_one();
        }
    }

    /// Read the display's bounds now, for the cursor and the input.
    fn follow_bounds(&self) {
        let target = *self.target.lock();
        let bounds = Source::<P>::target_bounds(target);
        let changed = {
            let mut now = self.bounds.lock();
            std::mem::replace(&mut *now, bounds) != bounds
        };
        if changed || bounds.is_some() {
            self.input.lock().set_bounds(bounds, instant());
        }
        let quality = self.desired.lock().quality;
        let scale = self.coding.lock().point_scale * scale_of(&quality);
        self.pixels_per_point
            .store(scale.to_bits(), Ordering::Relaxed);
    }

    /// Build sessions for what is desired now, at the open or after a change, and install them.
    /// Returns the stripes as the client is told them.
    async fn build(self: &Arc<Self>) -> Result<Vec<Stripe>, ScreenError> {
        let quality = self.desired.lock().quality;
        let native = self.coding.lock().native;
        let refresh_hz = Source::<P>::refresh_hz(*self.target.lock());
        let target = self.rate.lock().target_bps();
        let chroma = {
            let (_c, e) = configs(native, &quality, refresh_hz, Chroma::Subsampled);
            self.chroma
                .lock()
                .ask(quality.chroma, (e.width, e.height), target)
        };
        let (mut capture, mut encoder) = configs(native, &quality, refresh_hz, chroma);
        let mut layout = striping(&capture);
        let first = self
            .try_build(encoder, (capture.width, capture.height), layout)
            .await;
        let built = match first {
            Err(e) if chroma == Chroma::Full => {
                tracing::warn!(stream = %self.id, error = %e, "no 4:4:4 session here: 4:2:0");
                self.chroma.lock().refuse();
                (capture, encoder) = configs(native, &quality, refresh_hz, Chroma::Subsampled);
                layout = striping(&capture);
                self.try_build(encoder, (capture.width, capture.height), layout)
                    .await?
            }
            Err(e) if layout.is_some() => {
                tracing::warn!(stream = %self.id, error = %e, "no stripes here: one picture");
                layout = None;
                self.try_build(encoder, (capture.width, capture.height), layout)
                    .await?
            }
            built => built?,
        };
        let old = {
            let mut coding = self.coding.lock();
            let old = std::mem::replace(&mut coding.sessions, built.0);
            coding.layout = layout;
            coding.capture = capture;
            coding.encoder = encoder;
            old
        };
        for (coder, session) in self.coders.iter().zip(built.1) {
            coder.session.store(session, Ordering::Relaxed);
            coder.rebuilt();
        }
        self.encoder_lost.store(0, Ordering::Relaxed);
        for session in old {
            retire::<P>(session);
        }
        self.fps.store(encoder.fps, Ordering::Relaxed);
        self.fps_ceiling.store(encoder.fps, Ordering::Relaxed);
        *self.cadence.lock() = Cadence::new(encoder.fps);
        self.apply_bitrate(target);
        self.apply_cadence(target);
        self.repair.notify_one();
        Ok(told_stripes(self.id, layout.as_ref()))
    }

    /// New sessions on a blocking thread: VideoToolbox takes tens of milliseconds to make one.
    async fn try_build(
        self: &Arc<Self>,
        encoder: EncoderConfig,
        shown: (u32, u32),
        layout: Option<[CodedStripe; 2]>,
    ) -> Result<([Option<Arc<P::Video>>; Stripe::MAX], [u64; Stripe::MAX]), CodecError> {
        let weak = Arc::downgrade(self);
        let numbers = [
            self.sessions
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1),
            self.sessions
                .fetch_add(1, Ordering::Relaxed)
                .wrapping_add(1),
        ];
        let built = blocking::<P, _>(move || match layout {
            None => {
                let whole = open_session::<P>(&weak, encoder, shown, numbers[0], 0, None)?;
                Ok([Some(Arc::new(whole)), None])
            }
            Some([top, lower]) => {
                let top = open_session::<P>(&weak, encoder, shown, numbers[0], 0, Some(top))?;
                let lower = open_session::<P>(&weak, encoder, shown, numbers[1], 1, Some(lower))?;
                Ok([Some(Arc::new(top)), Some(Arc::new(lower))])
            }
        })
        .await
        .unwrap_or(Err(CodecError::Os {
            call: "the session build's thread",
            status: -1,
        }))?;
        Ok((built, numbers))
    }

    /// The stream's shape changed (scale, codec, chroma, stripes, display): new capture
    /// configuration and sessions. Returns the `Geometry` to tell.
    async fn reconfigure(self: &Arc<Self>) -> Result<ScreenEvent, ScreenError> {
        let before = self.coding.lock().capture;
        let stripes = self.build().await?;
        let capture = self.coding.lock().capture;
        let updated = (capture != before)
            .then(|| {
                self.capture.lock().as_ref().map(|running| {
                    let (tx, rx) = oneshot::channel();
                    Source::<P>::update(running, &capture, move |done| {
                        let _gone = tx.send(done);
                    });
                    rx
                })
            })
            .flatten();
        if let Some(updated) = updated {
            updated
                .await
                .unwrap_or_else(|_| Err(CaptureError::Stopped("never answered".to_owned())))?;
        }
        self.follow_bounds();
        Ok(ScreenEvent::Geometry {
            stream: self.id,
            width: capture.width,
            height: capture.height,
            stripes,
        })
    }
}

/// Codes the held capture again when it is owed, asked for, or refinable.
async fn repair_loop<P: Platform>(shared: Arc<Shared<P>>) {
    while !shared.sink.is_closed() && !shared.closed.load(Ordering::Acquire) {
        let Some(at) = shared.repair_at() else {
            shared.repair.notified().await;
            continue;
        };
        let now = now::<P>();
        if at > now {
            let wait = Duration::from_micros(at.saturating_sub(now));
            let _woken = tokio::time::timeout(wait, shared.repair.notified()).await;
            continue;
        }
        let period = period_us(shared.fps.load(Ordering::Relaxed));
        let repairing = Arc::clone(&shared);
        let Some(attempt) = blocking::<P, _>(move || repairing.try_encode(None, now)).await else {
            break;
        };
        let wait = match attempt {
            Attempt::Sent | Attempt::Nothing => continue,
            Attempt::NotDue => shared.due_at().saturating_sub(now).max(1_000),
            Attempt::NoRoom | Attempt::Failed => period,
        };
        let _woken =
            tokio::time::timeout(Duration::from_micros(wait), shared.repair.notified()).await;
    }
}

/// A heartbeat after [`HEARTBEAT_AFTER`] of silence, so the client tells a still screen from a
/// dead link.
async fn beat_loop<P: Platform>(shared: Arc<Shared<P>>) {
    let after_us = u64::try_from(HEARTBEAT_AFTER.as_micros()).unwrap_or(u64::MAX);
    let mut last_beat_us: u64 = 0;
    while !shared.sink.is_closed() && !shared.closed.load(Ordering::Acquire) {
        let now = now::<P>();
        let last_out = shared
            .last_push_us
            .load(Ordering::Relaxed)
            .max(last_beat_us);
        let silence = now.saturating_sub(last_out);
        if silence < after_us {
            tokio::time::sleep(Duration::from_micros(after_us.saturating_sub(silence))).await;
            continue;
        }
        last_beat_us = now;
        shared.counters.heartbeats.fetch_add(1, Ordering::Relaxed);
        let next_frame = shared.coders[0].packetizer.lock().next_frame();
        let _sent = shared.send(&[heartbeat_datagram(shared.id, next_frame, send_ms_lo(now))]);
    }
}

/// The pointer over the display at `at` (points), as stream pixels and whether it is on it.
fn cursor_sample(rect: Rect, at: (f64, f64), pixels_per_point: f64) -> (i32, i32, bool) {
    let to_pixels = |v: f64| -> i32 {
        #[expect(clippy::cast_possible_truncation, reason = "clamped")]
        let p = (v * pixels_per_point).round().clamp(-1.0e6, 1.0e6) as i32;
        p
    };
    (
        to_pixels(at.0 - rect.x),
        to_pixels(at.1 - rect.y),
        rect.contains(at.0, at.1),
    )
}

/// Where the stream's input placed the pointer, when it leaves the real one alone.
fn placed_sample(
    pointer: &PointerWatch,
    rect: Rect,
    pixels_per_point: f64,
) -> Option<(i32, i32, bool)> {
    match pointer.get() {
        Pointer::Real => None,
        Pointer::Placed(None) => Some((0, 0, false)),
        Pointer::Placed(Some(at)) => Some(cursor_sample(rect, at, pixels_per_point)),
    }
}

async fn cursor_event(placed: &mut PointerChanges) {
    tokio::select! {
        biased;
        () = placed.changed() => {}
        () = tokio::time::sleep(CURSOR_BACKSTOP) => {}
    }
}

/// The pointer's position, as its own datagrams at up to 120 Hz while it moves.
async fn cursor_loop<P: Platform>(shared: Arc<Shared<P>>) {
    let pointer = shared.input.lock().pointer();
    let mut placed = pointer.changes();
    let mut ticks = tokio::time::interval(CURSOR_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut seen: Option<(u32, (f64, f64))> = None;
    let mut last: Option<(i32, i32, bool)> = None;
    let mut seq: u32 = 0;
    while !shared.sink.is_closed() && !shared.closed.load(Ordering::Acquire) {
        let Some(rect) = *shared.bounds.lock() else {
            cursor_event(&mut placed).await;
            continue;
        };
        let pixels_per_point = f64::from_bits(shared.pixels_per_point.load(Ordering::Relaxed));
        let placed_at = placed_sample(&pointer, rect, pixels_per_point);
        let sample = if let Some(placed) = placed_at {
            placed
        } else {
            let moves = Source::<P>::pointer_moves();
            let at = match seen {
                Some((count, at)) if count == moves => at,
                _moved_or_unread => {
                    let Some(at) = blocking::<P, _>(Source::<P>::pointer_location).await else {
                        return;
                    };
                    seen = Some((moves, at));
                    at
                }
            };
            cursor_sample(rect, at, pixels_per_point)
        };
        if last != Some(sample) {
            last = Some(sample);
            seq = seq.wrapping_add(1);
            let datagram = cursor_datagram(
                shared.id,
                seq,
                send_ms_lo(now::<P>()),
                sample.0,
                sample.1,
                sample.2,
            );
            let _sent = shared.send(&[datagram]);
        }
        if placed_at.is_some() {
            cursor_event(&mut placed).await;
        } else {
            ticks.tick().await;
        }
    }
}

/// The cursor's shape, as an event each time it changes.
async fn shape_loop<P: Platform>(shared: Arc<Shared<P>>) {
    let mut ticks = tokio::time::interval(SHAPE_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut seed: Option<Option<i32>> = None;
    let mut told: Option<Option<CursorShape>> = None;
    let mut read_at: Option<Instant> = None;
    while !shared.sink.is_closed() && !shared.closed.load(Ordering::Acquire) {
        ticks.tick().await;
        let now = Source::<P>::cursor_seed();
        // With no seed to tell a change by, the shape is read once a backstop.
        let unseeded_recently = now.is_none()
            && read_at.is_some_and(|at| instant().saturating_duration_since(at) < CURSOR_BACKSTOP);
        if (seed == Some(now) && now.is_some()) || unseeded_recently {
            continue;
        }
        seed = Some(now);
        read_at = Some(instant());
        let Some(shape) = blocking::<P, _>(Source::<P>::cursor_shape).await else {
            return;
        };
        if told.as_ref() == Some(&shape) {
            continue;
        }
        told = Some(shape.clone());
        (shared.events)(StreamEvent::Screen(ScreenEvent::Cursor {
            stream: shared.id,
            shape,
        }));
    }
}

/// The source's state, as an event each time it changes.
async fn source_loop<P: Platform>(shared: Arc<Shared<P>>) {
    let mut tracker = SourceTracker::new(instant());
    let mut ticks = tokio::time::interval(SOURCE_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !shared.sink.is_closed() && !shared.closed.load(Ordering::Acquire) {
        ticks.tick().await;
        if let Some(Some(console)) = blocking::<P, _>(Source::<P>::console).await {
            tracker.set_console(console);
        }
        let encoded = shared.counters.encoded.load(Ordering::Relaxed);
        if let Some(state) = tracker.poll(encoded, instant()) {
            (shared.events)(StreamEvent::Screen(ScreenEvent::Source {
                stream: shared.id,
                state,
            }));
        }
    }
}

/// The display's bounds every [`GEOMETRY_PERIOD`], and new sessions when the stream's shape
/// changed or the encoder was lost.
async fn geometry_loop<P: Platform>(shared: Arc<Shared<P>>) {
    let mut ticks = tokio::time::interval(GEOMETRY_PERIOD);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    while !shared.sink.is_closed() && !shared.closed.load(Ordering::Acquire) {
        tokio::select! {
            biased;
            _ = ticks.tick() => {
                let follow = Arc::clone(&shared);
                let _read = blocking::<P, _>(move || follow.follow_bounds()).await;
            }
            () = shared.rebuild.notified() => {
                match shared.reconfigure().await {
                    Ok(geometry) => (shared.events)(StreamEvent::Screen(geometry)),
                    Err(e) => tracing::warn!(stream = %shared.id, error = %e, "new sessions refused"),
                }
            }
        }
    }
}
