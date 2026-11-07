//! One `SCStream` delivering pixel buffers to a sink.

use std::ptr::NonNull;

use block2::RcBlock;
use dispatch2::{DispatchQoS, DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread as _, DefinedClass as _, define_class, msg_send};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved,
    kAudioFormatLinearPCM,
};
use objc2_core_foundation::{
    CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{CGColor, kCGColorBlack, kCGDisplayStreamYCbCrMatrix_ITU_R_709_2};
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMClock, CMSampleBuffer,
    CMTime, CMTimeFlags, kCMTimeZero,
};
use objc2_core_video::{
    CVPixelBuffer, kCVPixelFormatType_32BGRA, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_screen_capture_kit::{
    SCCaptureDynamicRange, SCContentFilter, SCDisplay, SCFrameStatus, SCRunningApplication,
    SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamFrameInfo,
    SCStreamFrameInfoDisplayTime, SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
    SCWindow,
};
use onicode_desktop_codec::{PixelBuffer, micros};
use onicode_desktop_proto::screen::CaptureTarget;
use parking_lot::Mutex;

use crate::Shareable;
use crate::source::{
    AUDIO_CHANNELS, AUDIO_RATE, AudioSink, CaptureConfig, CaptureError, CapturedAudio,
    CapturedFrame, Heard, PixelFormat,
};

impl PixelFormat {
    const fn os_type(self) -> u32 {
        match self {
            Self::Nv12Full => kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            Self::Yuv444Full10 => kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
            Self::Bgra => kCVPixelFormatType_32BGRA,
        }
    }
}

/// What a fresh `SCStreamConfiguration` holds before anything is set on it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SckDefaults {
    /// `queueDepth`.
    pub queue_depth: isize,
    /// `minimumFrameInterval` in seconds; `None` when invalid (no cap).
    pub minimum_frame_interval_s: Option<f64>,
}

/// Read ScreenCaptureKit's defaults back from a configuration nothing was set on.
#[must_use]
pub fn sck_defaults() -> SckDefaults {
    // SAFETY: plain constructor.
    let c = unsafe { SCStreamConfiguration::new() };
    // SAFETY: plain getter on a fresh configuration object.
    let queue_depth = unsafe { c.queueDepth() };
    // SAFETY: as above.
    let interval = unsafe { c.minimumFrameInterval() };
    let minimum_frame_interval_s =
        (interval.flags.contains(CMTimeFlags::Valid) && interval.timescale > 0).then(|| {
            #[expect(clippy::cast_precision_loss, reason = "a small ratio")]
            let seconds = interval.value as f64 / f64::from(interval.timescale);
            seconds
        });
    SckDefaults {
        queue_depth,
        minimum_frame_interval_s,
    }
}

/// The windows this process put over the displays, which no display capture shows.
static LEFT_OUT: parking_lot::RwLock<Vec<u32>> = parking_lot::RwLock::new(Vec::new());

/// Leave the windows numbered `windows` out of every display this process captures from now on.
///
/// These are the curtain's shield ([`crate::Shield`]), and they replace the ones left out
/// before. A capture already running keeps its filter until it is resolved again, which the
/// curtain asks of every running capture before it shows the shield.
///
/// Process-wide, because the windows are: whatever display a stream shows, this process's own
/// shield is never what its client should see.
pub fn leave_out(windows: Vec<u32>) {
    *LEFT_OUT.write() = windows;
}

/// What to capture, resolved from a [`Shareable`] snapshot.
pub struct Target {
    kind: CaptureTarget,
    filter: Retained<SCContentFilter>,
    pixel_size: (u32, u32),
    point_scale: f32,
}

// SAFETY: `SCContentFilter` is an immutable description object that ScreenCaptureKit reads
// from its own queues; nothing here mutates it after construction.
unsafe impl Send for Target {}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("kind", &self.kind)
            .field("pixel_size", &self.pixel_size)
            .finish_non_exhaustive()
    }
}

