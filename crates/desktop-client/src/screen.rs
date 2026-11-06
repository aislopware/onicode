//! One displayed stream on this device: reassembly, hardware decode, stripes, feedback and the
//! picture handed to whatever shows it.
//!
//! The link's datagram reader routes every `MEDIA` datagram through a [`ScreenRouter`];
//! [`spawn_screen`] runs one task per stream that reassembles, NACKs and asks for refreshes on
//! the reassembler's schedule, hands complete frames to a decode thread per lane, reports every
//! 50 ms and probes desktopd's capture clock, all as `FEEDBACK` datagrams. Each picture goes to
//! the [`Present`] hook on the decoder's thread the moment it comes out (present-on-decode: an
//! `AVSampleBufferDisplayLayer` or a `CAMetalLayer` of the view's own waits for no UI frame),
//! and onto a newest-only channel.
//!
//! Ported from slopty's client `screen.rs` (MIT, same authors), whole displays only: no region
//! a picture shows part of, no window.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use onicode_desktop_media::wire::{
    ClockEcho, Feedback, Kind, MediaHeader, ReceiverReport, StreamId, VideoCodec, flags,
};
use onicode_desktop_media::{
    Action, CaptureClock, Captured, ClockEstimate, ClockSync, Config, FrameStamp, GlassStats,
    Ingest, Offer, Pacer, PacingStats, PaintRate, Reassembler, ReassemblerStats, STALL_GAP,
    StallAttribution,
};
use onicode_desktop_proto::screen::{CursorShape, Stripe};
use parking_lot::Mutex;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use crate::decode::{DecodedFrame, Image, Outcome, OutcomeSink, Picture, Refused, VideoDecode};
use crate::router::{Arrival, Attached, ScreenRouter};
use crate::{RuntimeClock, instant};

/// How often a receiver report goes to desktopd.
pub const REPORT_EVERY: Duration = Duration::from_millis(50);
/// Reassembler timer resolution while frames are pending.
const TICK: Duration = Duration::from_millis(2);
/// Timer period while nothing is pending: half the stall gap, so the stall detector can tell a
/// silence it watched from one it slept through.
const IDLE_TICK: Duration = Duration::from_millis(25);

/// Clock probes sent one a report as a stream starts, so the first pictures are timed from
/// their capture within a few round trips; after these, one every [`PROBE_EVERY`] reports.
const PROBES_AT_ONCE: u64 = 8;
/// Reports between clock probes once the estimate has its first: four a second.
const PROBE_EVERY: u64 = 5;

/// RTT assumed before the transport has measured one.
const DEFAULT_RTT: Duration = Duration::from_millis(20);

/// Frames remembered while the decoder works on them. VideoToolbox gives its callback nothing
/// but the presentation timestamp, so the stream parks the frame's timing under it.
const ARRIVALS: usize = 128;

/// How long the stitch waits for a capture's other stripe once one has decoded: a display
/// refresh. Past it the capture goes up with that stripe's previous picture, a seam tear.
const STITCH_WAIT: Duration = Duration::from_micros(16_667);

/// Frames a lane's decode thread may have waiting before the lane drops the next itself. A
/// submission returns in about a tenth of a millisecond, so one this deep is a decoder that has
/// stopped; [`DECODE_STUCK`] gives it up.
const DECODE_QUEUE: usize = 32;
/// How long one submission may stay inside the decoder before it is given up on and another
/// built for a keyframe (slopty measured one that never returned on a hosted virtual Mac).
const DECODE_STUCK: Duration = Duration::from_secs(2);
/// The most of a submission's time inside the decoder one report charges it: two report
/// periods. A report later than that found the stream task itself held up.
const STUCK_CREDIT: Duration = Duration::from_millis(100);
/// The longest [`DECODE_STUCK`] grows to as decoders in a row are given up on without a
/// picture: each leaves a thread waiting inside the platform.
const DECODE_STUCK_MAX: Duration = Duration::from_secs(32);

/// A decoded picture together with the timing that got it here.
#[derive(Debug)]
pub struct Presentable<P = Picture> {
    /// The picture: the whole of it, or for a striped stream the top stripe's.
    pub frame: DecodedFrame<P>,
    /// When the completing datagram arrived, when the decoder returned it, and when the Mac
    /// captured it, as far as the clock probes place that.
    pub stamp: FrameStamp,
    /// For a striped stream, the lower stripe and where the two meet; `None` for one picture.
    pub stripes: Option<Stitched<P>>,
}

/// The two stripes of one capture, each decoded by a session of its own, to be shown one over
/// the other with no copy between them.
///
/// Each stripe's picture holds the rows it codes, which run
/// [`onicode_desktop_codec::stripes::OVERLAP`] past the seam into the other's. The top one
/// ([`Presentable::frame`]) shows its first [`Self::top_rows`]; the lower one shows its rows
/// from [`Self::lower_from`] down, right under them.
#[derive(Clone, Debug)]
pub struct Stitched<P = Picture> {
    /// The lower stripe's picture.
    pub lower: DecodedFrame<P>,
    /// Rows of the top stripe's picture shown: the rows above the seam.
    pub top_rows: u32,
    /// The first row of the lower stripe's picture shown.
    pub lower_from: u32,
}

impl<P: Image> Presentable<P> {
    /// The picture's size in pixels, both stripes together.
    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        let width = self.frame.image.width();
        match &self.stripes {
            None => (width, self.frame.image.height()),
            Some(stitched) => {
                let top = usize::try_from(stitched.top_rows).unwrap_or(usize::MAX);
                let from = usize::try_from(stitched.lower_from).unwrap_or(usize::MAX);
                (
                    width,
                    top.saturating_add(stitched.lower.image.height().saturating_sub(from)),
                )
            }
        }
    }
}

/// Shows a decoded picture on the decoder's own thread, the moment it comes out
/// ([`ScreenHandle::set_present`]).
pub type Present<P = Picture> = Arc<dyn Fn(&Presentable<P>) + Send + Sync>;

/// Where the decoder's thread finds the [`Present`], if one is set.
struct PresentSlot<P>(Mutex<Option<Present<P>>>);

impl<P> Default for PresentSlot<P> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<P> std::fmt::Debug for PresentSlot<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PresentSlot")
            .field(&self.0.lock().is_some())
            .finish()
    }
}

/// How a frame was coded, from its prefix: the stripes coded from its capture (zero for one
/// picture) and the build of desktopd's sessions that coded it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Coded {
    stripes: u8,
    build: u8,
}

/// A frame handed to the decoder, parked by presentation timestamp for its callback.
#[derive(Clone, Copy, Debug)]
struct Parked {
    pts_us: u64,
    arrived: Instant,
    ltr_token: Option<u64>,
    /// A keyframe or an LTR refresh: the frame that restarts decoding.
    restarts: bool,
    /// Nothing later refers to it: failing on it leaves every reference intact.
    discardable: bool,
    coded: Coded,
}

/// What the decoder leaves for the stream task: the frames it is working on, the long-term
/// references it really decoded, and whether it failed or refused anything.
#[derive(Debug, Default)]
struct Inflight {
    parked: VecDeque<Parked>,
    /// Tokens of frames the decoder returned a picture for, not yet given to the reassembler.
    acks: Vec<u64>,
    failed: Option<Failed>,
    /// Failures on frames nothing refers to, which need no refresh.
    failed_discardable: u64,
    /// Frames refused as they were submitted, since the task last looked.
    refused: u64,
    /// The refresh those refusals ask for, `Some(keyframe)` ([`refresh_after_refusal`]).
    refusal_refresh: Option<bool>,
    /// The newest failure's status, until the task takes it.
    status: Option<Status>,
}

