//! Platforms that capture nothing: the tests' and `onicode-desktopd --synthetic`'s.
//!
//! - [`Drawn`], everywhere: two drawn displays whose pictures are numbers, a fake encoder that
//!   codes each deterministically (a keyframe when asked or first, a long-term reference every
//!   [`LTR_EVERY`] frames, the bytes a function of the picture), a tone for sound, and an injector
//!   that records what it is handed ([`Recording`]). The pipeline and session tests run on it over
//!   the simulated tailnet.
//! - `Canvas`, on a Mac: the capture crate's drawn canvas, coded by VideoToolbox, so a real client
//!   shows a real moving picture with no Screen Recording grant; input is recorded and counted on
//!   the canvas's strip.

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
    type Input = Recording;
    type Video = Fake;

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

/// A drawn picture: its number and size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Picture {
    /// Pictures drawn before it by its capture.
    pub seq: u64,
    /// Its width.
    pub width: u32,
    /// Its height.
    pub height: u32,
}

/// A drawn display, resolved.
#[derive(Clone, Copy, Debug)]
pub struct DrawnTarget {
    info: DisplayInfo,
}

/// A running drawn capture.
#[derive(Debug)]
pub struct DrawnStream {
    stop: Arc<AtomicBool>,
    size: Arc<Mutex<(u32, u32, u16)>>,
}

/// A running tone.
#[derive(Debug)]
pub struct DrawnSound {
    stop: Arc<AtomicBool>,
}

static START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
static POINTER: Mutex<(f64, f64)> = Mutex::new((0.0, 0.0));
static POINTER_MOVES: AtomicU32 = AtomicU32::new(0);
/// Whether the drawn displays draw: off, every picture is the last one again (a still screen).
static DRAWING: AtomicBool = AtomicBool::new(true);

/// Make the drawn displays still (`false`) or moving.
pub fn set_drawing(drawing: bool) {
    DRAWING.store(drawing, Ordering::Relaxed);
}

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
        let stop = Arc::new(AtomicBool::new(false));
        let size = Arc::new(Mutex::new((config.width, config.height, fps.max(1))));
        let (stopped, drawn) = (Arc::clone(&stop), Arc::clone(&size));
        std::thread::Builder::new()
            .name("onicode-drawn-display".to_owned())
            .spawn(move || {
                let mut seq = 0_u64;
                while !stopped.load(Ordering::Acquire) {
                    let (width, height, fps) = *drawn.lock();
                    if DRAWING.load(Ordering::Relaxed) {
                        seq = seq.wrapping_add(1);
                    }
                    sink(CapturedFrame {
                        image: Picture { seq, width, height },
                        capture_ts_us: Self::now_us(),
                        display_ts_us: None,
                        age_us: 0,
                        latency_us: 0,
                    });
                    std::thread::sleep(Duration::from_micros(1_000_000 / u64::from(fps.max(1))));
                }
            })
            .map_err(|e| CaptureError::Stopped(e.to_string()))?;
        done(Ok(()));
        Ok(DrawnStream { stop, size })
    }

    fn start_sound(
        _heard: &Heard,
        sink: AudioSink,
        _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<DrawnSound, CaptureError>) + Send + 'static,
    ) {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let spawned = std::thread::Builder::new()
            .name("onicode-drawn-tone".to_owned())
            .spawn(move || {
                let mut phase = 0.0_f32;
                while !stopped.load(Ordering::Acquire) {
                    let samples = (0..960)
                        .map(|i| {
                            if i % 2 == 0 {
                                phase += 0.057;
                            }
                            phase.sin() * 0.25
                        })
                        .collect();
                    sink(CapturedAudio {
                        pts_us: Self::now_us(),
                        samples,
                    });
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        done(
            spawned
                .map(|_thread| DrawnSound { stop })
                .map_err(|e| CaptureError::Stopped(e.to_string())),
        );
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
        u64::try_from(START.elapsed().as_micros()).unwrap_or(u64::MAX)
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
    layers: AtomicBool,
}

impl std::fmt::Debug for Fake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fake")
            .field("config", &*self.config.lock())
            .finish_non_exhaustive()
    }
}

/// Encoders made, process-wide: the tests read sessions being rebuilt.
pub static SESSIONS: AtomicU64 = AtomicU64::new(0);

impl VideoEncoder for Fake {
    type Image = Picture;

    fn new(
        config: EncoderConfig,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        SESSIONS.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            config: Mutex::new(config),
            sink: Box::new(sink),
            coded: AtomicU64::new(0),
            layers: AtomicBool::new(false),
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
        let discardable = !keyframe
            && ltr_token.is_none()
            && self.layers.load(Ordering::Relaxed)
            && coded % 2 == 1;
        let len = if keyframe {
            KEYFRAME_BYTES
        } else {
            DELTA_BYTES
        };
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a byte of the picture's number"
        )]
        let fill = image.seq as u8;
        (self.sink)(EncodedPacket {
            data: vec![fill; len],
            keyframe,
            ltr_token,
            ltr_refresh: options.force_ltr_refresh && !keyframe,
            discardable,
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

    fn set_temporal_layers(&self, on: bool) -> Result<bool, CodecError> {
        self.layers.store(on, Ordering::Relaxed);
        Ok(true)
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

/// Every input any [`Recording`] was handed, with its display, in order.
static RECORDED: Mutex<Vec<(CaptureTarget, ScreenInput)>> = Mutex::new(Vec::new());

/// What the recording injectors were handed so far.
#[must_use]
pub fn recorded() -> Vec<(CaptureTarget, ScreenInput)> {
    RECORDED.lock().clone()
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
        RECORDED.lock().push((self.target, input.clone()));
        #[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug)]
pub enum Canvas {}

#[cfg(target_os = "macos")]
impl Platform for Canvas {
    type Audio = onicode_desktop_codec::Opus;
    type Capture = onicode_desktop_capture::synthetic::Canvas;
    type Input = Recording;
    type Video = onicode_desktop_codec::VideoToolbox;

    fn displays() -> Vec<DisplayInfo> {
        onicode_desktop_capture::active_displays()
    }
}