impl Target {
    /// Resolve a target against a content snapshot. A display leaves out the windows this
    /// process put over the displays ([`leave_out`]).
    pub fn resolve(content: &Shareable, kind: CaptureTarget) -> Result<Self, CaptureError> {
        let left_out = LEFT_OUT.read().clone();
        Self::resolve_excluding(content, kind, &left_out)
    }

    /// Resolve a display, leaving out the windows numbered `excluded` (the curtain's shield).
    ///
    /// # Errors
    ///
    /// [`CaptureError::NotFound`] for a target the snapshot does not list, and
    /// [`CaptureError::Unlisted`] for a display when a window to leave out is not in it: the
    /// snapshot is older than the window, and showing it is never the fallback.
    pub fn resolve_excluding(
        content: &Shareable,
        kind: CaptureTarget,
        excluded: &[u32],
    ) -> Result<Self, CaptureError> {
        crate::ensure_core_graphics();
        let CaptureTarget::Display(id) = kind;
        let display = content.display(id.0).ok_or(CaptureError::NotFound(kind))?;
        let left_out = excluded
            .iter()
            .map(|number| {
                content
                    .window(*number)
                    .ok_or(CaptureError::Unlisted(*number))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::display_without(kind, &display, &left_out))
    }

    /// The filter a sound capture of `heard` reads through: the main display with every
    /// application, or with only `heard`'s. Sound is filtered by application whatever display
    /// the filter names.
    pub fn sound(content: &Shareable, heard: &Heard) -> Result<Self, CaptureError> {
        crate::ensure_core_graphics();
        let main = objc2_core_graphics::CGMainDisplayID();
        let kind = CaptureTarget::Display(onicode_desktop_proto::DisplayId(main));
        let display = content.display(main).ok_or(CaptureError::NotFound(kind))?;
        Ok(match heard {
            Heard::Every => Self::display(kind, &display),
            Heard::Apps(pids) => Self::display_of_apps(kind, &display, &content.applications(pids)),
        })
    }

    fn display(kind: CaptureTarget, display: &SCDisplay) -> Self {
        Self::display_without(kind, display, &[])
    }

    /// The display filter with `left_out`'s windows left out of the picture.
    fn display_without(
        kind: CaptureTarget,
        display: &SCDisplay,
        left_out: &[Retained<SCWindow>],
    ) -> Self {
        let windows: Vec<&SCWindow> = left_out.iter().map(|w| &**w).collect();
        let windows: Retained<NSArray<SCWindow>> = NSArray::from_slice(&windows);
        // SAFETY: as above.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                display,
                &windows,
            )
        };
        let (pixel_size, point_scale) = filter_pixel_size(&filter);
        Self {
            kind,
            filter,
            pixel_size,
            point_scale,
        }
    }

    /// The display filter restricted to `apps`: their windows, their audio.
    fn display_of_apps(
        kind: CaptureTarget,
        display: &SCDisplay,
        apps: &[Retained<SCRunningApplication>],
    ) -> Self {
        let apps: Vec<&SCRunningApplication> = apps.iter().map(|app| &**app).collect();
        let apps: Retained<NSArray<SCRunningApplication>> = NSArray::from_slice(&apps);
        let none: Retained<NSArray<SCWindow>> = NSArray::from_slice(&[]);
        // SAFETY: as above.
        let filter = unsafe {
            SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                display,
                &apps,
                &none,
            )
        };
        let (pixel_size, point_scale) = filter_pixel_size(&filter);
        Self {
            kind,
            filter,
            pixel_size,
            point_scale,
        }
    }

    /// Bundle ids of the applications the filter is restricted to: a sound capture's heard
    /// applications, none for a plain display filter.
    #[must_use]
    pub fn included_applications(&self) -> Vec<String> {
        // SAFETY: plain getter on a valid filter object.
        let apps = unsafe { self.filter.includedApplications() };
        apps.iter()
            .map(|a| {
                // SAFETY: plain getter on a valid object.
                unsafe { a.bundleIdentifier() }.to_string()
            })
            .collect()
    }

    /// The target this resolves.
    #[must_use]
    pub const fn kind(&self) -> CaptureTarget {
        self.kind
    }

    /// Native pixel size of the target, rounded up to even.
    #[must_use]
    pub const fn pixel_size(&self) -> (u32, u32) {
        self.pixel_size
    }

    /// Native pixels per point of the target (the backing scale of its display).
    #[must_use]
    pub const fn point_scale(&self) -> f32 {
        self.point_scale
    }
}