/// A failure's status, as [`ScreenStats::decode_failure`] gives it.
#[derive(Clone, Copy, Debug)]
struct Status(Option<i32>);

/// Failures since the task last looked, folded into one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Failed {
    /// The newest failed frame's timestamp.
    pts_us: u64,
    /// Any of them lost the session, so only a keyframe helps.
    session_lost: bool,
    /// Any of them was a frame that restarts decoding: only a keyframe helps.
    restart: bool,
    count: u64,
}

impl Inflight {
    fn park(&mut self, parked: Parked) {
        if self.parked.len() >= ARRIVALS {
            self.parked.pop_front();
        }
        self.parked.push_back(parked);
    }

    /// The frame with this timestamp, and everything older forgotten with it.
    fn take(&mut self, pts_us: u64) -> Option<Parked> {
        let at = self.parked.iter().position(|p| p.pts_us == pts_us)?;
        self.parked.drain(..=at).next_back()
    }

    /// Forget the frame with this timestamp alone: it never reached the decoder.
    fn unpark(&mut self, pts_us: u64) {
        if let Some(at) = self.parked.iter().rposition(|p| p.pts_us == pts_us) {
            let _gone = self.parked.remove(at);
        }
    }

    /// The decoder returned this frame's picture: its arrival and how it was coded, and its
    /// token is now held.
    fn decoded(&mut self, pts_us: u64) -> Option<(Instant, Coded)> {
        let parked = self.take(pts_us)?;
        self.acks.extend(parked.ltr_token);
        Some((parked.arrived, parked.coded))
    }

    /// The decoder returned no picture for this frame: its token is never acknowledged. A frame
    /// nothing refers to, failed with the session intact, asks for nothing.
    fn failed(&mut self, pts_us: u64, status: i32, session_lost: bool) {
        self.status = Some(Status(Some(status)));
        let parked = self.take(pts_us);
        if parked.is_some_and(|p| p.discardable) && !session_lost {
            self.failed_discardable = self.failed_discardable.saturating_add(1);
            return;
        }
        let restarts = parked.is_some_and(|p| p.restarts);
        let before = self.failed.unwrap_or(Failed {
            pts_us: 0,
            session_lost: false,
            restart: false,
            count: 0,
        });
        self.failed = Some(Failed {
            pts_us: before.pts_us.max(pts_us),
            session_lost: before.session_lost || session_lost,
            restart: before.restart || restarts,
            count: before.count.saturating_add(1),
        });
    }

    /// The frame with this timestamp was refused as it was submitted: it is forgotten, and
    /// `refresh` is what it asks for.
    fn refused(&mut self, pts_us: u64, status: Option<i32>, refresh: Option<bool>) {
        let _gone = self.take(pts_us);
        self.status = Some(Status(status));
        self.refused = self.refused.saturating_add(1);
        if let Some(keyframe) = refresh {
            self.refusal_refresh = Some(self.refusal_refresh.unwrap_or(false) || keyframe);
        }
    }
}

/// The Mac's pointer over a stream: where it is in stream pixels, and its picture.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CursorState {
    /// X, in stream pixels.
    pub x: i32,
    /// Y, in stream pixels.
    pub y: i32,
    /// Inside the streamed display.
    pub visible: bool,
    /// The pointer's picture, as desktopd last sent it; `None` for the system arrow (or none
    /// sent yet).
    pub shape: Option<Arc<CursorShape>>,
    /// Counts the shapes received, so a view redraws the pointer only when this moves.
    pub shape_serial: u64,
}

/// Receiver-side counters, refreshed with every report.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ScreenStats {
    /// Frames delivered to the decoder: of a striped stream, the top stripe's.
    pub frames: u64,
    /// Frames recovered by FEC.
    pub frames_fec: u64,
    /// Frames that needed a retransmission.
    pub frames_retransmit: u64,
    /// Frames given up on.
    pub frames_lost: u64,
    /// Frames nothing refers to that were skipped instead of repaired.
    pub frames_skipped: u64,
    /// Data fragments that never arrived.
    pub datagrams_lost: u64,
    /// Datagrams that arrived and were dropped before this stream read them (a full queue, or
    /// a backlog past its bound before the stream attached).
    pub datagrams_dropped: u64,
    /// Parity desktopd is sending, in thousandths of the data fragments, as observed.
    pub parity_permille: u16,
    /// Data fragments the frames seen so far were cut into.
    pub data_shards: u64,
    /// Parity fragments added to them.
    pub parity_shards: u64,
    /// NACKs sent.
    pub nacks: u64,
    /// Refreshes asked for.
    pub refreshes: u64,
    /// Refresh requests sent again while unanswered.
    pub refresh_repeats: u64,
    /// Decoder rejections, and frames dropped for a decoder a whole queue behind.
    pub decode_errors: u64,
    /// The platform's status for the newest rejection (zero for a frame dropped without naming
    /// an error); `None` before the first, and for a refusal that was not the platform's.
    pub decode_failure: Option<i32>,
    /// Frames in the decoders now.
    pub decoding: u64,
    /// Decoders given up on and built anew.
    pub decoders_replaced: u64,
    /// Datagrams seen.
    pub datagrams: u64,
    /// The stream is coded as two stripes now ([`Stitched`]).
    pub striped: bool,
    /// Captures of a striped stream that went up with one stripe's previous picture.
    pub seam_tears: u64,
    /// Bytes received in datagrams.
    pub bytes: u64,
    /// Stalls that released.
    pub stalls: u64,
    /// Time spent stalled, milliseconds.
    pub stalled_ms: u64,
    /// The link is stalled right now (as of the last report).
    pub stalled: bool,
    /// Where every silence past the stall gap went.
    pub silences: StallAttribution,
    /// Worst delay between the link's reader stamping a datagram and this task taking it.
    pub reader_lag_max: Duration,
    /// Datagrams this task read a stall gap or more after they arrived.
    pub reader_lag_over_gap: u64,
    /// When the first datagram of the stream arrived.
    pub first_datagram_at: Option<Instant>,
    /// When the first frame was complete and handed to the decoder.
    pub first_frame_at: Option<Instant>,
    /// When the first decoded picture came back.
    pub first_decoded_at: Option<Instant>,
    /// Longest wait from a frame's first fragment to its completion.
    pub hold_max: Duration,
    /// Median of that wait in the last report window.
    pub hold_p50: Duration,
    /// 95th percentile of the same.
    pub hold_p95: Duration,
    /// Interarrival jitter on desktopd's capture clock, as last reported.
    pub jitter: Duration,
    /// Frames held in order behind a missing one, as last reported.
    pub queue_depth: u8,
    /// desktopd's capture clock placed on this one by the clock probes; `None` until one has
    /// come back.
    pub clock: Option<ClockEstimate>,
    /// When the stream's task published these counters; `None` before its first report.
    pub reported_at: Option<Instant>,
    /// Pictures presented, skipped, repeated and late, from what the glass reported
    /// ([`ScreenHandle::presented`]).
    pub pacing: PacingStats,
    /// Capture-to-glass and input-to-glass, over the last pictures shown.
    pub glass: GlassStats,
    /// Pictures painted in the last second, and the ones that missed the display.
    pub paint_rate: PaintRate,
}

/// A live stream on this device. Clones share it; the last one dropped stops its task and
/// unroutes it. The session still closes the stream on desktopd.
pub struct ScreenHandle<P = Picture> {
    inner: Arc<Inner<P>>,
}

impl<P> Clone for ScreenHandle<P> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<P> std::fmt::Debug for ScreenHandle<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenHandle")
            .field("stream", &self.inner.stream)
            .finish_non_exhaustive()
    }
}

