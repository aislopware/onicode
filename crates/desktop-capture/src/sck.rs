//! [`CaptureSource`] on macOS: ScreenCaptureKit for the frames, CoreGraphics for where displays
//! are, the accessibility API for the focused field, and `NSCursor` for the pointer's picture.
//! Each function is the free function of the same job in this crate.

use std::sync::Arc;

use onicode_desktop_proto::screen::{CaptureTarget, CursorShape, DisplayInfo};

use crate::source::{
    AudioSink, CaptureConfig, CaptureError, CaptureSource, CapturedFrame, Console, FocusedField,
    Heard, Rect,
};
use crate::{Capture, Shareable, Target, geometry};

/// The macOS capture platform.
#[derive(Clone, Copy, Debug)]
pub enum ScreenCaptureKit {}

impl CaptureSource for ScreenCaptureKit {
    type Content = Shareable;
    type Image = onicode_desktop_codec::PixelBuffer;
    type Sound = Arc<Capture>;
    type Stream = Capture;
    type Target = Target;

    fn can_capture() -> bool {
        geometry::can_capture()
    }

    fn enumerate(done: impl FnOnce(Result<Shareable, CaptureError>) + Send + 'static) {
        crate::enumerate(done);
    }

    fn displays(content: &Shareable) -> Vec<DisplayInfo> {
        content.displays()
    }

    fn resolve(content: &Shareable, kind: CaptureTarget) -> Result<Target, CaptureError> {
        Target::resolve(content, kind)
    }

    fn pixel_size(target: &Target) -> (u32, u32) {
        target.pixel_size()
    }

    fn point_scale(target: &Target) -> f32 {
        target.point_scale()
    }

    fn start(
        target: &Target,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame) + Send + Sync + 'static,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<Capture, CaptureError> {
        Capture::start(target, config, sink, on_stop, done)
    }

    fn start_sound(
        heard: &Heard,
        sink: AudioSink,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<Arc<Capture>, CaptureError>) + Send + 'static,
    ) {
        let heard = heard.clone();
        crate::enumerate(move |content| {
            let target = match content.and_then(|content| Target::sound(&content, &heard)) {
                Ok(target) => target,
                Err(e) => return done(Err(e)),
            };
            // The start's answer can come before the capture is back from the call that
            // starts it, or after: whichever is second hands the capture on.
            let meeting = Arc::new(parking_lot::Mutex::new(Meeting::Waiting(Box::new(done))));
            let answered = Arc::clone(&meeting);
            let started = Capture::start_sound(&target, sink, on_stop, move |live| {
                answered.lock().answer(live);
            });
            match started {
                Ok(capture) => meeting.lock().started(capture),
                Err(e) => meeting.lock().answer(Err(e)),
            }
        });
    }

    fn hear(
        sound: &Arc<Capture>,
        heard: &Heard,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        let (sound, heard) = (Arc::clone(sound), heard.clone());
        crate::enumerate(move |content| {
            match content.and_then(|content| Target::sound(&content, &heard)) {
                Ok(target) => sound.retarget(&target, done),
                Err(e) => done(Err(e)),
            }
        });
    }

    fn stop_sound(
        sound: &Arc<Capture>,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        sound.stop(done);
    }

    fn update(
        stream: &Capture,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        stream.update(config, done);
    }

    fn retarget(
        stream: &Capture,
        target: &Target,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        stream.retarget(target, done);
    }

    fn stop(stream: &Capture, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        stream.stop(done);
    }

    fn now_us() -> u64 {
        crate::host_now_us()
    }

    fn console() -> Option<Console> {
        crate::console()
    }

    fn focused_field() -> Option<FocusedField> {
        crate::focus::focused_field()
    }

    fn target_bounds(target: CaptureTarget) -> Option<Rect> {
        geometry::target_bounds(target)
    }

    fn refresh_hz(target: CaptureTarget) -> Option<f64> {
        geometry::target_refresh_hz(target)
    }

    fn display_bounds(id: u32) -> Rect {
        geometry::display_bounds(id)
    }

    fn pointer_moves() -> u32 {
        geometry::pointer_moves()
    }

    fn pointer_location() -> (f64, f64) {
        geometry::pointer_location()
    }

    fn cursor_seed() -> Option<i32> {
        crate::cursor_seed()
    }

    fn cursor_shape() -> Option<CursorShape> {
        crate::read_cursor()
    }
}

/// What a sound capture's start is waiting on: the capture back from the call that started it,
/// and ScreenCaptureKit's answer that it is live.
enum Meeting {
    /// Neither yet.
    Waiting(Box<dyn FnOnce(Result<Arc<Capture>, CaptureError>) + Send>),
    /// The capture, the answer not yet.
    Started(
        Capture,
        Box<dyn FnOnce(Result<Arc<Capture>, CaptureError>) + Send>,
    ),
    /// The answer, the capture not yet.
    Answered(
        Result<(), CaptureError>,
        Box<dyn FnOnce(Result<Arc<Capture>, CaptureError>) + Send>,
    ),
    /// Told.
    Done,
}

impl Meeting {
    fn started(&mut self, capture: Capture) {
        *self = match std::mem::replace(self, Self::Done) {
            Self::Waiting(done) => Self::Started(capture, done),
            Self::Answered(live, done) => {
                done(live.map(|()| Arc::new(capture)));
                Self::Done
            }
            told @ (Self::Started(..) | Self::Done) => told,
        };
    }

    fn answer(&mut self, live: Result<(), CaptureError>) {
        *self = match std::mem::replace(self, Self::Done) {
            Self::Waiting(done) => Self::Answered(live, done),
            Self::Started(capture, done) => {
                done(live.map(|()| Arc::new(capture)));
                Self::Done
            }
            told @ (Self::Answered(..) | Self::Done) => told,
        };
    }
}