/// Pixel size of a filter's content rect, rounded up to even, and its points-to-pixels scale.
fn filter_pixel_size(filter: &SCContentFilter) -> ((u32, u32), f32) {
    // SAFETY: plain getters on a valid object.
    let rect = unsafe { filter.contentRect() };
    // SAFETY: as above.
    let point_scale = unsafe { filter.pointPixelScale() };
    let scale = f64::from(point_scale);
    let even = |v: f64| -> u32 {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped"
        )]
        let px = (v * scale).round().clamp(2.0, 16_384.0) as u32;
        px.next_multiple_of(2)
    };
    ((even(rect.size.width), even(rect.size.height)), point_scale)
}

type FrameSink = Box<dyn Fn(CapturedFrame) + Send + Sync>;
type StopSink = Box<dyn Fn(CaptureError) + Send + Sync>;

struct Ivars {
    sink: FrameSink,
    audio: Option<AudioSink>,
    on_stop: StopSink,
    /// The last frame status seen, so a change is logged once (`SCFrameStatus` raw value; -1
    /// before the first frame).
    last_status: std::sync::atomic::AtomicIsize,
}

define_class!(
    // SAFETY:
    // - `NSObject` has no subclassing requirements.
    // - `Output` does not implement `Drop`.
    #[unsafe(super(NSObject))]
    #[name = "OnicodeStreamOutput"]
    #[ivars = Ivars]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind == SCStreamOutputType::Screen {
                self.screen_sample(sample);
            } else if kind == SCStreamOutputType::Audio {
                self.audio_sample(sample);
            } else {
                tracing::trace!(?kind, "other sample");
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn did_stop(&self, _stream: &SCStream, error: &NSError) {
            (self.ivars().on_stop)(CaptureError::from_ns(error));
        }
    }
);

impl Output {
    fn new(sink: FrameSink, audio: Option<AudioSink>, on_stop: StopSink) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars {
            sink,
            audio,
            on_stop,
            last_status: std::sync::atomic::AtomicIsize::new(-1),
        });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
        unsafe { msg_send![super(this), init] }
    }

    fn screen_sample(&self, sample: &CMSampleBuffer) {
        let status = frame_status(sample);
        let raw = status.map_or(-2, |s| s.0);
        let before = self
            .ivars()
            .last_status
            .swap(raw, std::sync::atomic::Ordering::Relaxed);
        if before != raw {
            // Complete 0, Idle 1, Blank 2, Suspended 3, Started 4, Stopped 5 (SCStream.h): the
            // framework's own word on why frames stop, logged once per change.
            tracing::debug!(from = before, to = raw, "capture frame status");
        }
        if status != Some(SCFrameStatus::Complete) {
            return;
        }
        // SAFETY: valid sample buffer.
        let Some(image) = (unsafe { sample.image_buffer() }) else {
            return;
        };
        // SAFETY: valid sample buffer.
        let pts = unsafe { sample.presentation_time_stamp() };
        let capture_ts_us = micros(pts).unwrap_or(0);
        let display_ts_us = display_time(sample);
        let now_us = host_now_us();
        let age_us = now_us.saturating_sub(capture_ts_us);
        let image: CFRetained<CVPixelBuffer> = image;
        (self.ivars().sink)(CapturedFrame {
            image: PixelBuffer::from_retained(image),
            capture_ts_us,
            display_ts_us,
            age_us,
            latency_us: display_ts_us.map_or(age_us, |t| now_us.saturating_sub(t)),
        });
    }
}

