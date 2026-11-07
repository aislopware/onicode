//! Platforms that capture nothing: the tests' and `onicode-desktopd --synthetic`'s.
//!
//! - [`Drawn`], everywhere: two drawn displays whose pictures are numbers, a fake encoder that
//!   codes each deterministically (a keyframe when asked or first, a long-term reference every
//!   [`LTR_EVERY`] frames, the bytes a function of the picture, [`Coded`]), a tone for sound, and
//!   an injector that records what it is handed ([`Recording`]). Its displays draw and its tone
//!   plays on tokio's timers and every call into it is made on the runtime
//!   ([`Platform::ON_RUNTIME`]), so under a paused clock a whole stream runs in virtual time. The
//!   pipeline, session and glass tests run on it over the simulated tailnet.
//! - `Canvas`, on a Mac: the capture crate's drawn canvas, coded by VideoToolbox, so a real client
//!   shows a real moving picture with no Screen Recording grant; input is recorded and counted on
//!   the canvas's strip.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use onicode_desktop_capture::{
    AudioSink, CaptureConfig, CaptureError, CaptureSource, CapturedAudio, CapturedFrame, Heard,
    Rect,
};
use onicode_desktop_codec::{
    AudioEncoder, CodecError, EncodedPacket, EncoderConfig, FrameOptions, VideoEncoder,
};
use onicode_desktop_input::{InputError, InputSink, PointerWatch};
use onicode_desktop_proto::DisplayId;
use onicode_desktop_proto::screen::{CaptureTarget, CursorShape, DisplayInfo, ScreenInput};
use parking_lot::Mutex;

use crate::platform::Platform;

/// A long-term reference is offered every this many frames.
pub const LTR_EVERY: u64 = 10;
/// A fake keyframe's bytes.
pub const KEYFRAME_BYTES: usize = 6_000;
/// A fake delta frame's bytes.
pub const DELTA_BYTES: usize = 700;

/// The drawn platform.
#[derive(Clone, Copy, Debug)]
pub enum Drawn {}

impl Platform for Drawn {
    type Audio = Tally;
    type Capture = Pictures;
    type Drapes = crate::curtain::Unseen;
    type Input = Recording;
    type Video = Fake;

    const ON_RUNTIME: bool = true;

    fn displays() -> Vec<DisplayInfo> {
        DISPLAYS.to_vec()
    }
}

/// The drawn displays: one Retina panel and one small one beside it.
pub const DISPLAYS: [DisplayInfo; 2] = [
    DisplayInfo {
        id: DisplayId(1),
        w: 640.0,
        h: 400.0,
        scale: 2.0,
        hz: 60.0,
    },
    DisplayInfo {
        id: DisplayId(2),
        w: 320.0,
        h: 200.0,
        scale: 1.0,
        hz: 30.0,
    },
];

fn display(id: DisplayId) -> Option<(usize, &'static DisplayInfo)> {
    DISPLAYS.iter().enumerate().find(|(_i, d)| d.id == id)
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the drawn displays' small sizes"
)]
const fn pixels(d: &DisplayInfo) -> (u32, u32) {
    ((d.w * d.scale) as u32, (d.h * d.scale) as u32)
}

/// A drawn picture: its capture, its number, when it was drawn and its size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Picture {
    /// The capture that drew it: each started on a thread is numbered from 1 (`Desk`).
    pub capture: u32,
    /// Its number in its capture, from 1.
    pub seq: u64,
    /// When it was drawn ([`now_us`]).
    pub captured_us: u64,
    /// Its width.
    pub width: u32,
    /// Its height.
    pub height: u32,
}

/// What the fake encoder coded, as its bytes say it and its log keeps it ([`coded`]).
///
/// A coded frame's bytes start with [`Self::HEADER`] bytes: `K` or `D` (a keyframe or not),
/// then the capture, the picture's number, when it was drawn, its width and its height, little
/// endian; the rest is the number's low byte. A test's decoder reads them back ([`Self::read`]),
/// so what reaches the glass names the capture it shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Coded {
    /// The picture.
    pub picture: Picture,
    /// Coded to stand alone.
    pub keyframe: bool,
    /// The presentation timestamp it was coded with: the capture's for a fresh picture, later
    /// for one coded again (a repair, a refresh).
    pub pts_us: u64,
}