struct Inner<P> {
    stream: StreamId,
    frames: watch::Receiver<Option<Arc<Presentable<P>>>>,
    output: Arc<Output<P>>,
    cursor: Arc<watch::Sender<CursorState>>,
    stats: watch::Receiver<ScreenStats>,
    /// desktopd's capture target is producing pictures (shared with the task).
    source_live: Arc<AtomicBool>,
    router: ScreenRouter,
    task: JoinHandle<()>,
}

impl<P> Drop for Inner<P> {
    fn drop(&mut self) {
        self.task.abort();
        self.router.detach(self.stream);
    }
}

impl<P: Image> ScreenHandle<P> {
    /// The stream.
    #[must_use]
    pub fn stream(&self) -> StreamId {
        self.inner.stream
    }

    /// Newest picture; `changed().await` wakes when a new one lands. Only the newest is kept: a
    /// picture nobody got to show is stale by the time it would be.
    #[must_use]
    pub fn frames(&self) -> watch::Receiver<Option<Arc<Presentable<P>>>> {
        self.inner.frames.clone()
    }

    /// Hand every picture to `present` on the decoder's thread as it comes out, before
    /// [`Self::frames`] has it; `None` stops. Only pictures decoded from here on reach it.
    pub fn set_present(&self, present: Option<Present<P>>) {
        *self.inner.output.present.0.lock() = present;
    }

    /// The Mac's pointer over this stream.
    #[must_use]
    pub fn cursor(&self) -> watch::Receiver<CursorState> {
        self.inner.cursor.subscribe()
    }

    /// The picture stamped `stamp` that [`Present`] was handed reached the display at
    /// `presented_at`, or never did (`None`: a newer one replaced it first). Best effort: a
    /// view that cannot tell leaves the glass timings empty.
    pub fn presented(&self, stamp: FrameStamp, presented_at: Option<Instant>) {
        let mut pacer = self.inner.output.pacer.lock();
        match presented_at {
            Some(at) => pacer.shown(stamp, at),
            None => pacer.unshown(),
        }
    }

    /// The counters as of the last report, with the pacing as of now.
    #[must_use]
    pub fn stats(&self) -> ScreenStats {
        let mut stats = *self.inner.stats.borrow();
        let pacer = self.inner.output.pacer.lock();
        stats.pacing = pacer.stats();
        stats.glass = pacer.glass();
        stats.paint_rate = pacer.rate();
        stats
    }

    /// Whether desktopd's capture target is drawing, as last reported.
    #[must_use]
    pub fn source_live(&self) -> bool {
        self.inner.source_live.load(Ordering::Relaxed)
    }

    /// desktopd's `ScreenEvent::Source`: while the target draws nothing, the task stops asking
    /// for refreshes no frame could answer.
    pub(crate) fn set_source_live(&self, live: bool) {
        self.inner.source_live.store(live, Ordering::Relaxed);
    }

    /// desktopd's `ScreenEvent::Cursor`: the pointer's picture changed.
    pub(crate) fn set_cursor_shape(&self, shape: Option<CursorShape>) {
        self.inner.cursor.send_modify(|cursor| {
            cursor.shape = shape.map(Arc::new);
            cursor.shape_serial = cursor.shape_serial.wrapping_add(1);
        });
    }

    /// Whether the stream's task has ended (the link went).
    #[must_use]
    pub fn finished(&self) -> bool {
        self.inner.task.is_finished()
    }
}

/// What became of one feedback datagram.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Delivery {
    /// Handed to the link.
    Sent,
    /// Not sent, the link is still up (no room, too large): a report carries over.
    Dropped,
    /// The link is gone: the stream task ends.
    Gone,
}

/// How a stream task talks back to desktopd.
pub struct Uplink {
    /// Sends one `FEEDBACK` datagram's bytes (after the channel byte).
    pub feedback: Box<dyn Fn(Bytes) -> Delivery + Send>,
    /// The path's round trip; read once a report.
    pub rtt: Box<dyn Fn() -> Option<Duration> + Send>,
}

impl std::fmt::Debug for Uplink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Uplink").finish_non_exhaustive()
    }
}

/// Start receiving `stream` on `runtime`, decoding with `D`: reassembly, decode, reports and
/// NACK, refresh and clock feedback on `uplink`.
///
/// A striped stream's lower stripe comes on a media stream of its own ([`Stripe::media_of`]),
/// which the router hands this task with the stream's own: each is reassembled and decoded on
/// its own lane, and a stitch puts the two stripes of a capture up together.
#[must_use]
pub fn spawn_screen<D: VideoDecode>(
    runtime: &tokio::runtime::Handle,
    router: &ScreenRouter,
    stream: StreamId,
    codec: VideoCodec,
    uplink: Uplink,
) -> ScreenHandle<D::Image> {
    let Attached { datagrams, dropped } = router.attach(stream);
    let (frames_tx, frames) = watch::channel(None);
    let cursor = Arc::new(watch::Sender::new(CursorState::default()));
    let (stats_tx, stats) = watch::channel(ScreenStats::default());
    let source_live = Arc::new(AtomicBool::new(true));
    let output = Arc::new(Output {
        frames: frames_tx,
        present: PresentSlot::default(),
        decode_seq: AtomicU64::new(0),
        estimate: Mutex::new(None),
        first_decoded: Mutex::new(None),
        stitch: Mutex::new(Stitch::default()),
        pacer: Mutex::new(Pacer::new(RuntimeClock)),
    });
    let failed = Arc::new(Notify::new());
    let rtt = (uplink.rtt)().unwrap_or(DEFAULT_RTT);
    let top = Lane::<D>::new(Stripe::media_of(stream, 0), 0, codec, &output, &failed);
    let now = instant();
    let worker = Worker {
        stream,
        codec,
        datagrams,
        dropped,
        top,
        lower: None,
        top_whole: false,
        output: Arc::clone(&output),
        failed,
        feedback: uplink.feedback,
        path_rtt: uplink.rtt,
        rtt,
        cursor: Arc::clone(&cursor),
        stats: stats_tx,
        cursor_seq: None,
        counters: ScreenStats::default(),
        lower_counters: ReassemblerStats::default(),
        source_live: Arc::clone(&source_live),
        source_hint: true,
        capture_clock: CaptureClock::new(),
        clock: ClockSync::new(now),
        reports: 0,
        unstuck_at: now,
    };
    let task = runtime.spawn(worker.run());
    ScreenHandle {
        inner: Arc::new(Inner {
            stream,
            frames,
            output,
            cursor,
            stats,
            source_live,
            router: router.clone(),
            task,
        }),
    }
}

/// Where decoded pictures go, from every lane's decoder: the stitch, the pacer, the present
/// hook and the newest-picture channel.
struct Output<P> {
    frames: watch::Sender<Option<Arc<Presentable<P>>>>,
    present: PresentSlot<P>,
    /// Counted as pictures go up, so the pacer can tell how many never reached the glass.
    decode_seq: AtomicU64,
    /// Read on the decoder's thread for every picture, written by the task once an echo.
    estimate: Mutex<Option<ClockEstimate>>,
    /// When the first picture came back.
    first_decoded: Mutex<Option<Instant>>,
    stitch: Mutex<Stitch<DecodedFrame<P>>>,
    /// What went up and what the glass said of it.
    pacer: Mutex<Pacer<RuntimeClock>>,
}