impl Output {
    /// Interleave the sample's PCM and hand it to the audio sink.
    fn audio_sample(&self, sample: &CMSampleBuffer) {
        let Some(sink) = &self.ivars().audio else {
            return;
        };
        // SAFETY: valid sample buffer.
        let Some(description) = (unsafe { sample.format_description() }) else {
            tracing::trace!("audio sample without a format description");
            return;
        };
        // SAFETY: CoreMedia rule: the pointer is into the description, valid while it lives.
        let asbd = unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&description) };
        // SAFETY: as above; null for a non-audio description.
        let Some(asbd) = (unsafe { asbd.as_ref() }) else {
            tracing::trace!("audio sample with a non-audio description");
            return;
        };
        let float = asbd.mFormatID == kAudioFormatLinearPCM
            && asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
            && asbd.mBitsPerChannel == 32;
        if !float {
            tracing::warn!(
                format = asbd.mFormatID,
                flags = asbd.mFormatFlags,
                "audio not float32"
            );
            return;
        }
        let non_interleaved = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
        let channels = asbd.mChannelsPerFrame.max(1) as usize;
        // SAFETY: valid sample buffer.
        let frames = usize::try_from(unsafe { sample.num_samples() }).unwrap_or(0);
        if frames == 0 {
            tracing::trace!("empty audio sample");
            return;
        }
        // Ask CoreMedia how big the (flexible) buffer list is, then fetch it.
        let mut needed: usize = 0;
        // SAFETY: CoreMedia rule: a null list with a size-out pointer only reports the size.
        let status = unsafe {
            sample.audio_buffer_list_with_retained_block_buffer(
                &raw mut needed,
                std::ptr::null_mut(),
                0,
                None,
                None,
                0,
                std::ptr::null_mut(),
            )
        };
        if needed == 0 {
            tracing::debug!(status, "audio buffer list size");
            return;
        }
        let words = needed.div_ceil(size_of::<u64>());
        let mut list: Vec<u64> = vec![0; words];
        let mut block: *mut CMBlockBuffer = std::ptr::null_mut();
        // SAFETY: CoreMedia rule: the list is `needed` bytes, 8-byte aligned, which is what the
        // struct wants; the block buffer comes back retained and is released below.
        let status = unsafe {
            sample.audio_buffer_list_with_retained_block_buffer(
                std::ptr::null_mut(),
                list.as_mut_ptr().cast::<AudioBufferList>(),
                needed,
                None,
                None,
                0,
                &raw mut block,
            )
        };
        if status != 0 {
            tracing::debug!(status, needed, "audio buffer list");
            return;
        }
        // SAFETY: CoreMedia rule (Copy rule): the out block buffer is +1; owning it here
        // releases it when we are done reading the list it backs.
        let _block = NonNull::new(block).map(|p| unsafe { CFRetained::from_raw(p) });
        let head = list.as_ptr().cast::<AudioBufferList>();
        // SAFETY: CoreMedia filled a well-formed `AudioBufferList`: the count comes first.
        let count = usize::try_from(unsafe { (*head).mNumberBuffers }).unwrap_or(0);
        // SAFETY: `mBuffers` is the flexible array member at the struct's own offset.
        let first = unsafe { std::ptr::addr_of!((*head).mBuffers) }.cast::<AudioBuffer>();
        // Never trust the count past the bytes CoreMedia said it wrote.
        let fit = needed
            .saturating_sub(std::mem::offset_of!(AudioBufferList, mBuffers))
            .checked_div(size_of::<AudioBuffer>())
            .unwrap_or(0);
        // SAFETY: `count.min(fit)` `AudioBuffer`s follow contiguously inside the `needed` bytes.
        let entries = unsafe { std::slice::from_raw_parts(first, count.min(fit)) };
        let samples = interleave(entries, frames, non_interleaved, channels);
        if samples.is_empty() {
            return;
        }
        // SAFETY: valid sample buffer.
        let pts = unsafe { sample.presentation_time_stamp() };
        sink(CapturedAudio {
            pts_us: micros(pts).unwrap_or(0),
            samples,
        });
    }
}