impl Coded {
    /// The header's length.
    pub const HEADER: usize = 29;

    /// The frame's bytes, `len` of them.
    fn bytes(&self, len: usize) -> Vec<u8> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a byte of the picture's number"
        )]
        let fill = self.picture.seq as u8;
        let mut data = Vec::with_capacity(len.max(Self::HEADER));
        data.push(if self.keyframe { b'K' } else { b'D' });
        data.extend_from_slice(&self.picture.capture.to_le_bytes());
        data.extend_from_slice(&self.picture.seq.to_le_bytes());
        data.extend_from_slice(&self.picture.captured_us.to_le_bytes());
        data.extend_from_slice(&self.picture.width.to_le_bytes());
        data.extend_from_slice(&self.picture.height.to_le_bytes());
        data.resize(len.max(Self::HEADER), fill);
        data
    }

    /// What a coded frame's bytes say, coded at `pts_us`; `None` for bytes the fake did not
    /// make.
    #[must_use]
    pub fn read(data: &[u8], pts_us: u64) -> Option<Self> {
        let keyframe = match data.first()? {
            b'K' => true,
            b'D' => false,
            _other => return None,
        };
        let u32_at = |at: usize| -> Option<u32> {
            Some(u32::from_le_bytes(
                data.get(at..at.checked_add(4)?)?.try_into().ok()?,
            ))
        };
        let u64_at = |at: usize| -> Option<u64> {
            Some(u64::from_le_bytes(
                data.get(at..at.checked_add(8)?)?.try_into().ok()?,
            ))
        };
        Some(Self {
            picture: Picture {
                capture: u32_at(1)?,
                seq: u64_at(5)?,
                captured_us: u64_at(13)?,
                width: u32_at(21)?,
                height: u32_at(25)?,
            },
            keyframe,
            pts_us,
        })
    }
}

/// The drawn Mac a thread's runtime drives: the captures it started, what its encoders coded
/// and what its injectors were handed.
///
/// One a thread: a test's current-thread runtime makes every drawn call on the test's own
/// thread ([`Platform::ON_RUNTIME`]), so each test has a Mac of its own whatever else runs in
/// the process, and reads it from its own body ([`coded`], [`recorded`]).
#[derive(Debug, Default)]
struct Desk {
    captures: u32,
    coded: Vec<Coded>,
    recorded: Vec<(CaptureTarget, ScreenInput)>,
}

thread_local! {
    static DESK: RefCell<Desk> = RefCell::new(Desk::default());
}

/// What the fake encoders on this thread coded so far, in order.
#[must_use]
pub fn coded() -> Vec<Coded> {
    DESK.with_borrow(|desk| desk.coded.clone())
}

/// The drawn clock: microseconds of tokio's clock since this thread first read it, so under a
/// paused clock a capture's stamp, the encoder's and the glass's are all virtual time.
#[must_use]
pub fn now_us() -> u64 {
    thread_local! {
        static EPOCH: tokio::time::Instant = tokio::time::Instant::now();
    }
    let elapsed = tokio::time::Instant::now().saturating_duration_since(EPOCH.with(|e| *e));
    u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX)
}

/// The runtime the drawn displays and tone run on: the caller's.
fn runtime() -> Result<tokio::runtime::Handle, CaptureError> {
    tokio::runtime::Handle::try_current()
        .map_err(|e| CaptureError::Stopped(format!("the drawn platform runs on tokio: {e}")))
}

/// One frame period at `fps`.
fn period(fps: u16) -> Duration {
    Duration::from_micros(1_000_000 / u64::from(fps.max(1)))
}

/// A drawn display, resolved.
#[derive(Clone, Copy, Debug)]
pub struct DrawnTarget {
    info: DisplayInfo,
}