impl<P: Image> Output<P> {
    /// Lane `index`'s decoder returned `frame`, which the datagram that completed it brought at
    /// `arrived`, coded as `coded` says.
    fn decoded(&self, index: usize, frame: DecodedFrame<P>, arrived: Instant, coded: Coded) {
        let decoded = instant();
        self.first_decoded.lock().get_or_insert(decoded);
        let captured = self
            .estimate
            .lock()
            .and_then(|e| Captured::by(&e, frame.pts_us));
        // Numbered as it goes up, in `show`: a capture's two stripes are one picture.
        let stamp = FrameStamp {
            pts_us: frame.pts_us,
            decode_seq: 0,
            arrived,
            decoded,
            captured,
        };
        let picture = if coded.stripes == 0 {
            if index != 0 {
                return;
            }
            self.stitch.lock().whole();
            Some(Presentable {
                frame,
                stamp,
                stripes: None,
            })
        } else {
            let joined = self
                .stitch
                .lock()
                .decoded(index, frame, stamp, coded, decoded);
            joined.map(Joined::presentable)
        };
        if let Some(picture) = picture {
            self.show(picture);
        }
    }

    /// A capture whose other stripe is late past [`STITCH_WAIT`] goes up at `now` without it.
    fn expire(&self, now: Instant) {
        let joined = self.stitch.lock().settle(now);
        if let Some(joined) = joined {
            self.show(joined.presentable());
        }
    }

    fn show(&self, mut picture: Presentable<P>) {
        picture.stamp.decode_seq = self.decode_seq.fetch_add(1, Ordering::Relaxed);
        {
            let mut pacer = self.pacer.lock();
            if pacer.offer(picture.stamp) == Offer::Drop {
                return;
            }
            // Presented on decode: the hook is the paint.
            let _painted = pacer.painted();
        }
        let picture = Arc::new(picture);
        let present = self.present.0.lock().clone();
        if let Some(present) = present {
            present(&picture);
        }
        let _no_receiver = self.frames.send(Some(picture));
    }
}

/// A stripe's newest picture, and the build of the sessions that coded it.
#[derive(Clone, Debug)]
struct Shown<F> {
    frame: F,
    stamp: FrameStamp,
    coded: Coded,
}

/// The capture the stitch waits on.
#[derive(Clone, Copy, Debug)]
struct Waiting {
    pts: u64,
    coded: Coded,
    /// When the first stripe not yet shown came out: a newer capture that replaces this one
    /// keeps it, so a stripe running behind never starves the picture.
    since: Instant,
}

/// Two stripes' pictures to show as one.
#[derive(Debug)]
struct Joined<F> {
    top: F,
    lower: F,
    stamp: FrameStamp,
}

impl<P> Joined<DecodedFrame<P>> {
    /// The top stripe's rows above the seam, the lower one's from the seam down.
    fn presentable(self) -> Presentable<P>
    where
        P: Image,
    {
        let overlap = onicode_desktop_codec::stripes::OVERLAP;
        let top_height = u32::try_from(self.top.image.height()).unwrap_or(u32::MAX);
        Presentable {
            frame: self.top,
            stamp: self.stamp,
            stripes: Some(Stitched {
                lower: self.lower,
                top_rows: top_height.saturating_sub(overlap),
                lower_from: overlap,
            }),
        }
    }
}

/// Puts the two stripes of a capture up together.
///
/// Each stripe's session decodes on its own thread. The frame prefix names the stripes coded
/// from a capture: once every one it names has decoded that capture, the two go up as one
/// picture. A stripe it does not name keeps the picture it has, and so does one whose media
/// stream waits for a refresh ([`Self::set_stalled`]). One that is merely late is waited for
/// [`STITCH_WAIT`]. Only stripes of one build of desktopd's sessions go up together.
#[derive(Debug)]
struct Stitch<F> {
    /// Each stripe's newest picture, top first.
    newest: [Option<Shown<F>>; Stripe::MAX],
    waiting: Option<Waiting>,
    /// Stripes whose media stream waits for a refresh, bit `i` for stripe `i`.
    stalled: u8,
    /// Captures that went up with a stripe's previous picture.
    tears: u64,
}

impl<F> Default for Stitch<F> {
    fn default() -> Self {
        Self {
            newest: [None, None],
            waiting: None,
            stalled: 0,
            tears: 0,
        }
    }
}

impl<F: Clone> Stitch<F> {
    /// A picture of the whole stream came out: the stream is not striped (any more).
    fn whole(&mut self) {
        *self = Self {
            tears: self.tears,
            ..Self::default()
        };
    }

    /// Whether stripe `index`'s media stream waits for a refresh.
    fn set_stalled(&mut self, index: usize, stalled: bool) {
        let bit = 1_u8 << index;
        if stalled {
            self.stalled |= bit;
        } else {
            self.stalled &= !bit;
        }
    }

    /// Stripe `index` decoded `frame`, of a capture coded as `coded` says, at `now`: the
    /// picture to put up, when this completed the capture waited on.
    fn decoded(
        &mut self,
        index: usize,
        frame: F,
        stamp: FrameStamp,
        coded: Coded,
        now: Instant,
    ) -> Option<Joined<F>> {
        let pts = stamp.pts_us;
        let behind = self
            .newest
            .iter()
            .flatten()
            .any(|shown| shown.stamp.pts_us > pts);
        if let Some(slot) = self.newest.get_mut(index) {
            *slot = Some(Shown {
                frame,
                stamp,
                coded,
            });
        }
        if !behind {
            let since = self.waiting.map_or(now, |waiting| waiting.since);
            self.waiting = Some(Waiting { pts, coded, since });
        }
        self.settle(now)
    }

    /// The capture waited on, once every stripe it names has decoded it, or the late ones have
    /// stalled, or [`STITCH_WAIT`] has passed at `now`.
    fn settle(&mut self, now: Instant) -> Option<Joined<F>> {
        let waiting = self.waiting?;
        let late = self.missing(waiting) & !self.stalled;
        if late != 0 && now.saturating_duration_since(waiting.since) < STITCH_WAIT {
            return None;
        }
        self.waiting = None;
        let joined = self.joined(waiting)?;
        if self.missing(waiting) != 0 {
            self.tears = self.tears.saturating_add(1);
        }
        Some(joined)
    }

    /// The stripes `waiting` names whose newest picture is not of its capture.
    fn missing(&self, waiting: Waiting) -> u8 {
        let mut missing = 0;
        for (i, newest) in self.newest.iter().enumerate() {
            let bit = 1_u8 << i;
            let has = newest
                .as_ref()
                .is_some_and(|shown| shown.stamp.pts_us == waiting.pts);
            if waiting.coded.stripes & bit != 0 && !has {
                missing |= bit;
            }
        }
        missing
    }

    /// Whether a capture waits for a stripe.
    const fn waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// The two stripes' newest pictures as one, stamped as `waiting`'s capture; `None` until
    /// both have a picture of its build.
    fn joined(&self, waiting: Waiting) -> Option<Joined<F>> {
        let [Some(top), Some(lower)] = &self.newest else {
            return None;
        };
        let fits = |shown: &Shown<F>| shown.coded.build == waiting.coded.build;
        if !fits(top) || !fits(lower) {
            return None;
        }
        let stamp = [top, lower]
            .into_iter()
            .find(|shown| shown.stamp.pts_us == waiting.pts)?
            .stamp;
        let arrived = if top.stamp.pts_us == lower.stamp.pts_us {
            top.stamp.arrived.max(lower.stamp.arrived)
        } else {
            stamp.arrived
        };
        Some(Joined {
            top: top.frame.clone(),
            lower: lower.frame.clone(),
            stamp: FrameStamp { arrived, ..stamp },
        })
    }
}

