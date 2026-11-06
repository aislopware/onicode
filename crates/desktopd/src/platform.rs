//! The platform desktopd runs on, as the seams its streams use.
//!
//! The seams are capture (which also reads the cursor and the pointer), the video and audio
//! encoders, input, and the display list. Each but the last is a trait of its desktop crate, and
//! a [`Platform`] names one implementation of each, so every call on the frame path is a direct
//! one. [`Native`] is macOS's ([`mac`]) on a Mac and [`headless::Headless`] elsewhere, which
//! streams nothing; the tests' is [`crate::synthetic::Drawn`].

use onicode_desktop_capture::CaptureSource;
use onicode_desktop_codec::{AudioEncoder, VideoEncoder};
use onicode_desktop_input::InputSink;
use onicode_desktop_proto::screen::DisplayInfo;

#[cfg(target_os = "macos")]
pub mod mac;

/// One implementation of each seam.
pub trait Platform: Send + Sync + 'static {
    /// Pictures of a display, the cursor and the pointer.
    type Capture: CaptureSource<Image: Clone + Send + Sync>;
    /// The video encoder, coding what the capture delivers.
    type Video: VideoEncoder<Image = <Self::Capture as CaptureSource>::Image>;
    /// The sound's encoder.
    type Audio: AudioEncoder;
    /// Input into the Mac.
    type Input: InputSink;

    /// The displays, as the window server lists them: never through ScreenCaptureKit, whose
    /// listing prompts for Screen Recording.
    fn displays() -> Vec<DisplayInfo>;

    /// Whether display `id` is one made for a client ([`crate::sized`]): never listed as a
    /// physical display.
    fn made_for_a_client(_id: u32) -> bool {
        false
    }

    /// Keep the displays awake while a stream runs; dropping the answer lets go. `None` where
    /// the platform holds nothing.
    fn keep_awake() -> Option<Box<dyn Send + Sync>> {
        None
    }
}

/// The platform this build streams from.
#[cfg(target_os = "macos")]
pub type Native = mac::MacOs;

/// The platform this build streams from: none, off a Mac.
#[cfg(not(target_os = "macos"))]
pub type Native = headless::Headless;

/// A platform with no screen: every capture and encode is refused, so a stream never opens.
pub mod headless {
    use std::time::Instant;

    use onicode_desktop_capture::{
        AudioSink, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Heard, Rect,
    };
    use onicode_desktop_codec::{
        AudioEncoder, CodecError, EncodedPacket, EncoderConfig, FrameOptions, VideoEncoder,
    };
    use onicode_desktop_input::{InputError, InputSink, PointerWatch};
    use onicode_desktop_proto::screen::{CaptureTarget, CursorShape, DisplayInfo, ScreenInput};

    use super::Platform;

    /// No screen, no encoder, no input.
    #[derive(Clone, Copy, Debug)]
    pub enum Headless {}

    impl Platform for Headless {
        type Audio = NoAudio;
        type Capture = NoCapture;
        type Input = NoInput;
        type Video = NoVideo;

        fn displays() -> Vec<DisplayInfo> {
            Vec::new()
        }
    }

    /// A value that cannot be: the targets, streams and sounds of a capture that has none.
    #[derive(Clone, Copy, Debug)]
    pub enum Never {}

    impl Never {
        const fn absurd(self) -> ! {
            match self {}
        }
    }

    /// No capture.
    #[derive(Clone, Copy, Debug)]
    pub enum NoCapture {}

    impl CaptureSource for NoCapture {
        type Content = ();
        type Image = ();
        type Sound = Never;
        type Stream = Never;
        type Target = Never;

        fn can_capture() -> bool {
            false
        }

        fn enumerate(done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
            done(Err(CaptureError::Unsupported));
        }

        fn displays((): &()) -> Vec<DisplayInfo> {
            Vec::new()
        }

        fn resolve((): &(), _kind: CaptureTarget) -> Result<Never, CaptureError> {
            Err(CaptureError::Unsupported)
        }

        fn pixel_size(target: &Never) -> (u32, u32) {
            target.absurd()
        }

        fn point_scale(target: &Never) -> f32 {
            target.absurd()
        }

        fn start(
            target: &Never,
            _config: &CaptureConfig,
            _sink: impl Fn(CapturedFrame<()>) + Send + Sync + 'static,
            _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
            _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) -> Result<Never, CaptureError> {
            target.absurd()
        }

        fn start_sound(
            _heard: &Heard,
            _sink: AudioSink,
            _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
            done: impl FnOnce(Result<Never, CaptureError>) + Send + 'static,
        ) {
            done(Err(CaptureError::Unsupported));
        }

        fn hear(
            sound: &Never,
            _heard: &Heard,
            _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            sound.absurd()
        }

        fn stop_sound(
            sound: &Never,
            _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            sound.absurd()
        }

        fn update(
            stream: &Never,
            _config: &CaptureConfig,
            _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            stream.absurd()
        }

        fn retarget(
            stream: &Never,
            _target: &Never,
            _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
        ) {
            stream.absurd()
        }

        fn stop(stream: &Never, _done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
            stream.absurd()
        }

        fn now_us() -> u64 {
            static START: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);
            u64::try_from(START.elapsed().as_micros()).unwrap_or(u64::MAX)
        }

        fn target_bounds(_target: CaptureTarget) -> Option<Rect> {
            None
        }

        fn refresh_hz(_target: CaptureTarget) -> Option<f64> {
            None
        }

        fn display_bounds(_id: u32) -> Rect {
            Rect::default()
        }

        fn pointer_moves() -> u32 {
            0
        }

        fn pointer_location() -> (f64, f64) {
            (0.0, 0.0)
        }

        fn cursor_seed() -> Option<i32> {
            None
        }

        fn cursor_shape() -> Option<CursorShape> {
            None
        }
    }

    /// No video encoder.
    #[derive(Debug)]
    pub enum NoVideo {}

    impl VideoEncoder for NoVideo {
        type Image = ();

        fn new(
            config: EncoderConfig,
            _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, CodecError> {
            Err(CodecError::Unsupported(config.codec))
        }

        fn encode(&self, (): &(), _pts_us: u64, _options: &FrameOptions) -> Result<(), CodecError> {
            match *self {}
        }

        fn set_bitrate(&self, _bps: u32) -> Result<(), CodecError> {
            match *self {}
        }

        fn set_frame_rate(&self, _fps: u16) -> Result<(), CodecError> {
            match *self {}
        }
    }

    /// No audio encoder.
    #[derive(Debug)]
    pub enum NoAudio {}

    impl AudioEncoder for NoAudio {
        fn new() -> Result<Self, CodecError> {
            Err(CodecError::NoAudio)
        }

        fn push(&mut self, _samples: &[f32], _out: impl FnMut(&[u8])) -> Result<(), CodecError> {
            match *self {}
        }
    }

    /// No input: every event is refused.
    #[derive(Debug, Default)]
    pub struct NoInput(PointerWatch);

    impl InputSink for NoInput {
        fn new(_target: CaptureTarget, _scale: f64) -> Self {
            Self::default()
        }

        fn set_scale(&mut self, _scale: f64) {}

        fn set_bounds(&mut self, _bounds: Option<Rect>, _at: Instant) {}

        fn inject(&mut self, _input: &ScreenInput) -> Result<(), InputError> {
            Err(InputError::Unsupported)
        }

        fn focus(&mut self) -> Result<(), InputError> {
            Err(InputError::Unsupported)
        }

        fn release_all(&mut self) {}

        fn pointer(&self) -> PointerWatch {
            self.0.clone()
        }
    }
}