/// A running drawn capture: a task drawing at the display's rate.
#[derive(Debug)]
pub struct DrawnStream {
    stop: Arc<AtomicBool>,
    size: Arc<Mutex<(u32, u32, u16)>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for DrawnStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// A running tone: a task playing 10 ms at a time.
#[derive(Debug)]
pub struct DrawnSound {
    stop: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for DrawnSound {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The Mac's one pointer, where the last move put it.
static POINTER: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));
static POINTER_MOVES: AtomicU32 = AtomicU32::new(0);

/// The drawn capture.
#[derive(Clone, Copy, Debug)]
pub enum Pictures {}

impl CaptureSource for Pictures {
    type Content = Vec<DisplayInfo>;
    type Image = Picture;
    type Sound = DrawnSound;
    type Stream = DrawnStream;
    type Target = DrawnTarget;

    fn can_capture() -> bool {
        true
    }

    fn enumerate(done: impl FnOnce(Result<Vec<DisplayInfo>, CaptureError>) + Send + 'static) {
        done(Ok(DISPLAYS.to_vec()));
    }

    fn displays(content: &Vec<DisplayInfo>) -> Vec<DisplayInfo> {
        content.clone()
    }

    fn resolve(
        content: &Vec<DisplayInfo>,
        kind: CaptureTarget,
    ) -> Result<DrawnTarget, CaptureError> {
        let CaptureTarget::Display(id) = kind;
        content
            .iter()
            .find(|d| d.id == id)
            .map(|info| DrawnTarget { info: *info })
            .ok_or(CaptureError::NotFound(kind))
    }

    fn pixel_size(target: &DrawnTarget) -> (u32, u32) {
        pixels(&target.info)
    }

    fn point_scale(target: &DrawnTarget) -> f32 {
        target.info.scale
    }