/// A complete frame on its way to the decode thread.
struct Submit {
    data: Bytes,
    pts_us: u64,
    /// Its number on the wire, for the log.
    frame: u32,
    discardable: bool,
    restarts: bool,
}

/// Where a lane's decoder leaves what it made of each frame, for the stream task to take.
struct Outcomes<P> {
    /// The lane: 0 for the whole picture or the top stripe.
    index: usize,
    inflight: Arc<Mutex<Inflight>>,
    /// Set once the decoder has left something in [`Self::inflight`].
    news: Arc<AtomicBool>,
    /// Wakes the task for a failure.
    wake: Arc<Notify>,
    output: Arc<Output<P>>,
}

impl<P> Clone for Outcomes<P> {
    fn clone(&self) -> Self {
        Self {
            index: self.index,
            inflight: Arc::clone(&self.inflight),
            news: Arc::clone(&self.news),
            wake: Arc::clone(&self.wake),
            output: Arc::clone(&self.output),
        }
    }
}

impl<P> Outcomes<P> {
    /// Something was left that asks for an answer: the task wakes for it.
    fn failed(&self) {
        self.news.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}

/// What a lane's decode thread shares with the stream task.
#[derive(Debug, Default)]
struct DecodeState {
    /// The number of the submission inside the decoder now, from one; zero while none is.
    submitting: AtomicU64,
    /// Frames handed to the thread and not yet taken by it.
    queued: AtomicUsize,
    /// The decoder has a session, as of its last submission.
    ready: AtomicBool,
    /// Given up on: nothing more is submitted to it, and what it returns is not shown.
    abandoned: AtomicBool,
    /// Pictures it returned.
    pictures: AtomicU64,
}

/// Where a lane's frames go into its decoder.
enum Feed {
    /// The decode thread's queue.
    Thread(std::sync::mpsc::SyncSender<Submit>),
    /// The decoder itself, submitted to in place ([`VideoDecode::INLINE`]).
    Inline(Box<dyn FnMut(Submit) + Send>),
}

/// A lane's decoder, on a thread of its own: a platform decode call can block its caller, and
/// must never take the runtime's thread with it. Dropping it lets the thread end. A decoder
/// that never blocks ([`VideoDecode::INLINE`]) has no thread: the stream task submits to it in
/// place, so its pictures come out at the instant their frame completed.
struct DecodeThread {
    feed: Feed,
    state: Arc<DecodeState>,
}

impl DecodeThread {
    /// A `D` for `media`'s frames, leaving what it makes of each in `outcomes`. A thread the
    /// system will not start leaves a decoder that is gone, which the lane replaces.
    fn start<D: VideoDecode>(
        media: StreamId,
        codec: VideoCodec,
        outcomes: &Outcomes<D::Image>,
    ) -> Self {
        let state = Arc::new(DecodeState::default());
        let sink: OutcomeSink<D::Image> = {
            let state = Arc::clone(&state);
            let Outcomes {
                index,
                inflight,
                news,
                wake,
                output,
            } = outcomes.clone();
            Box::new(move |outcome| {
                if state.abandoned.load(Ordering::Acquire) {
                    return;
                }
                let frame = match outcome {
                    Outcome::Picture(frame) => frame,
                    Outcome::Failed {
                        pts_us,
                        status,
                        session_lost,
                    } => {
                        inflight.lock().failed(pts_us, status, session_lost);
                        news.store(true, Ordering::Release);
                        wake.notify_one();
                        return;
                    }
                };
                state.pictures.fetch_add(1, Ordering::Relaxed);
                // A picture whose arrival is no longer parked is still shown; its timing simply
                // does not enter the ring.
                let parked = inflight.lock().decoded(frame.pts_us);
                news.store(true, Ordering::Release);
                let (arrived, coded) = parked.unwrap_or_else(|| (instant(), Coded::default()));
                output.decoded(index, frame, arrived, coded);
            })
        };
        if D::INLINE {
            let mut decoder = D::start(codec, sink);
            let (inline, outcomes) = (Arc::clone(&state), outcomes.clone());
            let mut submissions = 0_u64;
            let feed = Feed::Inline(Box::new(move |submit: Submit| {
                if inline.abandoned.load(Ordering::Acquire) {
                    return;
                }
                submissions = submissions.saturating_add(1);
                let _going = decode_one(
                    media,
                    &mut decoder,
                    &submit,
                    &inline,
                    &outcomes,
                    submissions,
                );
            }));
            return Self { feed, state };
        }
        let (submits, queue) = std::sync::mpsc::sync_channel::<Submit>(DECODE_QUEUE);
        let thread = Arc::clone(&state);
        let outcomes = outcomes.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("onicode-decode-{}", media.0))
            .spawn(move || {
                let decoder = D::start(codec, sink);
                decode_thread(media, decoder, &queue, &thread, &outcomes);
            });
        if let Err(e) = spawned {
            tracing::warn!(stream = media.0, error = %e, "no thread to decode on");
        }
        Self {
            feed: Feed::Thread(submits),
            state,
        }
    }

    /// Hand `submit` to the thread: the frame back when its queue is full, or the thread gone.
    /// An inline decoder takes it at once.
    fn submit(&mut self, submit: Submit) -> Result<(), std::sync::mpsc::TrySendError<Submit>> {
        let submits = match &mut self.feed {
            Feed::Inline(decode) => {
                decode(submit);
                return Ok(());
            }
            Feed::Thread(submits) => submits,
        };
        self.state.queued.fetch_add(1, Ordering::AcqRel);
        let sent = submits.try_send(submit);
        if sent.is_err() {
            self.state.queued.fetch_sub(1, Ordering::AcqRel);
        }
        sent
    }

    /// The thread has taken all but a few of what it was handed.
    fn has_room(&self) -> bool {
        self.state.queued.load(Ordering::Acquire) < DECODE_QUEUE / 2
    }

    fn ready(&self) -> bool {
        self.state.ready.load(Ordering::Acquire)
    }
}

impl Drop for DecodeThread {
    fn drop(&mut self) {
        self.state.abandoned.store(true, Ordering::Release);
    }
}

/// The decode thread's loop: submit each frame `queue` brings ([`decode_one`]). It ends once
/// its [`DecodeThread`] is dropped or given up on.
fn decode_thread<D: VideoDecode>(
    media: StreamId,
    mut decoder: D,
    queue: &std::sync::mpsc::Receiver<Submit>,
    state: &DecodeState,
    outcomes: &Outcomes<D::Image>,
) {
    let mut submissions = 0_u64;
    while let Ok(submit) = queue.recv() {
        state.queued.fetch_sub(1, Ordering::AcqRel);
        if state.abandoned.load(Ordering::Acquire) {
            break;
        }
        submissions = submissions.saturating_add(1);
        if !decode_one(media, &mut decoder, &submit, state, outcomes, submissions) {
            break;
        }
    }
}

/// Submit one frame, the `number`th, and leave a refusal in `outcomes`; `false` once the
/// decoder was given up on meanwhile.
fn decode_one<D: VideoDecode>(
    media: StreamId,
    decoder: &mut D,
    submit: &Submit,
    state: &DecodeState,
    outcomes: &Outcomes<D::Image>,
    number: u64,
) -> bool {
    state.submitting.store(number, Ordering::Release);
    let result = decoder.decode(&submit.data, submit.pts_us);
    state.submitting.store(0, Ordering::Release);
    let ready = decoder.ready();
    state.ready.store(ready, Ordering::Release);
    if state.abandoned.load(Ordering::Acquire) {
        return false;
    }
    let Err(Refused { status }) = result else {
        return true;
    };
    let refresh = refresh_after_refusal(submit.discardable, ready, submit.restarts);
    tracing::debug!(
        stream = media.0,
        frame = submit.frame,
        ?status,
        ?refresh,
        "decode refused"
    );
    outcomes
        .inflight
        .lock()
        .refused(submit.pts_us, status, refresh);
    outcomes.failed();
    true
}