/// Stereo interleaved samples from ScreenCaptureKit's buffers (mono duplicated, extra
/// channels dropped).
fn interleave(
    entries: &[AudioBuffer],
    frames: usize,
    non_interleaved: bool,
    channels: usize,
) -> Vec<f32> {
    let floats = |b: &AudioBuffer| -> &[f32] {
        let n = usize::try_from(b.mDataByteSize)
            .unwrap_or(0)
            .checked_div(size_of::<f32>())
            .unwrap_or(0);
        if b.mData.is_null() || n == 0 {
            return &[];
        }
        // SAFETY: CoreMedia rule: `mData` holds `mDataByteSize` bytes of the described
        // float32 PCM for as long as the block buffer lives (the caller holds it).
        unsafe { std::slice::from_raw_parts(b.mData.cast::<f32>(), n) }
    };
    let mut out = Vec::with_capacity(frames.saturating_mul(2));
    if non_interleaved {
        let left = entries.first().map_or(&[][..], floats);
        let right = entries.get(1).map_or(left, floats);
        for (i, &l) in left.iter().take(frames).enumerate() {
            out.push(l);
            out.push(right.get(i).copied().unwrap_or(l));
        }
    } else {
        let data = entries.first().map_or(&[][..], floats);
        for frame in data.chunks_exact(channels.max(1)).take(frames) {
            let l = frame.first().copied().unwrap_or(0.0);
            let r = frame.get(1).copied().unwrap_or(l);
            out.push(l);
            out.push(r);
        }
    }
    out
}

/// A numeric `SCStreamFrameInfo*` entry from the sample's first attachment dictionary.
fn frame_info(sample: &CMSampleBuffer, key: &SCStreamFrameInfo) -> Option<i64> {
    // SAFETY: valid sample buffer; `false` never allocates.
    let array = unsafe { sample.sample_attachments_array(false) }?;
    // SAFETY: CoreMedia documents the array's elements as CFDictionaries keyed by CFString.
    let array: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(array) };
    let dict = array.get(0)?;
    let key_ptr: NonNull<NSString> = NonNull::from(key);
    // SAFETY: `NSString` and `CFString` are toll-free bridged; the pointee is a constant.
    let key: &CFString = unsafe { key_ptr.cast::<CFString>().as_ref() };
    dict.get(key)?.downcast::<CFNumber>().ok()?.as_i64()
}

/// `SCStreamFrameInfoStatus` from the sample's first attachment dictionary.
fn frame_status(sample: &CMSampleBuffer) -> Option<SCFrameStatus> {
    // SAFETY: framework-provided constant string.
    let key = unsafe { SCStreamFrameInfoStatus };
    let status = frame_info(sample, key)?;
    Some(SCFrameStatus(isize::try_from(status).ok()?))
}

/// `SCStreamFrameInfoDisplayTime` (mach absolute time) on the host clock, microseconds.
fn display_time(sample: &CMSampleBuffer) -> Option<u64> {
    // SAFETY: framework-provided constant string.
    let key = unsafe { SCStreamFrameInfoDisplayTime };
    let ticks = u64::try_from(frame_info(sample, key)?).ok()?;
    // SAFETY: CoreMedia rule: converts `mach_absolute_time` units to the host time clock.
    micros(unsafe { CMClock::make_host_time_from_system_units(ticks) })
}

/// Now on the host time clock, microseconds; the clock ScreenCaptureKit stamps frames with.
#[must_use]
pub fn host_now_us() -> u64 {
    // SAFETY: the host time clock always exists.
    let clock = unsafe { CMClock::host_time_clock() };
    // SAFETY: valid clock.
    micros(unsafe { clock.time() }).unwrap_or(0)
}