    fn start(
        target: &DrawnTarget,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame<Picture>) + Send + Sync + 'static,
        _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<DrawnStream, CaptureError> {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a display's rate"
        )]
        let hz = target.info.hz as u16;
        let fps = if config.fps == 0 { hz } else { config.fps };
        let runtime = runtime()?;
        let capture = DESK.with_borrow_mut(|desk| {
            desk.captures = desk.captures.wrapping_add(1);
            desk.captures
        });
        let stop = Arc::new(AtomicBool::new(false));
        let size = Arc::new(Mutex::new((config.width, config.height, fps.max(1))));
        let (stopped, drawn) = (Arc::clone(&stop), Arc::clone(&size));
        let task = runtime.spawn(async move {
            let mut seq = 0_u64;
            let mut drawing_at = fps.max(1);
            let mut ticks = tokio::time::interval(period(drawing_at));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let (width, height, fps) = *drawn.lock();
                if fps != drawing_at {
                    drawing_at = fps;
                    let every = period(fps);
                    ticks = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
                    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                }
                seq = seq.wrapping_add(1);
                let captured_us = now_us();
                sink(CapturedFrame {
                    image: Picture {
                        capture,
                        seq,
                        captured_us,
                        width,
                        height,
                    },
                    capture_ts_us: captured_us,
                    display_ts_us: None,
                    age_us: 0,
                    latency_us: 0,
                });
            }
        });
        done(Ok(()));
        Ok(DrawnStream { stop, size, task })
    }

    fn start_sound(
        _heard: &Heard,
        sink: AudioSink,
        _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<DrawnSound, CaptureError>) + Send + 'static,
    ) {
        let runtime = match runtime() {
            Ok(runtime) => runtime,
            Err(e) => return done(Err(e)),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let task = runtime.spawn(async move {
            let mut phase = 0.0_f32;
            let mut ticks = tokio::time::interval(Duration::from_millis(10));
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticks.tick().await;
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let samples = (0..960)
                    .map(|i| {
                        if i % 2 == 0 {
                            phase += 0.057;
                        }
                        phase.sin() * 0.25
                    })
                    .collect();
                sink(CapturedAudio {
                    pts_us: now_us(),
                    samples,
                });
            }
        });
        done(Ok(DrawnSound { stop, task }));
    }

    fn hear(
        _sound: &DrawnSound,
        _heard: &Heard,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        done(Ok(()));
    }

    fn stop_sound(
        sound: &DrawnSound,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        sound.stop.store(true, Ordering::Release);
        done(Ok(()));
    }

    fn update(
        stream: &DrawnStream,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        let mut size = stream.size.lock();
        size.0 = config.width;
        size.1 = config.height;
        if config.fps != 0 {
            size.2 = config.fps;
        }
        drop(size);
        done(Ok(()));
    }

    fn retarget(
        stream: &DrawnStream,
        target: &DrawnTarget,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        let (width, height) = pixels(&target.info);
        let mut size = stream.size.lock();
        size.0 = width;
        size.1 = height;
        drop(size);
        done(Ok(()));
    }

    fn stop(stream: &DrawnStream, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        stream.stop.store(true, Ordering::Release);
        done(Ok(()));
    }

    fn now_us() -> u64 {
        now_us()
    }

    fn target_bounds(target: CaptureTarget) -> Option<Rect> {
        let CaptureTarget::Display(id) = target;
        display(id).map(|_found| Self::display_bounds(id.0))
    }

    fn refresh_hz(target: CaptureTarget) -> Option<f64> {
        let CaptureTarget::Display(id) = target;
        display(id).map(|(_i, d)| f64::from(d.hz))
    }

    fn display_bounds(id: u32) -> Rect {
        display(DisplayId(id)).map_or_else(Rect::default, |(i, d)| {
            let x: f64 = DISPLAYS
                .iter()
                .take(i)
                .map(|before| f64::from(before.w))
                .sum();
            Rect {
                x,
                y: 0.0,
                w: f64::from(d.w),
                h: f64::from(d.h),
            }
        })
    }

    fn pointer_moves() -> u32 {
        POINTER_MOVES.load(Ordering::Relaxed)
    }

    fn pointer_location() -> (f64, f64) {
        *POINTER.lock()
    }

    fn cursor_seed() -> Option<i32> {
        Some(1)
    }

    fn cursor_shape() -> Option<CursorShape> {
        Some(CursorShape {
            w: 2,
            h: 2,
            hot_x: 0,
            hot_y: 0,
            bgra: vec![0xff; 16],
            scale: 1,
        })
    }
}

/// The fake encoder's sink.
type Sink = Box<dyn Fn(EncodedPacket) + Send + Sync>;

/// A deterministic encoder of drawn pictures.
pub struct Fake {
    config: Mutex<EncoderConfig>,
    sink: Sink,
    coded: AtomicU64,
}

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fake")
            .field("config", &*self.config.lock())
            .finish_non_exhaustive()
    }
}

impl VideoEncoder for Fake {
    type Image = Picture;

    fn new(
        config: EncoderConfig,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Ok(Self {
            config: Mutex::new(config),
            sink: Box::new(sink),
            coded: AtomicU64::new(0),
        })
    }

    fn stripe(
        config: EncoderConfig,
        _stripe: onicode_desktop_codec::stripes::Stripe,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Self::new(config, sink)
    }

    fn encode(
        &self,
        image: &Picture,
        pts_us: u64,
        options: &FrameOptions,
    ) -> Result<(), CodecError> {
        let config = *self.config.lock();
        let surface = (config.width, config.height);
        if (
            image.width.next_multiple_of(16),
            image.height.next_multiple_of(16),
        ) != (
            surface.0.next_multiple_of(16),
            surface.1.next_multiple_of(16),
        ) {
            let side = |v: u32| usize::try_from(v).unwrap_or(usize::MAX);
            return Err(CodecError::WrongSize {
                image: (side(image.width), side(image.height)),
                session: (side(surface.0), side(surface.1)),
            });
        }
        let coded = self.coded.fetch_add(1, Ordering::Relaxed);
        let keyframe = coded == 0 || options.force_keyframe;
        let ltr_token = coded
            .is_multiple_of(LTR_EVERY)
            .then_some(coded.wrapping_add(1));
        let len = if keyframe {
            KEYFRAME_BYTES
        } else {
            DELTA_BYTES
        };
        let made = Coded {
            picture: *image,
            keyframe,
            pts_us,
        };
        DESK.with_borrow_mut(|desk| desk.coded.push(made));
        (self.sink)(EncodedPacket {
            data: made.bytes(len),
            keyframe,
            ltr_token,
            ltr_refresh: options.force_ltr_refresh && !keyframe,
            mse: None,
            pts_us,
        });
        Ok(())
    }