/// One coded picture's way in: the whole picture, or one stripe of it, on its media stream.
struct Lane<D: VideoDecode> {
    media: StreamId,
    codec: VideoCodec,
    reassembler: Reassembler,
    decoder: DecodeThread,
    outcomes: Outcomes<D::Image>,
    /// Timestamp of the last frame that restarts decoding. A failure older than it was
    /// already answered by it.
    restart_pts: Option<u64>,
    /// How long a submission may stay inside the decoder before it is given up on.
    stuck_after: Duration,
    /// The submission seen inside the decoder at the last report, and how long the task has
    /// run on time since it went in.
    stuck: Option<(u64, Duration)>,
    /// A frame was dropped for a full decode queue: what is predicted from it is dropped too,
    /// until a restart reaches the decoder or one is asked for once it has room.
    backed_up: bool,
}

impl<D: VideoDecode> Lane<D> {
    /// Lane `index`, on `media`: its pictures go to `output`, a failure wakes the task through
    /// `failed`.
    fn new(
        media: StreamId,
        index: usize,
        codec: VideoCodec,
        output: &Arc<Output<D::Image>>,
        failed: &Arc<Notify>,
    ) -> Self {
        let outcomes = Outcomes {
            index,
            inflight: Arc::default(),
            news: Arc::default(),
            wake: Arc::clone(failed),
            output: Arc::clone(output),
        };
        Self {
            media,
            codec,
            // The loop ticks after every branch, so the longest it goes without one is the idle
            // sleep: the reassembler needs it to tell a link that held datagrams from a runtime
            // that did not run this task.
            reassembler: Reassembler::new(
                media,
                Config {
                    tick_period: IDLE_TICK,
                    ..Config::default()
                },
                instant(),
            ),
            decoder: DecodeThread::start::<D>(media, codec, &outcomes),
            outcomes,
            restart_pts: None,
            stuck_after: DECODE_STUCK,
            stuck: None,
            backed_up: false,
        }
    }

    /// Give the decoder up and start another, which waits for a keyframe.
    fn replace_decoder(&mut self, now: Instant) {
        self.stuck_after = if self.decoder.state.pictures.load(Ordering::Relaxed) > 0 {
            DECODE_STUCK
        } else {
            self.stuck_after.saturating_mul(2).min(DECODE_STUCK_MAX)
        };
        self.decoder = DecodeThread::start::<D>(self.media, self.codec, &self.outcomes);
        self.outcomes.inflight.lock().parked.clear();
        self.restart_pts = None;
        self.stuck = None;
        self.backed_up = false;
        self.reassembler.force_refresh(now, true);
    }

    /// Frames waiting behind a missing one, or a refresh asked for and not yet answered.
    fn busy(&self) -> bool {
        self.reassembler.queue_depth() > 0 || self.reassembler.awaiting_refresh()
    }
}

struct Worker<D: VideoDecode> {
    stream: StreamId,
    codec: VideoCodec,
    datagrams: mpsc::Receiver<Arrival>,
    /// The stream's datagrams the router dropped before this task read them.
    dropped: Arc<AtomicU64>,
    /// The whole picture, or the top stripe: the stream's own media stream, which also carries
    /// its cursor, heartbeats and clock echoes.
    top: Lane<D>,
    /// The lower stripe, from its first datagram until the stream is one picture again.
    lower: Option<Lane<D>>,
    /// The top lane's newest frame was of the whole picture.
    top_whole: bool,
    output: Arc<Output<D::Image>>,
    /// A decoder failed or refused a frame.
    failed: Arc<Notify>,
    feedback: Box<dyn Fn(Bytes) -> Delivery + Send>,
    path_rtt: Box<dyn Fn() -> Option<Duration> + Send>,
    /// The round trip as of the last report, which the reassemblers' timers go by.
    rtt: Duration,
    cursor: Arc<watch::Sender<CursorState>>,
    stats: watch::Sender<ScreenStats>,
    cursor_seq: Option<u32>,
    counters: ScreenStats,
    /// The lower stripe's reassembler counters as of its last report.
    lower_counters: ReassemblerStats,
    source_live: Arc<AtomicBool>,
    /// The last hint handed to the reassembler.
    source_hint: bool,
    /// Widens the wire's 32-bit capture timestamp.
    capture_clock: CaptureClock,
    /// Places desktopd's capture clock on this one from the probes' echoes.
    clock: ClockSync,
    /// Reports sent so far: the probes ride on their schedule.
    reports: u64,
    /// When the decoders were last looked at for a stuck submission.
    unstuck_at: Instant,
}

impl<D: VideoDecode> Worker<D> {
    async fn run(mut self) {
        let mut report = tokio::time::interval(REPORT_EVERY);
        report.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // One timer for the task's life, moved on each wake.
        let tick = tokio::time::sleep(IDLE_TICK);
        tokio::pin!(tick);
        loop {
            let busy = self.top.busy()
                || self.lower.as_ref().is_some_and(Lane::busy)
                || self.output.stitch.lock().waiting();
            let period = if busy { TICK } else { IDLE_TICK };
            let now = tokio::time::Instant::now();
            tick.as_mut().reset(now.checked_add(period).unwrap_or(now));
            tokio::select! {
                biased;
                () = &mut tick => {}
                () = self.failed.notified() => {}
                _instant = report.tick() => {
                    if !self.report() {
                        break;
                    }
                }
                arrival = self.datagrams.recv() => {
                    let Some((at, datagram)) = arrival else { break };
                    self.ingest(&datagram, at);
                    // What is already queued goes in before the timers look at frame ages.
                    while let Ok((at, datagram)) = self.datagrams.try_recv() {
                        self.ingest(&datagram, at);
                    }
                }
            }
            self.outcomes();
            if !self.actions() {
                break;
            }
            // A tick can release a frame queued behind a lost one; on a still screen the next
            // datagram is a heartbeat half a stall gap away.
            self.deliver(0);
            self.deliver(1);
            self.stitch();
        }
        tracing::debug!(stream = self.stream.0, "screen task finished");
    }

    /// Tell the stitch which stripes wait for a refresh, and put up a capture whose other
    /// stripe is late past [`STITCH_WAIT`].
    fn stitch(&self) {
        let lower = self
            .lower
            .as_ref()
            .is_some_and(|lane| lane.reassembler.awaiting_refresh());
        {
            let mut stitch = self.output.stitch.lock();
            stitch.set_stalled(0, self.top.reassembler.awaiting_refresh());
            stitch.set_stalled(1, lower);
        }
        self.output.expire(instant());
    }