/// A running capture stream.
pub struct Capture {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    /// The video and the audio sample queues.
    _queues: [DispatchRetained<DispatchQueue>; 2],
    target: CaptureTarget,
}

// SAFETY: `SCStream` methods may be called from any thread (ScreenCaptureKit dispatches
// internally); the output object is only ever touched by ScreenCaptureKit's queue.
unsafe impl Send for Capture {}
// SAFETY: as above; `&Capture` only exposes `SCStream` calls.
unsafe impl Sync for Capture {}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl Capture {
    /// Build a stream of pictures and start it. `done` runs on a ScreenCaptureKit queue once the
    /// stream is live (or failed); frames follow on `sink`, and `on_stop` fires if the stream
    /// dies.
    pub fn start(
        target: &Target,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame) + Send + Sync + 'static,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<Self, CaptureError> {
        Self::start_with(
            target,
            config,
            Box::new(sink),
            None,
            Box::new(on_stop),
            done,
        )
    }

    /// Build a stream of `target`'s sound and start it: its audio goes to `audio`, and the
    /// pictures ScreenCaptureKit makes beside it, 2 × 2 pixels once a second, go nowhere.
    pub fn start_sound(
        target: &Target,
        audio: AudioSink,
        on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<Self, CaptureError> {
        let config = CaptureConfig {
            width: SOUND_PICTURE,
            height: SOUND_PICTURE,
            align: 1,
            fps: 1,
            format: PixelFormat::Nv12Full,
            queue_depth: 3,
        };
        let pictures = Box::new(|_frame: CapturedFrame| {});
        Self::start_with(
            target,
            &config,
            pictures,
            Some(audio),
            Box::new(on_stop),
            done,
        )
    }

    fn start_with(
        target: &Target,
        config: &CaptureConfig,
        sink: FrameSink,
        audio: Option<AudioSink>,
        on_stop: StopSink,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<Self, CaptureError> {
        let wants_audio = audio.is_some();
        let configuration = stream_configuration(config, wants_audio);
        let output = Output::new(sink, audio, on_stop);
        let delegate: &ProtocolObject<dyn SCStreamDelegate> = ProtocolObject::from_ref(&*output);
        // SAFETY: filter, configuration and delegate are valid; the delegate is retained by the
        // stream (it is also kept alive by `Capture`).
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &target.filter,
                &configuration,
                Some(delegate),
            )
        };
        // Both queues are user-interactive. The video callback is where a frame is gated and
        // handed to the encoder, on the path from the window server to the client's glass; a
        // queue left at the default class runs behind whatever else the host's machine is
        // doing, and every millisecond it waits is a millisecond of latency on every frame.
        let interactive = DispatchQueueAttr::with_qos_class(
            DispatchQueueAttr::SERIAL,
            DispatchQoS::UserInteractive,
            0,
        );
        let queue = DispatchQueue::new("dev.onicode.capture", Some(&interactive));
        // Audio on its own queue: a sample never waits behind a frame's encode call, and the
        // player's ~40 ms of slack is less than a few late video callbacks.
        let audio_queue = DispatchQueue::new("dev.onicode.capture.audio", Some(&interactive));
        let handler: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
        // SAFETY: valid stream, output and queue.
        unsafe {
            stream.addStreamOutput_type_sampleHandlerQueue_error(
                handler,
                SCStreamOutputType::Screen,
                Some(&queue),
            )
        }
        .map_err(|e| CaptureError::from_ns(&e))?;
        if wants_audio {
            // SAFETY: valid stream, output and queue.
            unsafe {
                stream.addStreamOutput_type_sampleHandlerQueue_error(
                    handler,
                    SCStreamOutputType::Audio,
                    Some(&audio_queue),
                )
            }
            .map_err(|e| CaptureError::from_ns(&e))?;
        }
        let block = completion(done);
        // SAFETY: the block is copied by the framework; it captures only `Send` data.
        unsafe {
            stream.startCaptureWithCompletionHandler(Some(&block));
        }
        Ok(Self {
            stream,
            _output: output,
            _queues: [queue, audio_queue],
            target: target.kind(),
        })
    }

    /// What is being captured.
    #[must_use]
    pub const fn target(&self) -> CaptureTarget {
        self.target
    }

    /// Change size, rate or format on the live stream.
    pub fn update(
        &self,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        let configuration = stream_configuration(config, false);
        let block = completion(done);
        // SAFETY: valid stream and configuration; the block is copied by the framework.
        unsafe {
            self.stream
                .updateConfiguration_completionHandler(&configuration, Some(&block))
        }
    }

    /// Swap the content filter on the live stream: another display, or the same one with other
    /// windows left out.
    pub fn retarget(
        &self,
        target: &Target,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        let block = completion(done);
        // SAFETY: valid stream and filter; the block is copied by the framework.
        unsafe {
            self.stream
                .updateContentFilter_completionHandler(&target.filter, Some(&block))
        }
    }

    /// Stop; `done` runs once ScreenCaptureKit has torn the stream down.
    pub fn stop(&self, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        let block = completion(done);
        // SAFETY: valid stream; the block is copied by the framework.
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&block)) }
    }
}