    fn set_bitrate(&self, bps: u32) -> Result<(), CodecError> {
        self.config.lock().bitrate_bps = bps;
        Ok(())
    }

    fn set_frame_rate(&self, fps: u16) -> Result<(), CodecError> {
        self.config.lock().fps = fps;
        Ok(())
    }
}

/// An audio encoder that packs each 10 ms of sound into a small packet of its level.
#[derive(Debug, Default)]
pub struct Tally {
    pending: Vec<f32>,
}

impl AudioEncoder for Tally {
    fn new() -> Result<Self, CodecError> {
        Ok(Self::default())
    }

    fn push(&mut self, samples: &[f32], mut out: impl FnMut(&[u8])) -> Result<(), CodecError> {
        const TEN_MS: usize = 960;
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= TEN_MS {
            let chunk: Vec<f32> = self.pending.drain(..TEN_MS).collect();
            let peak = chunk.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a level in 0..=255"
            )]
            let level = (peak.clamp(0.0, 1.0) * 255.0) as u8;
            out(&[level; 20]);
        }
        Ok(())
    }
}

/// What the recording injectors on this thread were handed so far, with their displays, in
/// order (`Desk`).
#[must_use]
pub fn recorded() -> Vec<(CaptureTarget, ScreenInput)> {
    DESK.with_borrow(|desk| desk.recorded.clone())
}

/// An injector that records what it is handed and puts the pointer where a move says.
#[derive(Debug)]
pub struct Recording {
    target: CaptureTarget,
    scale: f64,
    bounds: Option<Rect>,
    pointer: PointerWatch,
}

impl InputSink for Recording {
    fn new(target: CaptureTarget, scale: f64) -> Self {
        Self {
            target,
            scale,
            bounds: None,
            pointer: PointerWatch::default(),
        }
    }

    fn set_scale(&mut self, scale: f64) {
        self.scale = scale;
    }

    fn set_bounds(&mut self, bounds: Option<Rect>, _at: Instant) {
        self.bounds = bounds;
    }

    fn inject(&mut self, input: &ScreenInput) -> Result<(), InputError> {
        DESK.with_borrow_mut(|desk| desk.recorded.push((self.target, input.clone())));
        let _count = onicode_desktop_capture::synthetic::take_input();
        if let ScreenInput::Move { x, y } = input {
            let bounds = self.bounds.ok_or(InputError::NoBounds)?;
            let scale = if self.scale > 0.0 { self.scale } else { 1.0 };
            let at = (
                bounds.x + f64::from(*x) / scale,
                bounds.y + f64::from(*y) / scale,
            );
            self.pointer.place(at.0, at.1);
            *POINTER.lock() = at;
            POINTER_MOVES.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn focus(&mut self) -> Result<(), InputError> {
        Ok(())
    }

    fn release_all(&mut self) {}

    fn pointer(&self) -> PointerWatch {
        self.pointer.clone()
    }
}

/// The capture crate's drawn canvas, coded by VideoToolbox: `--synthetic` on a Mac.
#[derive(Clone, Copy, Debug)]
pub enum Canvas {}

impl Platform for Canvas {
    type Audio = onicode_desktop_codec::Opus;
    type Capture = onicode_desktop_capture::synthetic::Canvas;
    type Drapes = crate::curtain::Unseen;
    type Input = Recording;
    type Video = onicode_desktop_codec::VideoToolbox;

    fn displays() -> Vec<DisplayInfo> {
        onicode_desktop_capture::active_displays()
    }
}