    /// Feed one datagram that the link handed over at `now`.
    fn ingest(&mut self, datagram: &Bytes, now: Instant) {
        self.counters.datagrams = self.counters.datagrams.saturating_add(1);
        let len = u64::try_from(datagram.len()).unwrap_or(u64::MAX);
        self.counters.bytes = self.counters.bytes.saturating_add(len);
        self.counters.first_datagram_at.get_or_insert(now);
        let lag = instant().saturating_duration_since(now);
        if lag > self.counters.reader_lag_max {
            self.counters.reader_lag_max = lag;
        }
        if lag >= STALL_GAP {
            self.counters.reader_lag_over_gap = self.counters.reader_lag_over_gap.saturating_add(1);
        }
        let Some((header, _payload)) = MediaHeader::parse(datagram) else {
            return;
        };
        let media = StreamId(header.stream.get());
        let (_stream, index) = Stripe::stream_of(media);
        if index != 0 {
            // A lower stripe's datagram still on its way when the stream went back to one
            // picture opens no lane; a new striped session starts on its keyframe.
            if self.lower.is_none() && self.top_whole && header.flags & flags::KEYFRAME == 0 {
                return;
            }
            let (codec, output, failed) = (self.codec, &self.output, &self.failed);
            let lower = self.lower.get_or_insert_with(|| {
                tracing::debug!(stream = media.0, "the lower stripe's first datagram");
                Lane::new(media, 1, codec, output, failed)
            });
            if lower.reassembler.ingest(datagram, now) == Ingest::Video {
                self.deliver(1);
            }
            return;
        }
        self.clock_echo(datagram, now);
        match self.top.reassembler.ingest(datagram, now) {
            Ingest::Video => self.deliver(0),
            Ingest::Cursor { seq, update } => {
                let newer = self
                    .cursor_seq
                    .is_none_or(|last| seq.wrapping_sub(last) < u32::MAX / 2);
                if newer {
                    self.cursor_seq = Some(seq);
                    let (x, y, visible) = (update.x.get(), update.y.get(), update.visible != 0);
                    self.cursor.send_if_modified(|cursor| {
                        let moved = (cursor.x, cursor.y, cursor.visible) != (x, y, visible);
                        cursor.x = x;
                        cursor.y = y;
                        cursor.visible = visible;
                        moved
                    });
                }
            }
            // The Mac's sound comes on a media stream of its own (`crate::sound`).
            Ingest::Audio { .. } | Ingest::Heartbeat | Ingest::Ignored(_) => {}
        }
    }

    /// Take a clock probe's echo that arrived at `now`.
    fn clock_echo(&mut self, datagram: &Bytes, now: Instant) {
        let Some((header, payload)) = MediaHeader::parse(datagram) else {
            return;
        };
        if header.kind() != Some(Kind::Clock) {
            return;
        }
        let Some(echo) = ClockEcho::parse(payload) else {
            return;
        };
        self.clock
            .observe(echo.sent.get(), echo.received.get(), echo.echoed.get(), now);
        *self.output.estimate.lock() = self.clock.estimate();
    }

    /// Send a clock probe when one is due. `false` once the link is gone.
    fn probe(&mut self) -> bool {
        let due = self.reports < PROBES_AT_ONCE || self.reports.is_multiple_of(PROBE_EVERY);
        self.reports = self.reports.saturating_add(1);
        if !due {
            return true;
        }
        let sent_us = self.clock.stamp(instant());
        let probe = Feedback::Clock {
            stream: self.stream,
            sent_us,
        };
        (self.feedback)(probe.encode()) != Delivery::Gone
    }