fn completion(
    done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
) -> RcBlock<dyn Fn(*mut NSError)> {
    let done = Mutex::new(Some(done));
    RcBlock::new(move |error: *mut NSError| {
        let Some(done) = done.lock().take() else {
            return;
        };
        let result = match NonNull::new(error) {
            // SAFETY: ScreenCaptureKit passes a live error object for the callback's duration.
            Some(error) => Err(CaptureError::from_ns(unsafe { error.as_ref() })),
            None => Ok(()),
        };
        done(result);
    })
}

/// The side of the pictures a sound capture makes beside its sound: ScreenCaptureKit makes
/// pictures for every stream, and one of sound alone wants as few as it will take.
pub(crate) const SOUND_PICTURE: u32 = 2;

fn stream_configuration(config: &CaptureConfig, audio: bool) -> Retained<SCStreamConfiguration> {
    // SAFETY: plain constructor.
    let c = unsafe { SCStreamConfiguration::new() };
    let interval = if config.fps == 0 {
        // SCStream.h: "Set this to kCMTimeZero to capture at display's native refresh rate."
        // SAFETY: an immutable CoreMedia constant, read by value.
        unsafe { kCMTimeZero }
    } else {
        CMTime {
            value: 1,
            timescale: i32::from(config.fps),
            flags: CMTimeFlags::Valid,
            epoch: 0,
        }
    };
    let (width, height) = config.surface();
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setWidth(usize::try_from(width).unwrap_or(usize::MAX));
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setHeight(usize::try_from(height).unwrap_or(usize::MAX));
    }
    // The picture at the surface's top-left, at its own size; `scalesToFit` and
    // `preservesAspectRatio` act inside this rectangle. `SCStream.h`: "The rectangle is specified
    // in pixels".
    let picture = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: f64::from(config.width),
            height: f64::from(config.height),
        },
    };
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setDestinationRect(picture);
    }
    // The padding, and any letterbox, is black: "By default the background color is clear"
    // (`SCStream.h`), which a Y′CbCr surface has no value for.
    // SAFETY: framework-provided constant string.
    let name = unsafe { kCGColorBlack };
    // `CGColorGetConstantColor` (CGColor.h) returns a colour the system owns and never frees,
    // which the setter's unretained (`assign`) property needs.
    if let Some(black) = CGColor::constant_color(Some(name)) {
        // SAFETY: plain property write on the fresh configuration object; the colour outlives
        // it (above).
        unsafe {
            c.setBackgroundColor(&black);
        }
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setMinimumFrameInterval(interval);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setPixelFormat(config.format.os_type());
    }
    if matches!(
        config.format,
        PixelFormat::Nv12Full | PixelFormat::Yuv444Full10
    ) {
        // SCStream.h leaves the YCbCr matrix's default unsaid; the encoder tags BT.709 and the
        // client converts with whatever the stream says, so the capture must be BT.709 too.
        // SAFETY: plain property write on the fresh configuration object; the value is one of
        // the `kCGDisplayStreamYCbCrMatrix_*` strings the header asks for.
        unsafe {
            c.setColorMatrix(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2);
        }
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setShowsCursor(false);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setQueueDepth(isize::from(config.queue_depth.max(1)));
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setScalesToFit(true);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setPreservesAspectRatio(true);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setCapturesAudio(audio);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setSampleRate(isize::try_from(AUDIO_RATE).unwrap_or(isize::MAX));
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setChannelCount(isize::try_from(AUDIO_CHANNELS).unwrap_or(isize::MAX));
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setExcludesCurrentProcessAudio(true);
    }
    // SAFETY: plain property write on the fresh configuration object.
    unsafe {
        c.setCaptureDynamicRange(SCCaptureDynamicRange::SDR);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The video format asked of ScreenCaptureKit is full range with the matrix the encoder
    /// tags, so nothing between the capture and the client's shader converts or guesses; a
    /// full-chroma stream asks for `xf44` under the same matrix.
    #[test]
    fn the_capture_is_full_range_bt709() {
        for (asked, os_type) in [
            (
                PixelFormat::Nv12Full,
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            ),
            (
                PixelFormat::Yuv444Full10,
                kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
            ),
        ] {
            let config = CaptureConfig {
                width: 64,
                height: 64,
                align: 16,
                fps: 60,
                format: asked,
                queue_depth: 2,
            };
            let c = stream_configuration(&config, false);
            // SAFETY: plain getter on a valid configuration object.
            let format = unsafe { c.pixelFormat() };
            // SAFETY: as above.
            let matrix = unsafe { c.colorMatrix() };
            assert_eq!(format, os_type, "{asked:?}");
            // SAFETY: framework-provided constant string.
            let bt709 = unsafe { kCGDisplayStreamYCbCrMatrix_ITU_R_709_2 };
            assert_eq!(matrix.to_string(), bt709.to_string(), "{asked:?}");
        }
    }

    /// A 3024 × 1964 picture is captured into a 3024 × 1968 surface, drawn at its top-left at
    /// its own size over black; one already aligned fills its surface.
    #[test]
    fn a_padded_surface_holds_the_picture_at_its_top_left() {
        for ((width, height), surface) in [
            ((3024, 1964), (3024_usize, 1968_usize)),
            ((1920, 1088), (1920, 1088)),
        ] {
            let config = CaptureConfig {
                width,
                height,
                align: 16,
                fps: 0,
                format: PixelFormat::Nv12Full,
                queue_depth: 3,
            };
            let c = stream_configuration(&config, false);
            // SAFETY: plain getter on a valid configuration object.
            let w = unsafe { c.width() };
            // SAFETY: as above.
            let h = unsafe { c.height() };
            // SAFETY: as above.
            let rect = unsafe { c.destinationRect() };
            assert_eq!((w, h), surface, "the surface");
            assert_eq!(
                (
                    rect.origin.x,
                    rect.origin.y,
                    rect.size.width,
                    rect.size.height
                ),
                (0.0, 0.0, f64::from(width), f64::from(height)),
                "the picture"
            );
            // SAFETY: plain getter; the colour is the system's constant black.
            let background = unsafe { c.backgroundColor() };
            // SAFETY: framework-provided constant string.
            let name = unsafe { kCGColorBlack };
            let black = CGColor::constant_color(Some(name)).unwrap();
            assert!(
                CGColor::equal_to_color(Some(&background), Some(&black)),
                "black"
            );
        }
    }
}