    /// Push every frame of lane `index` that is now complete into its decoder. A frame of the
    /// whole picture on the top lane says the stream is not striped: the lower lane goes.
    fn deliver(&mut self, index: usize) {
        let Self {
            top,
            lower,
            counters,
            capture_clock,
            stream,
            top_whole,
            ..
        } = self;
        let Some(lane) = (if index == 0 {
            Some(top)
        } else {
            lower.as_mut()
        }) else {
            return;
        };
        let (mut whole, mut frame_seen) = (false, false);
        while let Some(frame) = lane.reassembler.next_frame() {
            frame_seen = true;
            if index == 0 {
                counters.frames = counters.frames.saturating_add(1);
            }
            counters.first_frame_at.get_or_insert_with(instant);
            if frame.hold > counters.hold_max {
                counters.hold_max = frame.hold;
            }
            whole = index == 0 && frame.info.stripes == 0;
            // Widened here, where the wire's 32-bit stamp becomes a u64: the decoder echoes it
            // back, so the parked arrivals and the pacer both survive the wrap.
            let pts = capture_clock.widen(frame.info.capture_ts_us);
            // Parked before submitting: the decoder may call back before `decode` returns.
            let restarts = frame.info.keyframe || frame.info.ltr_refresh;
            let discardable = frame.info.discardable;
            lane.outcomes.inflight.lock().park(Parked {
                pts_us: pts,
                arrived: frame.arrived,
                ltr_token: frame.info.ltr_token,
                restarts,
                discardable,
                coded: Coded {
                    stripes: frame.info.stripes,
                    build: frame.info.build,
                },
            });
            if restarts {
                lane.restart_pts = Some(pts);
            }
            if lane.backed_up && !restarts {
                lane.outcomes.inflight.lock().unpark(pts);
                counters.decode_errors = counters.decode_errors.saturating_add(1);
                continue;
            }
            let submit = Submit {
                data: frame.data,
                pts_us: pts,
                frame: frame.info.frame,
                discardable,
                restarts,
            };
            match lane.decoder.submit(submit) {
                Ok(()) => lane.backed_up = false,
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    if !lane.backed_up {
                        tracing::debug!(
                            stream = lane.media.0,
                            frame = frame.info.frame,
                            "the decoder is a queue behind: frames dropped"
                        );
                    }
                    lane.outcomes.inflight.lock().unpark(pts);
                    counters.decode_errors = counters.decode_errors.saturating_add(1);
                    lane.backed_up = true;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    tracing::warn!(
                        stream = lane.media.0,
                        "the decode thread is gone: a new decoder"
                    );
                    lane.replace_decoder(instant());
                    counters.decoders_replaced = counters.decoders_replaced.saturating_add(1);
                }
            }
        }
        if index == 0 && frame_seen {
            *top_whole = whole;
        }
        counters.striped = lower.is_some() && !*top_whole;
        if whole && lower.take().is_some() {
            tracing::debug!(
                stream = stream.0,
                "one picture again: the lower stripe's lane goes"
            );
        }
    }

    /// Take what the decoders' callbacks left: acknowledge the references they decoded, and ask
    /// for a refresh when one failed on a frame the last restart did not already replace.
    fn outcomes(&mut self) {
        for index in 0..Stripe::MAX {
            let Self {
                top,
                lower,
                counters,
                ..
            } = self;
            let Some(lane) = (if index == 0 {
                Some(top)
            } else {
                lower.as_mut()
            }) else {
                continue;
            };
            if !lane.outcomes.news.swap(false, Ordering::Acquire) {
                continue;
            }
            let (acks, failed, failed_discardable, status, refused, refusal_refresh) = {
                let mut inflight = lane.outcomes.inflight.lock();
                (
                    std::mem::take(&mut inflight.acks),
                    inflight.failed.take(),
                    std::mem::take(&mut inflight.failed_discardable),
                    inflight.status.take(),
                    std::mem::take(&mut inflight.refused),
                    inflight.refusal_refresh.take(),
                )
            };
            for token in acks {
                lane.reassembler.ack_ltr(token);
            }
            if let Some(Status(status)) = status {
                counters.decode_failure = status;
            }
            counters.decode_errors = counters
                .decode_errors
                .saturating_add(failed_discardable)
                .saturating_add(refused);
            if let Some(keyframe) = refusal_refresh {
                tracing::debug!(
                    stream = lane.media.0,
                    refused,
                    keyframe,
                    "the decoder refused frames"
                );
                lane.reassembler.force_refresh(instant(), keyframe);
            }
            let Some(failed) = failed else {
                continue;
            };
            counters.decode_errors = counters.decode_errors.saturating_add(failed.count);
            let Some(keyframe) = refresh_for(failed, lane.restart_pts, lane.decoder.ready()) else {
                continue;
            };
            tracing::debug!(
                stream = lane.media.0,
                ?failed,
                keyframe,
                "decoder returned no picture"
            );
            lane.reassembler.force_refresh(instant(), keyframe);
        }
    }

    /// Run the reassemblers' timers; `false` when the link is gone.
    fn actions(&mut self) -> bool {
        let hint = self.source_live.load(Ordering::Relaxed);
        let hinted = hint != self.source_hint;
        self.source_hint = hint;
        let rtt = self.rtt;
        let Self {
            top,
            lower,
            counters,
            feedback,
            ..
        } = self;
        for lane in std::iter::once(top).chain(lower.as_mut()) {
            if hinted {
                lane.reassembler.set_source_live(hint);
            }
            let media = lane.media;
            for action in lane.reassembler.tick(instant(), rtt) {
                let message = match action {
                    Action::Nack { frame, fragments } => {
                        counters.nacks = counters.nacks.saturating_add(1);
                        Feedback::Nack {
                            stream: media,
                            frame,
                            fragments,
                        }
                    }
                    Action::RequestRefresh {
                        last_good_frame,
                        keyframe,
                    } => Feedback::Refresh {
                        stream: media,
                        last_good_frame,
                        keyframe,
                    },
                };
                if (*feedback)(message.encode()) == Delivery::Gone {
                    return false;
                }
            }
        }
        true
    }

    /// Publish the counters and send desktopd a receiver report for each media stream. A report
    /// that does not go carries its counts and acknowledgements into the next. `false` once
    /// the link is gone.
    fn report(&mut self) -> bool {
        self.outcomes();
        self.rtt = (self.path_rtt)().unwrap_or(DEFAULT_RTT);
        let now = instant();
        self.unstick(now);
        let report = self.top.reassembler.take_report(now, 0);
        let stats = self.top.reassembler.stats();
        if let Some(lower) = &self.lower {
            self.lower_counters = lower.reassembler.stats();
        }
        self.count(&stats, &report, now);
        self.stats.send_replace(self.counters);
        let stream = self.stream;
        match self.send_report(stream, report) {
            Delivery::Sent => {}
            Delivery::Dropped => self.top.reassembler.take_back(&report),
            Delivery::Gone => return false,
        }
        if let Some(lower) = &mut self.lower {
            let report = lower.reassembler.take_report(now, 0);
            let message = Feedback::Report {
                stream: lower.media,
                report,
            };
            match (self.feedback)(message.encode()) {
                Delivery::Sent => {}
                Delivery::Dropped => lower.reassembler.take_back(&report),
                Delivery::Gone => return false,
            }
        }
        self.probe()
    }

    /// The counters, from the top lane's `stats` and `report` and the lower lane's last.
    fn count(&mut self, stats: &ReassemblerStats, report: &ReceiverReport, now: Instant) {
        let lower = &self.lower_counters;
        let c = &mut self.counters;
        c.frames_fec = stats.frames_fec.saturating_add(lower.frames_fec);
        c.frames_retransmit = stats
            .frames_retransmit
            .saturating_add(lower.frames_retransmit);
        c.frames_lost = stats.frames_lost.saturating_add(lower.frames_lost);
        c.refreshes = stats.refreshes.saturating_add(lower.refreshes);
        c.refresh_repeats = stats.refresh_repeats.saturating_add(lower.refresh_repeats);
        c.frames_skipped = stats.frames_skipped.saturating_add(lower.frames_skipped);
        c.datagrams_lost = stats.datagrams_lost.saturating_add(lower.datagrams_lost);
        c.parity_permille = observed_parity(stats);
        c.data_shards = stats.data_shards.saturating_add(lower.data_shards);
        c.parity_shards = stats.parity_shards.saturating_add(lower.parity_shards);
        c.stalls = stats.stalls;
        c.stalled_ms = stats.stalled_ms;
        c.silences = stats.silences;
        c.stalled = self.top.reassembler.stalled(now);
        c.hold_p50 = report.hold_p50;
        c.hold_p95 = report.hold_p95;
        c.jitter = report.owd_jitter;
        c.queue_depth = report.queue_depth;
        c.decoding = std::iter::once(&self.top)
            .chain(self.lower.as_ref())
            .map(|lane| {
                u64::try_from(lane.outcomes.inflight.lock().parked.len()).unwrap_or(u64::MAX)
            })
            .sum();
        c.datagrams_dropped = self.dropped.load(Ordering::Relaxed);
        c.reported_at = Some(now);
        c.first_decoded_at = *self.output.first_decoded.lock();
        c.seam_tears = self.output.stitch.lock().tears;
        c.clock = self.clock.estimate();
    }

    /// Give up on a decoder whose submission has stayed inside the platform past its lane's
    /// patience, and start another for a keyframe. A decoder that dropped frames for a full
    /// queue and has room again is asked a refresh for. The patience is spent only while this
    /// task runs on time ([`STUCK_CREDIT`]).
    fn unstick(&mut self, now: Instant) {
        let ran = now
            .saturating_duration_since(self.unstuck_at)
            .min(STUCK_CREDIT);
        self.unstuck_at = now;
        let Self {
            top,
            lower,
            counters,
            ..
        } = self;
        for lane in std::iter::once(top).chain(lower.as_mut()) {
            let inside = lane.decoder.state.submitting.load(Ordering::Acquire);
            lane.stuck = (inside != 0).then(|| match lane.stuck {
                Some((seen, stuck)) if seen == inside => (inside, stuck.saturating_add(ran)),
                _ => (inside, Duration::ZERO),
            });
            let stuck = lane.stuck.map(|(_, stuck)| stuck);
            let Some(stuck) = stuck.filter(|stuck| *stuck >= lane.stuck_after) else {
                if lane.backed_up && lane.decoder.has_room() {
                    tracing::debug!(stream = lane.media.0, "the decoder caught up: a refresh");
                    lane.backed_up = false;
                    lane.reassembler.force_refresh(now, !lane.decoder.ready());
                }
                continue;
            };
            tracing::warn!(
                stream = lane.media.0,
                ?stuck,
                "a decode submission has not returned: a new decoder"
            );
            lane.replace_decoder(now);
            counters.decoders_replaced = counters.decoders_replaced.saturating_add(1);
        }
    }

    fn send_report(&self, media: StreamId, report: ReceiverReport) -> Delivery {
        let delivery = (self.feedback)(
            Feedback::Report {
                stream: media,
                report,
            }
            .encode(),
        );
        if delivery == Delivery::Dropped {
            tracing::debug!(stream = media.0, "report not sent; carried to the next");
        }
        delivery
    }
}

/// What the decoder refusing a frame as it was submitted asks for: nothing for a frame nothing
/// refers to while the session holds; otherwise a refresh, a keyframe when no session is left
/// or the refused frame was itself a restart.
const fn refresh_after_refusal(discardable: bool, ready: bool, restarts: bool) -> Option<bool> {
    if discardable && ready {
        None
    } else {
        Some(!ready || restarts)
    }
}

/// The refresh a decode failure asks for, `Some(keyframe)`; `None` when the last restart, sent
/// after the failed frame, already replaces it.
fn refresh_for(failed: Failed, restart_pts: Option<u64>, ready: bool) -> Option<bool> {
    if restart_pts.is_some_and(|restart| failed.pts_us < restart) {
        return None;
    }
    Some(failed.session_lost || failed.restart || !ready)
}

/// The parity ratio desktopd is sending, in thousandths of the data fragments, from the frame
/// layouts seen so far. Zero until a frame arrives.
fn observed_parity(stats: &ReassemblerStats) -> u16 {
    let permille = stats
        .parity_shards
        .saturating_mul(1000)
        .checked_div(stats.data_shards)
        .unwrap_or(0);
    u16::try_from(permille).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests;
