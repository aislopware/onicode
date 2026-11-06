//! `VTCompressionSession` for interactive streaming (macOS worker only).

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU16, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType, Type as _,
};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
    kCMSampleAttachmentKey_IsDependedOnByOthers, kCMSampleAttachmentKey_NotSync, kCMTimeInvalid,
    kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSession, VTSessionCopySupportedPropertyDictionary,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AllowOpenGOP,
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_BaseLayerBitRateFraction,
    kVTCompressionPropertyKey_BaseLayerFrameRateFraction,
    kVTCompressionPropertyKey_CalculateMeanSquaredError, kVTCompressionPropertyKey_DataRateLimits,
    kVTCompressionPropertyKey_EnableLTR, kVTCompressionPropertyKey_ExpectedFrameRate,
    kVTCompressionPropertyKey_MaxFrameDelayCount, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_MaximumRealTimeFrameRate,
    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_AcknowledgedLTRTokens,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTEncodeFrameOptionKey_ForceLTRRefresh,
    kVTInvalidSessionErr, kVTProfileLevel_H264_High_AutoLevel, kVTProfileLevel_HEVC_Main_AutoLevel,
    kVTPropertyNotSupportedErr, kVTPropertySupportedValueListKey,
    kVTSampleAttachmentKey_QualityMetrics, kVTSampleAttachmentKey_RequireLTRAcknowledgementToken,
    kVTSampleAttachmentQualityMetricsKey_ChromaBlueMeanSquaredError,
    kVTSampleAttachmentQualityMetricsKey_ChromaRedMeanSquaredError,
    kVTSampleAttachmentQualityMetricsKey_LumaMeanSquaredError, kVTSessionMalfunctionErr,
    kVTVideoEncoderMalfunctionErr, kVTVideoEncoderNotAvailableNowErr,
    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};
use onicode_desktop_media::wire::VideoCodec;

use crate::cf::{self, check};
use crate::stripes::{Stripe, StripeCopy};
use crate::video::{Chroma, EncodedPacket, EncoderConfig, FrameOptions, Mse, VideoEncoder};
use crate::{CodecError, PixelBuffer, nal};

/// The 10-bit 4:4:4 profile's `ProfileLevel` value. The low-latency HEVC encoder lists it in its
/// own supported values (`VTCopySupportedPropertyDictionaryForEncoder`, macOS 27.0 on an M1 Max)
/// but no SDK header exports it (`VTCompressionProperties.h` in the macOS 27.0 SDK stops at
/// `kVTProfileLevel_HEVC_Main42210_AutoLevel`), so the session's own string is looked up by this
/// name and used, never this literal.
const MAIN_444_10: &str = "HEVC_Main44410_AutoLevel";

/// The `CVPixelFormatType` a session of `chroma` is fed: full-range NV12 (`420f`) for 4:2:0,
/// full-range 10-bit bi-planar 4:4:4 (`xf44`) for 4:4:4.
#[must_use]
pub const fn pixel_format(chroma: Chroma) -> u32 {
    match chroma {
        Chroma::Subsampled => objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        Chroma::Full => kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
    }
}

/// Which rate-control mode a session runs in. The worker only ever runs the low-latency one;
/// the other exists for the measurement that rejected it (`experiments` feature).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(
    not(feature = "experiments"),
    expect(
        unreachable_pub,
        reason = "re-exported only for the rate-control experiments"
    )
)]
pub enum RateControl {
    /// `EnableLowLatencyRateControl` in the encoder specification (infinite GOP, no
    /// reordering, LTR available) with `AverageBitRate` + `DataRateLimits`: the ruled mode.
    #[default]
    LowLatency,
    /// macOS 26's `VariableBitRate` + `VBVMaxBitRate` + `VBVBufferDuration` on a session
    /// *without* low-latency rate control (the header says they are incompatible with it).
    /// Measured against `LowLatency` in MEASUREMENTS.md; not used by the worker.
    #[cfg(feature = "experiments")]
    Vbv,
}

type Sink = Box<dyn Fn(EncodedPacket) + Send + Sync>;

fn retain(value: &CFType) -> CFRetained<CFType> {
    CFType::retain(value)
}

/// What a session made for 60 frames a second or more declares as its frame rate until its
/// first frame ([`placement_fps`]). VideoToolbox places a session on one of this Mac's encode
/// engines, and fixes its internal set-up, from the `ExpectedFrameRate` last set between
/// `PrepareToEncodeFrames` and the first frame: a value set only before the prepare places
/// nothing, and nothing after the first frame moves the session. Two 1080p sessions declaring 60
/// share one engine and take turns, 6.6 → 11.8 ms p95 for either. Declaring 120, they run on the
/// two engines side by side at 6.7, and six keep 60 frames a second where they made 45. One
/// declaring 60 beside one declaring 120 got an engine of its own only half the time. A lone
/// session pays up to 0.6 dB at 60 frames a second, and only where the picture is already above
/// 54 dB (MEASUREMENTS.md, "several streams on the encode engines"). The same as
/// `MaximumRealTimeFrameRate`, the most any stream is fed.
const PLACEMENT_FPS: u16 = 120;

/// The rate a session made for `fps` declares until its first frame: [`PLACEMENT_FPS`] from 60
/// up, its own below that, where declaring 120 costs 0.6–2 dB and the stream was asked to be
/// slow.
const fn placement_fps(fps: u16) -> u16 {
    if fps >= 60 && fps < PLACEMENT_FPS {
        PLACEMENT_FPS
    } else {
        fps
    }
}

/// The share of frames in the base layer while temporal layers are on: every other frame is one
/// nothing refers to. `BaseLayerFrameRateFraction` takes effect on a live session from its next
/// frame, and 1.0 turns layers off again (MEASUREMENTS.md, "temporal layers on the worker's
/// session").
const LAYERED_BASE_FRACTION: f64 = 0.5;
/// The share of the bitrate the base layer gets while layers are on. Of Apple's suggested 0.6 to
/// 0.8, 0.8 cost the fewest bytes for the same picture on scrolling text, and it leaves the
/// frames whose loss costs nothing the smaller share.
const LAYERED_BASE_BITS: f64 = 0.8;

/// Whether temporal layers were measured on the low-latency session for `codec` and `chroma`:
/// every other frame marked, none dropped, a live toggle taken both ways (MEASUREMENTS.md,
/// "temporal layers on each session"). HEVC 4:2:0 and 4:4:4 10-bit and H.264 4:2:0 were; a
/// pair not yet measured gets no layers.
const fn layers_measured(codec: VideoCodec, chroma: Chroma) -> bool {
    matches!(
        (codec, chroma),
        (VideoCodec::Hevc, Chroma::Subsampled | Chroma::Full)
            | (VideoCodec::H264, Chroma::Subsampled)
    )
}

/// The per-frame refcon of a frame submitted as an LTR refresh. VideoToolbox hands a frame's
/// `sourceFrameRefcon` back to the output callback with that frame and never dereferences it,
/// so the flag rides with its own frame whatever is in flight or dropped around it.
const REFRESH_REFCON: usize = 1;

/// The `sourceFrameRefcon` for a frame: [`REFRESH_REFCON`] for a refresh, null otherwise.
const fn frame_refcon(refresh: bool) -> *mut c_void {
    if refresh {
        ptr::without_provenance_mut(REFRESH_REFCON)
    } else {
        ptr::null_mut()
    }
}

/// Whether the frame the callback got was submitted as a refresh.
fn is_refresh(refcon: *mut c_void) -> bool {
    refcon.addr() == REFRESH_REFCON
}

/// What a frame is submitted as, `(keyframe, ltr_refresh)`, for `options` on a session that
/// does or does not have long-term references (`ltr`).
///
/// A refresh is what a receiver with a hole is waiting on, so it must come back as a picture
/// that stands on its own. With references the encoder makes it a delta off an acknowledged one
/// (or an IDR when none is). Without them `ForceLTRRefresh` means nothing and the frame would
/// be a plain P-frame off the picture the receiver lost; a keyframe is the refresh then. A
/// keyframe asked for outright answers any refresh with it, so it is not flagged as one.
const fn submission(options_keyframe: bool, options_refresh: bool, ltr: bool) -> (bool, bool) {
    let keyframe = options_keyframe || (options_refresh && !ltr);
    (keyframe, options_refresh && ltr && !keyframe)
}

/// Whether a frame that came back with `status` says the session is gone for good: every frame
/// after it fails too, and only a new session codes.
///
/// That is `kVTInvalidSessionErr` (`mediaserverd` restarting), `kVTSessionMalfunctionErr`, or
/// the hardware encoder's own malfunction. `kVTVideoEncoderNotAvailableNowErr` alone is not: a
/// busy encoder answers some frames with it and codes the next ([`BUSY_LOST`]).
const fn encoder_lost(status: i32) -> bool {
    status == kVTInvalidSessionErr
        || status == kVTSessionMalfunctionErr
        || status == kVTVideoEncoderMalfunctionErr
}

/// How long a session may answer every frame with `kVTVideoEncoderNotAvailableNowErr`, coding
/// none in between, before it is taken as gone.
///
/// Busy is not stopped. Thirty-two sessions alive at once got that status for some frames and
/// coded the next at once (`docs/decisions/video.md`, "A virtual Mac's encoder stops for good
/// past its 1020th client"), so a frame refused with it is skipped and the session kept. But on
/// a starved machine a session whose encoder malfunctioned answered every frame after it with
/// that for as long as it lived, while new sessions beside it coded (MEASUREMENTS.md, "an
/// encoder session that malfunctions"), and a stopped encoder answers so for good. A run as long
/// as the worker's first wait before rebuilding a lost session again tells the two apart: at
/// 60 fps it is fifteen refusals in a row.
const BUSY_LOST: Duration = Duration::from_millis(250);

struct Shared {
    sink: Sink,
    codec: VideoCodec,
    /// Frames the session gave up: dropped by rate control or failed.
    dropped: AtomicU64,
    /// The status of the first frame that came back from a session gone for good
    /// ([`encoder_lost`]); zero while it codes.
    lost: AtomicI32,
    /// When the session was made: the clock [`Self::busy_since_us`] counts on.
    born: Instant,
    /// Microseconds after [`Self::born`], plus one, of the first refusal in the run of
    /// `kVTVideoEncoderNotAvailableNowErr` under way; zero while none is ([`BUSY_LOST`]).
    busy_since_us: AtomicU64,
}

impl Shared {
    fn new(sink: Sink, codec: VideoCodec) -> Self {
        Self {
            sink,
            codec,
            dropped: AtomicU64::new(0),
            lost: AtomicI32::new(0),
            born: Instant::now(),
            busy_since_us: AtomicU64::new(0),
        }
    }

    /// A frame was refused with `status` at `now`, as it was submitted or when it came back.
    /// The session is gone when the status says so ([`encoder_lost`]), or when it is the busy
    /// encoder's and every frame has had it for [`BUSY_LOST`]; a lone busy refusal skips only
    /// its frame.
    fn refused(&self, status: i32, now: Instant) {
        if encoder_lost(status) {
            self.lose(status);
            return;
        }
        if status != kVTVideoEncoderNotAvailableNowErr {
            return;
        }
        let at = u64::try_from(now.saturating_duration_since(self.born).as_micros())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let since = self
            .busy_since_us
            .compare_exchange(0, at, Ordering::AcqRel, Ordering::Acquire)
            // The run starts here when none was under way, or went on from where it started.
            .map_or_else(|since| since, |_none| at);
        if Duration::from_micros(at.saturating_sub(since)) >= BUSY_LOST {
            self.lose(status);
        }
    }

    /// A frame came out: a run of busy refusals is over.
    fn coded(&self) {
        self.busy_since_us.store(0, Ordering::Release);
    }

    /// A frame came back with `status`, which says the session is gone: the first such status
    /// is the one kept.
    fn lose(&self, status: i32) {
        let _kept = self
            .lost
            .compare_exchange(0, status, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Whether the session is gone, as [`CodecError::EncoderLost`].
    fn lost(&self) -> Result<(), CodecError> {
        match self.lost.load(Ordering::Acquire) {
            0 => Ok(()),
            status => Err(CodecError::EncoderLost { status }),
        }
    }
}

/// A hardware encoder.
pub struct Encoder {
    session: CFRetained<VTCompressionSession>,
    config: EncoderConfig,
    rate_control: RateControl,
    ltr: bool,
    /// Whether the session has had its first frame, from when on its declared rate places
    /// nothing; true from the start for a session with no placement to make.
    placed: AtomicBool,
    /// The frame rate asked of an unplaced session ([`Self::set_frame_rate`]), told it once the
    /// first frame placed it; 0 when nothing was asked.
    asked_fps: AtomicU16,
    // Declared last so it outlives the session's `Drop` (the callback's refcon points at it).
    shared: Arc<Shared>,
}

// SAFETY: VideoToolbox sessions are documented as usable from any thread:
// `VTSessionSetProperty`, `VTCompressionSessionEncodeFrame` and `VTCompressionSessionInvalidate`
// are serialised inside the framework, and the output callback already arrives on a
// VideoToolbox thread. `Shared` is only read, through the `Send + Sync` sink.
unsafe impl Send for Encoder {}
// SAFETY: as above; every `&self` method is a thread-safe VideoToolbox call.
unsafe impl Sync for Encoder {}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encoder")
            .field("config", &self.config)
            .field("rate_control", &self.rate_control)
            .field("ltr", &self.ltr)
            .field("placed", &self.placed)
            .finish_non_exhaustive()
    }
}

impl Encoder {
    /// Create and configure a low-latency session carrying `config.chroma`. Packets are
    /// delivered to `sink` on VideoToolbox's thread. A [`Chroma::Full`] session is HEVC only,
    /// and every picture must then be in the format [`pixel_format`] names for it.
    pub fn new(
        config: EncoderConfig,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Self::with_rate_control(config, RateControl::LowLatency, Box::new(sink))
    }

    /// A session in another rate-control mode, for the measurement that compares them.
    #[cfg(feature = "experiments")]
    pub fn experiment(
        config: EncoderConfig,
        rate_control: RateControl,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Self::with_rate_control(config, rate_control, Box::new(sink))
    }

    fn with_rate_control(
        config: EncoderConfig,
        rate_control: RateControl,
        sink: Sink,
    ) -> Result<Self, CodecError> {
        crate::admit("VTCompressionSessionCreate")?;
        let chroma = config.chroma;
        if chroma == Chroma::Full && config.codec != VideoCodec::Hevc {
            return Err(CodecError::NoFullChroma(config.codec));
        }
        let shared = Arc::new(Shared::new(sink, config.codec));
        // SAFETY: framework-provided constant string.
        let hardware_key =
            unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder };
        let spec = match rate_control {
            RateControl::LowLatency => {
                // SAFETY: framework-provided constant string.
                let low_latency_key =
                    unsafe { kVTVideoEncoderSpecification_EnableLowLatencyRateControl };
                CFDictionary::<CFString, CFType>::from_slices(
                    &[low_latency_key, hardware_key],
                    &[cf::boolean(true), cf::boolean(true)],
                )
            }
            #[cfg(feature = "experiments")]
            RateControl::Vbv => {
                CFDictionary::<CFString, CFType>::from_slices(&[hardware_key], &[cf::boolean(true)])
            }
        };
        let codec_type = match config.codec {
            VideoCodec::Hevc => kCMVideoCodecType_HEVC,
            VideoCodec::H264 => kCMVideoCodecType_H264,
        };
        // A 4:4:4 session says what it is fed; a 4:2:0 one takes the capture's NV12 as it comes.
        let source_format = CFNumber::new_i64(i64::from(pixel_format(chroma)));
        let source = (chroma == Chroma::Full).then(|| {
            CFDictionary::<CFString, CFType>::from_slices(
                // SAFETY: framework-provided constant string.
                &[unsafe { kCVPixelBufferPixelFormatTypeKey }],
                &[&*source_format],
            )
        });
        let mut raw: *mut VTCompressionSession = ptr::null_mut();
        // SAFETY: all pointers are valid for the call; the refcon is the `Arc<Shared>` kept
        // alive by this `Encoder` until the session is invalidated (see `Drop`).
        let status = unsafe {
            VTCompressionSession::create(
                None,
                i32::try_from(config.width).unwrap_or(i32::MAX),
                i32::try_from(config.height).unwrap_or(i32::MAX),
                codec_type,
                Some(spec.as_opaque()),
                source.as_deref().map(CFDictionary::as_opaque),
                None,
                Some(output_callback),
                Arc::as_ptr(&shared).cast_mut().cast::<c_void>(),
                NonNull::from(&mut raw),
            )
        };
        check("VTCompressionSessionCreate", status)?;
        let Some(raw) = NonNull::new(raw) else {
            return Err(CodecError::Os {
                call: "VTCompressionSessionCreate",
                status: -1,
            });
        };
        // SAFETY: `create` returned a +1 reference.
        let session = unsafe { CFRetained::from_raw(raw) };
        let mut encoder = Self {
            session,
            config,
            rate_control,
            ltr: false,
            placed: AtomicBool::new(placement_fps(config.fps) == config.fps),
            asked_fps: AtomicU16::new(0),
            shared,
        };
        encoder.configure()?;
        // SAFETY: the session is fully configured; this only pre-allocates encoder resources.
        let status = unsafe { encoder.session.prepare_to_encode_frames() };
        check("VTCompressionSessionPrepareToEncodeFrames", status)?;
        // Again after the prepare: set only before it, the rate places nothing, and the session
        // lands wherever the driver likes ([`PLACEMENT_FPS`]).
        encoder.declare_frame_rate(placement_fps(encoder.config.fps))?;
        Ok(encoder)
    }

    /// The settings in force.
    #[must_use]
    pub const fn config(&self) -> &EncoderConfig {
        &self.config
    }

    /// The colour the stream carries.
    #[must_use]
    pub const fn chroma(&self) -> Chroma {
        self.config.chroma
    }

    /// True when the encoder accepted long-term references.
    #[must_use]
    pub const fn ltr_enabled(&self) -> bool {
        self.ltr
    }

    fn set(&self, key: &CFString, value: &CFType, call: &'static str) -> Result<(), CodecError> {
        cf::set_property(&self.session, key, value, call)
    }

    /// Set one public `kVTCompressionPropertyKey_*` on the live session; `call` names it in
    /// the error. The worker's property set lives in `configure`.
    #[cfg(feature = "experiments")]
    pub fn set_property(
        &self,
        key: &CFString,
        value: &CFType,
        call: &'static str,
    ) -> Result<(), CodecError> {
        self.set(key, value, call)
    }

    /// Set a property that some encoders do not implement; unsupported is logged, not fatal.
    fn set_optional(&self, key: &CFString, value: &CFType, call: &'static str) -> bool {
        match self.set(key, value, call) {
            Ok(()) => true,
            Err(CodecError::Os { status, .. }) if status == kVTPropertyNotSupportedErr => {
                tracing::debug!(
                    call,
                    "encoder does not support this property (expected on Apple silicon for AllowOpenGOP, MaxFrameDelayCount, PrioritizeEncodingSpeedOverQuality)"
                );
                false
            }
            Err(err) => {
                tracing::warn!(call, %err, "setting encoder property failed");
                false
            }
        }
    }

    fn configure(&mut self) -> Result<(), CodecError> {
        let fps = f64::from(self.config.fps);
        // `(key, value, name, required)`. Optional ones are absent on some encoders.
        let table: [(&CFString, CFRetained<CFType>, &'static str, bool); 9] = [
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_RealTime },
                retain(cf::boolean(true)),
                "RealTime",
                true,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_AllowFrameReordering },
                retain(cf::boolean(false)),
                "AllowFrameReordering",
                true,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_AllowOpenGOP },
                retain(cf::boolean(false)),
                "AllowOpenGOP",
                false,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_MaxFrameDelayCount },
                retain(&cf::int(0)),
                "MaxFrameDelayCount",
                false,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality },
                retain(cf::boolean(true)),
                "PrioritizeEncodingSpeedOverQuality",
                false,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_ExpectedFrameRate },
                retain(&cf::float(fps)),
                "ExpectedFrameRate",
                true,
            ),
            (
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_MaximumRealTimeFrameRate },
                retain(&cf::int(120)),
                "MaximumRealTimeFrameRate",
                false,
            ),
            (
                // Infinite GOP: keyframes only on request.
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval },
                retain(&cf::int(i64::from(i32::MAX))),
                "MaxKeyFrameInterval",
                true,
            ),
            (
                // The capture asks ScreenCaptureKit for BT.709 (`onicode_desktop_capture`), and
                // the stream says so in its VUI: the client's shader reads the matrix back off the
                // decoded buffer instead of assuming one.
                // SAFETY: framework-provided constant string.
                unsafe { kVTCompressionPropertyKey_YCbCrMatrix },
                // SAFETY: framework-provided constant string.
                retain(unsafe { kCVImageBufferYCbCrMatrix_ITU_R_709_2 }),
                "YCbCrMatrix",
                true,
            ),
        ];
        for (key, value, name, required) in &table {
            if *required {
                self.set(key, value, name)?;
            } else {
                self.set_optional(key, value, name);
            }
        }
        let profile: CFRetained<CFString> = match (self.config.codec, self.config.chroma) {
            (VideoCodec::Hevc, Chroma::Subsampled) => {
                // SAFETY: framework-provided constant string.
                unsafe { kVTProfileLevel_HEVC_Main_AutoLevel }.retain()
            }
            (VideoCodec::H264, Chroma::Subsampled) => {
                // SAFETY: framework-provided constant string.
                unsafe { kVTProfileLevel_H264_High_AutoLevel }.retain()
            }
            (VideoCodec::Hevc, Chroma::Full) => self
                .advertised_profile(MAIN_444_10)
                .ok_or(CodecError::NoFullChroma(VideoCodec::Hevc))?,
            (VideoCodec::H264, Chroma::Full) => {
                return Err(CodecError::NoFullChroma(VideoCodec::H264));
            }
        };
        // SAFETY: framework-provided constant string.
        let profile_key = unsafe { kVTCompressionPropertyKey_ProfileLevel };
        self.set(profile_key, &profile, "ProfileLevel")?;
        self.set_bitrate(self.config.bitrate_bps)?;
        // SAFETY: framework-provided constant string.
        let ltr_key = unsafe { kVTCompressionPropertyKey_EnableLTR };
        self.ltr = self.set_optional(ltr_key, cf::boolean(true), "EnableLTR");
        // The encoder's own error per frame, which tells a still picture's refinement when to
        // stop (`docs/decisions/video.md`, "A still picture is refined until the encoder stops
        // gaining on it").
        // SAFETY: framework-provided constant string.
        let mse_key = unsafe { kVTCompressionPropertyKey_CalculateMeanSquaredError };
        self.set_optional(mse_key, cf::boolean(true), "CalculateMeanSquaredError");
        Ok(())
    }

    /// Temporal layers on or off from the next frame ([`VideoEncoder::set_temporal_layers`]);
    /// whether the session writes them now.
    ///
    /// Only on a session whose layers were measured: HEVC 4:2:0, HEVC 4:4:4 10-bit and H.264
    /// 4:2:0. Off puts both fractions back to 1.0, and so does an on that the session refused
    /// half of, so a session is either wholly layered or as it was opened.
    #[must_use]
    pub fn set_temporal_layers(&self, on: bool) -> bool {
        // SAFETY: framework-provided constant strings.
        let (fraction_key, bits_key) = unsafe {
            (
                kVTCompressionPropertyKey_BaseLayerFrameRateFraction,
                kVTCompressionPropertyKey_BaseLayerBitRateFraction,
            )
        };
        let (fraction_name, bits_name) = ("BaseLayerFrameRateFraction", "BaseLayerBitRateFraction");
        let layered = on
            && layers_measured(self.config.codec, self.config.chroma)
            && self.set_optional(bits_key, &cf::float(LAYERED_BASE_BITS), bits_name)
            && self.set_optional(
                fraction_key,
                &cf::float(LAYERED_BASE_FRACTION),
                fraction_name,
            );
        if !layered {
            self.set_optional(fraction_key, &cf::float(1.0), fraction_name);
            self.set_optional(bits_key, &cf::float(1.0), bits_name);
        }
        layered
    }

    /// Frames the session has given up since it opened.
    #[must_use]
    pub fn frames_dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// The session's own `ProfileLevel` value named `name`, from the supported-value list it
    /// advertises (`VTSessionCopySupportedPropertyDictionary`).
    fn advertised_profile(&self, name: &str) -> Option<CFRetained<CFString>> {
        // SAFETY: framework-provided constant strings.
        let (profile_key, list_key) = unsafe {
            (
                kVTCompressionPropertyKey_ProfileLevel,
                kVTPropertySupportedValueListKey,
            )
        };
        let entry = self.supported_dictionary()?.get(profile_key)?;
        let entry = entry.downcast::<CFDictionary>().ok()?;
        // SAFETY: VTSession.h: each property's entry is a dictionary keyed by CFString.
        let entry: CFRetained<CFDictionary<CFString, CFType>> =
            unsafe { CFRetained::cast_unchecked(entry) };
        let list = entry.get(list_key)?.downcast::<CFArray>().ok()?;
        // SAFETY: the supported-value list is a CFArray of property values; each is only read.
        let list: CFRetained<CFArray<CFType>> = unsafe { CFRetained::cast_unchecked(list) };
        list.iter()
            .filter_map(|value| value.downcast::<CFString>().ok())
            .find(|value| value.to_string() == name)
    }

    /// The session's `VTSessionCopySupportedPropertyDictionary`, keyed by property name.
    fn supported_dictionary(&self) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
        let session: NonNull<CFType> = NonNull::from(self.session.as_ref());
        // SAFETY: VTSession.h: a `VTCompressionSessionRef` is a `VTSessionRef`; read only.
        let session: &VTSession = unsafe { session.cast::<VTSession>().as_ref() };
        let mut raw: *const CFDictionary = ptr::null();
        // SAFETY: valid session and out pointer; the dictionary comes back +1 (Copy rule).
        let status =
            unsafe { VTSessionCopySupportedPropertyDictionary(session, NonNull::from(&mut raw)) };
        let raw = NonNull::new(raw.cast_mut()).filter(|_| status == 0)?;
        // SAFETY: +1 reference from the copy call, keyed by `CFString`s (VTSession.h).
        Some(unsafe { CFRetained::from_raw(raw.cast()) })
    }

    /// Tell the session how many frames a second it is now being given.
    ///
    /// `AverageBitRate` is spent over a second however many frames arrive, so the cadence change
    /// alone already gives the surviving frames the skipped ones' bytes. `ExpectedFrameRate` is
    /// the hint the rate controller sizes its first frames from; it would still describe the
    /// old cadence otherwise.
    ///
    /// A session made for 60 frames a second or more declares 120 until its first frame, which
    /// gives it an encode engine of its own; a rate asked for before then is told after that
    /// frame.
    pub fn set_frame_rate(&self, fps: u16) -> Result<(), CodecError> {
        // Stored, then `placed` read again, against `place` swapping `placed` and then reading
        // the rate, all sequentially consistent: at least one of the two sees the other, so a
        // rate asked while the first frame goes in is told either way.
        if !self.placed.load(Ordering::SeqCst) {
            self.asked_fps.store(fps.max(1), Ordering::SeqCst);
            if !self.placed.load(Ordering::SeqCst) {
                return Ok(());
            }
        }
        self.declare_frame_rate(fps)
    }

    /// Set `ExpectedFrameRate`, and on a VBV session the window it sizes, to `fps`.
    fn declare_frame_rate(&self, fps: u16) -> Result<(), CodecError> {
        let fps = f64::from(fps.max(1));
        // SAFETY: framework-provided constant string.
        let expected_key = unsafe { kVTCompressionPropertyKey_ExpectedFrameRate };
        self.set(expected_key, &cf::float(fps), "ExpectedFrameRate")?;
        #[cfg(feature = "experiments")]
        if self.rate_control == RateControl::Vbv {
            // SAFETY: framework-provided constant string.
            let duration_key =
                unsafe { objc2_video_toolbox::kVTCompressionPropertyKey_VBVBufferDuration };
            self.set(duration_key, &cf::float(2.0 / fps), "VBVBufferDuration")?;
        }
        Ok(())
    }

    /// Change the target bitrate on the fly (bits per second).
    pub fn set_bitrate(&self, bps: u32) -> Result<(), CodecError> {
        match self.rate_control {
            RateControl::LowLatency => {
                // SAFETY: framework-provided constant string.
                let average_key = unsafe { kVTCompressionPropertyKey_AverageBitRate };
                self.set(average_key, &cf::int(i64::from(bps)), "AverageBitRate")?;
                // Hard cap per second: 1.25× the average, in bytes.
                let bytes_per_second = i64::from(bps).saturating_mul(5) / 32;
                let limits = CFArray::<CFNumber>::from_retained_objects(&[
                    cf::int(bytes_per_second),
                    cf::float(1.0),
                ]);
                // SAFETY: framework-provided constant string.
                let limits_key = unsafe { kVTCompressionPropertyKey_DataRateLimits };
                self.set_optional(limits_key, limits.as_opaque(), "DataRateLimits");
            }
            #[cfg(feature = "experiments")]
            RateControl::Vbv => experiments::set_vbv_bitrate(self, bps)?,
        }
        Ok(())
    }

    /// Submit one picture. `pts_us` is echoed on the packet; use the capture timestamp.
    ///
    /// [`CodecError::EncoderLost`] once a frame came back from a session the system took away,
    /// this one included when its callback ran inside the submit: the session codes nothing
    /// more, and the caller replaces it.
    pub fn encode(
        &self,
        image: &CVPixelBuffer,
        pts_us: u64,
        options: &FrameOptions,
    ) -> Result<(), CodecError> {
        self.shared.lost()?;
        let format = CVPixelBufferGetPixelFormatType(image);
        if self.config.chroma == Chroma::Full && format != pixel_format(Chroma::Full) {
            return Err(CodecError::NotFullChroma(format));
        }
        let size = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        let session = (
            usize::try_from(self.config.width).unwrap_or(usize::MAX),
            usize::try_from(self.config.height).unwrap_or(usize::MAX),
        );
        if size != session {
            return Err(CodecError::WrongSize {
                image: size,
                session,
            });
        }
        let mut keys: Vec<&CFString> = Vec::new();
        let mut values: Vec<&CFType> = Vec::new();
        let tokens: CFRetained<CFArray<CFNumber>>;
        let has_acks = self.ltr && !options.acked_ltr.is_empty();
        let (keyframe, refresh) =
            submission(options.force_keyframe, options.force_ltr_refresh, self.ltr);
        if keyframe {
            // SAFETY: framework-provided constant string.
            let key = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            keys.push(key);
            values.push(CFBoolean::new(true));
        }
        if refresh {
            // SAFETY: framework-provided constant string.
            let key = unsafe { kVTEncodeFrameOptionKey_ForceLTRRefresh };
            keys.push(key);
            values.push(CFBoolean::new(true));
        }
        if has_acks {
            let numbers: Vec<CFRetained<CFNumber>> = options
                .acked_ltr
                .iter()
                .map(|&t| cf::int(i64::try_from(t).unwrap_or(i64::MAX)))
                .collect();
            tokens = CFArray::from_retained_objects(&numbers);
            // SAFETY: framework-provided constant string.
            let key = unsafe { kVTEncodeFrameOptionKey_AcknowledgedLTRTokens };
            keys.push(key);
            values.push(&tokens);
        }
        let properties = (!keys.is_empty())
            .then(|| CFDictionary::<CFString, CFType>::from_slices(&keys, &values));
        let mut flags = VTEncodeInfoFlags::empty();
        // SAFETY: the image buffer is valid and retained by the caller for the call; the
        // session retains it as long as the encoder needs it. The source refcon is a tag the
        // framework only hands back to the callback (`frame_refcon`), never a pointer it reads.
        let status = unsafe {
            self.session.encode_frame(
                image,
                cf::time_us(pts_us),
                kCMTimeInvalid,
                properties.as_deref().map(CFDictionary::as_opaque),
                frame_refcon(refresh),
                &raw mut flags,
            )
        };
        self.shared.refused(status, Instant::now());
        self.shared.lost()?;
        check("VTCompressionSessionEncodeFrame", status)?;
        self.place();
        Ok(())
    }

    /// After the first frame, which placed the session: tell it the rate it now runs at, the one
    /// last asked of it or its own.
    fn place(&self) {
        if self.placed.swap(true, Ordering::SeqCst) {
            return;
        }
        let fps = match self.asked_fps.load(Ordering::SeqCst) {
            0 => self.config.fps,
            asked => asked,
        };
        if let Err(e) = self.declare_frame_rate(fps) {
            tracing::warn!(fps, error = %e, "the frame rate after placement");
        }
    }

    /// Wait for every submitted frame to come out of the sink.
    pub fn flush(&self) -> Result<(), CodecError> {
        // SAFETY: an invalid time means "everything submitted so far".
        let status = unsafe { self.session.complete_frames(kCMTimeInvalid) };
        check("VTCompressionSessionCompleteFrames", status)
    }
}

/// [`Encoder`] as the worker's [`VideoEncoder`]: it takes the frames ScreenCaptureKit hands
/// over, which are `IOSurface`-backed pixel buffers.
#[derive(Debug)]
pub struct VideoToolbox {
    encoder: Encoder,
    /// For a stripe's session, its rows of each capture, copied into a picture of its own on
    /// the submitting thread, so two stripes submitted at once copy at once too.
    stripe: Option<StripeCopy>,
}

impl VideoEncoder for VideoToolbox {
    type Image = PixelBuffer;

    fn new(
        config: EncoderConfig,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Ok(Self {
            encoder: Encoder::new(config, sink)?,
            stripe: None,
        })
    }

    fn stripe(
        config: EncoderConfig,
        stripe: Stripe,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        let side = |n: u32| usize::try_from(n).unwrap_or(usize::MAX);
        let copy = StripeCopy::new(
            (side(config.width), side(config.height)),
            stripe,
            config.chroma,
        )?;
        let config = EncoderConfig {
            height: stripe.coded_rows,
            ..config
        };
        Ok(Self {
            encoder: Encoder::new(config, sink)?,
            stripe: Some(copy),
        })
    }

    fn side_by_side(
        width: u32,
        height: u32,
        chroma: Chroma,
    ) -> Result<Option<crate::stripes::SideBySide>, CodecError> {
        crate::stripes::side_by_side(width, height, chroma)
    }

    fn encode(
        &self,
        image: &PixelBuffer,
        pts_us: u64,
        options: &FrameOptions,
    ) -> Result<(), CodecError> {
        match &self.stripe {
            None => self.encoder.encode(image.as_cv(), pts_us, options),
            Some(copy) => self
                .encoder
                .encode(&*copy.copy(image.as_cv())?, pts_us, options),
        }
    }

    fn set_bitrate(&self, bps: u32) -> Result<(), CodecError> {
        self.encoder.set_bitrate(bps)
    }

    fn set_frame_rate(&self, fps: u16) -> Result<(), CodecError> {
        self.encoder.set_frame_rate(fps)
    }

    fn set_temporal_layers(&self, on: bool) -> Result<bool, CodecError> {
        Ok(self.encoder.set_temporal_layers(on))
    }

    fn frames_dropped(&self) -> u64 {
        self.encoder.frames_dropped()
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: invalidation stops callbacks; afterwards the session never touches the
        // refcon again, so the `Arc<Shared>` may be released with `self`.
        unsafe { self.session.invalidate() }
    }
}

unsafe extern "C-unwind" fn output_callback(
    refcon: *mut c_void,
    source: *mut c_void,
    status: i32,
    flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    let refresh = is_refresh(source);
    // SAFETY: the refcon was created from `Arc::as_ptr` on the encoder's `Shared`, which the
    // `Encoder` keeps alive until the session is invalidated (see `Drop`).
    let shared: &Shared = unsafe { &*refcon.cast::<Shared>() };
    if status != 0 || flags.contains(VTEncodeInfoFlags::FrameDropped) {
        // A refresh lost here is asked for again: the receiver repeats its request until a
        // picture it can decode arrives.
        tracing::debug!(status, ?flags, refresh, "encoder dropped a frame");
        shared.dropped.fetch_add(1, Ordering::Relaxed);
        shared.refused(status, Instant::now());
        return;
    }
    let Some(sample) = NonNull::new(sample) else {
        return;
    };
    shared.coded();
    // SAFETY: the sample buffer is valid for the duration of the callback.
    let sample: &CMSampleBuffer = unsafe { sample.as_ref() };
    match packet(sample, shared.codec, refresh) {
        Ok(packet) => (shared.sink)(packet),
        Err(e) => tracing::warn!(error = %e, "encoder output is not an access unit; dropped"),
    }
}

/// VideoToolbox's output as a packet: the parameter sets in front of a keyframe, each behind its
/// length, then the sample's length-prefixed units copied once, as they are.
fn packet(
    sample: &CMSampleBuffer,
    codec: VideoCodec,
    refresh: bool,
) -> Result<EncodedPacket, CodecError> {
    // SAFETY: valid sample buffer.
    let block = unsafe { sample.data_buffer() }.ok_or(CodecError::Os {
        call: "CMSampleBufferGetDataBuffer",
        status: -1,
    })?;
    let marks = attachments(sample);
    let keyframe = marks.keyframe;
    // SAFETY: valid sample buffer.
    let pts_us = cf::micros(unsafe { sample.presentation_time_stamp() }).unwrap_or(0);
    let format = if keyframe {
        // SAFETY: valid sample buffer.
        unsafe { sample.format_description() }
    } else {
        None
    };
    let sets = match &format {
        Some(format) => parameter_sets(format, codec)?,
        None => Vec::new(),
    };
    // SAFETY: valid block buffer.
    let body = unsafe { block.data_length() };
    let head = sets.iter().fold(0_usize, |sum, set| {
        sum.saturating_add(4).saturating_add(set.len())
    });
    let mut data = Vec::with_capacity(head.saturating_add(body));
    for set in &sets {
        nal::push(&mut data, set)?;
    }
    if body > 0
        && let Some(spare) = NonNull::new(data.spare_capacity_mut().as_mut_ptr())
    {
        // SAFETY: CoreMedia rule: `CMBlockBufferCopyDataBytes` writes exactly `body` bytes to the
        // destination, which is the vector's spare capacity (at least `body` bytes, reserved
        // above).
        let status = unsafe { block.copy_data_bytes(0, body, spare.cast::<c_void>()) };
        check("CMBlockBufferCopyDataBytes", status)?;
        // SAFETY: the copy succeeded, so the `body` bytes past `head` are initialised.
        unsafe {
            data.set_len(head.saturating_add(body));
        }
    }
    nal::check(data.get(head..).unwrap_or_default())?;
    Ok(EncodedPacket {
        data,
        keyframe,
        ltr_token: marks.ltr_token,
        ltr_refresh: refresh,
        // A refresh is what a receiver with a hole waits on; the frames after one decode from it
        // whatever layer it took (MEASUREMENTS.md, "temporal layers on the worker's session").
        discardable: marks.discardable && !keyframe && !refresh,
        mse: marks.mse,
        pts_us,
    })
}

/// What an encoded sample says of itself.
#[derive(Clone, Copy, Debug)]
struct Marks {
    keyframe: bool,
    ltr_token: Option<u64>,
    /// `IsDependedOnByOthers` is present and false: CoreMedia's own definition of a frame that
    /// may be dropped (`CMSampleBuffer.h`).
    discardable: bool,
    mse: Option<Mse>,
}

/// [`Marks`] from the sample's first attachment dictionary.
fn attachments(sample: &CMSampleBuffer) -> Marks {
    let unmarked = Marks {
        keyframe: true,
        ltr_token: None,
        discardable: false,
        mse: None,
    };
    // SAFETY: valid sample buffer; `false` never allocates.
    let Some(array) = (unsafe { sample.sample_attachments_array(false) }) else {
        return unmarked;
    };
    // SAFETY: CoreMedia documents the array's elements as CFDictionaries keyed by CFString.
    let array: CFRetained<CFArray<CFDictionary<CFString, CFType>>> =
        unsafe { CFRetained::cast_unchecked(array) };
    let Some(dict) = array.get(0) else {
        return unmarked;
    };
    // SAFETY: framework-provided constant string.
    let not_sync_key = unsafe { kCMSampleAttachmentKey_NotSync };
    let not_sync = dict
        .get(not_sync_key)
        .and_then(|v| v.downcast::<CFBoolean>().ok())
        .is_some_and(|b| b.as_bool());
    // SAFETY: framework-provided constant string.
    let token_key = unsafe { kVTSampleAttachmentKey_RequireLTRAcknowledgementToken };
    let token = dict
        .get(token_key)
        .and_then(|v| v.downcast::<CFNumber>().ok())
        .and_then(|n| n.as_i64())
        .and_then(|n| u64::try_from(n).ok());
    // SAFETY: framework-provided constant string.
    let depended_key = unsafe { kCMSampleAttachmentKey_IsDependedOnByOthers };
    let depended = dict
        .get(depended_key)
        .and_then(|v| v.downcast::<CFBoolean>().ok())
        .map(|b| b.as_bool());
    Marks {
        keyframe: !not_sync,
        ltr_token: token,
        discardable: depended == Some(false),
        mse: mse(&dict),
    }
}

/// The encoder's per-plane error from a sample's `QualityMetrics` attachment, when the session
/// measures it.
fn mse(dict: &CFDictionary<CFString, CFType>) -> Option<Mse> {
    // SAFETY: framework-provided constant strings.
    let (metrics_key, luma_key, blue_key, red_key) = unsafe {
        (
            kVTSampleAttachmentKey_QualityMetrics,
            kVTSampleAttachmentQualityMetricsKey_LumaMeanSquaredError,
            kVTSampleAttachmentQualityMetricsKey_ChromaBlueMeanSquaredError,
            kVTSampleAttachmentQualityMetricsKey_ChromaRedMeanSquaredError,
        )
    };
    let metrics = dict.get(metrics_key)?.downcast::<CFDictionary>().ok()?;
    // SAFETY: VTCompressionProperties.h: the metrics are a dictionary keyed by CFString whose
    // values, for a single-view stream, are CFNumbers; they are only read.
    let metrics: CFRetained<CFDictionary<CFString, CFType>> =
        unsafe { CFRetained::cast_unchecked(metrics) };
    let number = |key: &CFString| {
        metrics
            .get(key)
            .and_then(|v| v.downcast::<CFNumber>().ok())
            .and_then(|n| n.as_f64())
    };
    let luma = number(luma_key)?;
    let chroma = number(blue_key)
        .zip(number(red_key))
        .map(|(blue, red)| blue.midpoint(red));
    Some(Mse { luma, chroma })
}

/// The parameter sets of a format description, borrowed from it. The stream's NAL lengths must
/// be four bytes, the wire's ([`nal`]), since its units travel as written; VideoToolbox's are.
fn parameter_sets(
    format: &CMFormatDescription,
    codec: VideoCodec,
) -> Result<Vec<&[u8]>, CodecError> {
    let mut sets = Vec::new();
    let mut count: usize = 0;
    let mut nal_length: std::ffi::c_int = 4;
    let mut index = 0_usize;
    loop {
        let mut ptr: *const u8 = ptr::null();
        let mut size: usize = 0;
        let status = match codec {
            // SAFETY: every out-pointer is valid; the parameter set bytes are owned by the
            // format description, which outlives the returned slices.
            VideoCodec::Hevc => unsafe {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format,
                    index,
                    &raw mut ptr,
                    &raw mut size,
                    &raw mut count,
                    &raw mut nal_length,
                )
            },
            // SAFETY: as above.
            VideoCodec::H264 => unsafe {
                CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    format,
                    index,
                    &raw mut ptr,
                    &raw mut size,
                    &raw mut count,
                    &raw mut nal_length,
                )
            },
        };
        if status != 0 || ptr.is_null() || size == 0 {
            break;
        }
        // SAFETY: CoreMedia returned `size` readable bytes at `ptr`, owned by `format`.
        sets.push(unsafe { std::slice::from_raw_parts(ptr, size) });
        index = index.saturating_add(1);
        if index >= count {
            break;
        }
    }
    if nal_length != 4 {
        return Err(CodecError::MalformedNal { offset: 0 });
    }
    Ok(sets)
}

/// The rate-control comparison and the property probes behind the encoder's DECISIONS entries.
#[cfg(feature = "experiments")]
mod experiments {
    use objc2_core_foundation::{CFNumber, CFRetained, CFString};
    use objc2_video_toolbox::{
        kVTCompressionPropertyKey_MaxAllowedFrameQP, kVTCompressionPropertyKey_MinAllowedFrameQP,
        kVTCompressionPropertyKey_VBVBufferDuration, kVTCompressionPropertyKey_VBVMaxBitRate,
        kVTCompressionPropertyKey_VariableBitRate,
    };

    use super::Encoder;
    use crate::{CodecError, cf};

    pub(super) fn set_vbv_bitrate(encoder: &Encoder, bps: u32) -> Result<(), CodecError> {
        // SAFETY: framework-provided constant string.
        let vbr_key = unsafe { kVTCompressionPropertyKey_VariableBitRate };
        encoder.set(vbr_key, &cf::int(i64::from(bps)), "VariableBitRate")?;
        // The same 1.25× peak as the low-latency mode's data-rate limit.
        let peak = i64::from(bps).saturating_mul(5) / 4;
        // SAFETY: framework-provided constant string.
        let peak_key = unsafe { kVTCompressionPropertyKey_VBVMaxBitRate };
        encoder.set(peak_key, &cf::int(peak), "VBVMaxBitRate")?;
        // Two frames of buffer: the smallest model that still lets a frame differ from the
        // average (the default is 2.5 s, a delay this pipeline cannot pay).
        let duration = 2.0 / f64::from(encoder.config.fps.max(1));
        // SAFETY: framework-provided constant string.
        let duration_key = unsafe { kVTCompressionPropertyKey_VBVBufferDuration };
        encoder.set(duration_key, &cf::float(duration), "VBVBufferDuration")
    }

    impl Encoder {
        /// Try the optional quality keys on this session and report each `OSStatus` (0 =
        /// accepted, `kVTPropertyNotSupportedErr` = this encoder has no such knob). Leaves the
        /// accepted ones set, so call it on a throwaway session.
        #[must_use]
        pub fn probe_quality_keys(&self) -> Vec<(&'static str, i32)> {
            let keys: [(&CFString, CFRetained<CFNumber>, &'static str); 2] = [
                (
                    // SAFETY: framework-provided constant string.
                    unsafe { kVTCompressionPropertyKey_MaxAllowedFrameQP },
                    cf::int(45),
                    "MaxAllowedFrameQP",
                ),
                (
                    // SAFETY: framework-provided constant string.
                    unsafe { kVTCompressionPropertyKey_MinAllowedFrameQP },
                    cf::int(10),
                    "MinAllowedFrameQP",
                ),
            ];
            keys.iter()
                .map(|(key, value, name)| {
                    let status = match self.set(key, value, name) {
                        Ok(()) => 0,
                        Err(CodecError::Os { status, .. }) => status,
                        Err(_other) => -1,
                    };
                    (*name, status)
                })
                .collect()
        }

        /// Property keys the session says it supports
        /// (`VTSessionCopySupportedPropertyDictionary`).
        #[must_use]
        pub fn supported_properties(&self) -> Vec<String> {
            let Some(dict) = self.supported_dictionary() else {
                tracing::debug!("no supported-property dictionary");
                return Vec::new();
            };
            let (keys, _values) = dict.to_vecs();
            let mut names: Vec<String> = keys.iter().map(ToString::to_string).collect();
            names.sort_unstable();
            names
        }
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::arithmetic_side_effects,
        reason = "test fixture arithmetic on small, bounded values"
    )]

    use objc2_core_media::{CMBlockBuffer, CMSampleTimingInfo, kCMBlockBufferAssureMemoryNowFlag};

    use super::*;

    /// A sample buffer holding `body` (length-prefixed NAL units) the way the encoder hands one
    /// over, without a format description or attachments.
    fn sample_of(body: &[u8]) -> CFRetained<CMSampleBuffer> {
        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        // SAFETY: CoreMedia rule: a NULL memory block makes CoreMedia allocate `len` bytes.
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                body.len(),
                None,
                ptr::null(),
                0,
                body.len(),
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            )
        };
        assert_eq!(status, 0);
        // SAFETY: +1 reference from the create call.
        let block = unsafe { CFRetained::from_raw(NonNull::new(block).unwrap()) };
        // SAFETY: the source holds `body.len()` bytes, the block exactly that capacity.
        let status = unsafe {
            CMBlockBuffer::replace_data_bytes(
                NonNull::from(&body[0]).cast::<c_void>(),
                &block,
                0,
                body.len(),
            )
        };
        assert_eq!(status, 0);
        let timing = CMSampleTimingInfo {
            duration: cf::time_us(0),
            presentationTimeStamp: cf::time_us(1),
            decodeTimeStamp: cf::time_us(1),
        };
        let size = body.len();
        let mut sample: *mut CMSampleBuffer = ptr::null_mut();
        // SAFETY: one sample, one timing entry, one size entry; every pointer is valid.
        let status = unsafe {
            CMSampleBuffer::create_ready(
                None,
                Some(&block),
                None,
                1,
                1,
                &raw const timing,
                1,
                &raw const size,
                NonNull::from(&mut sample),
            )
        };
        assert_eq!(status, 0);
        // SAFETY: +1 reference from the create call.
        unsafe { CFRetained::from_raw(NonNull::new(sample).unwrap()) }
    }

    /// Length-prefixed NAL units of `sizes` bytes each.
    fn access_unit(sizes: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        for (n, &size) in sizes.iter().enumerate() {
            out.extend_from_slice(&u32::try_from(size).unwrap().to_be_bytes());
            out.extend((0..size).map(|i| u8::try_from(i.wrapping_add(n) % 251).unwrap()));
        }
        out
    }

    const W: usize = 320;
    const H: usize = 180;

    /// A full-range NV12 frame, the format the worker captures, with a moving gradient.
    fn frame(index: usize) -> CFRetained<CVPixelBuffer> {
        use objc2_core_video::{
            CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
            CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress,
            CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        };
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo rule: a valid out-pointer and no attributes.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                W,
                H,
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0);
        // SAFETY: +1 reference from the create call.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };
        // SAFETY: CoreVideo rule: lock before touching the planes.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(locked, 0);
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            let rows = if plane == 0 { H } else { H / 2 };
            for y in 0..rows {
                // SAFETY: the plane is locked, so row `y < rows` starts inside its mapping.
                let start = unsafe { base.add(y * stride) };
                // SAFETY: the row spans `stride >= W` writable bytes of the locked plane.
                let row = unsafe { std::slice::from_raw_parts_mut(start, W) };
                for (x, cell) in row.iter_mut().enumerate() {
                    *cell = if plane == 0 {
                        u8::try_from((x + y + index * 7) % 256).unwrap()
                    } else {
                        128
                    };
                }
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0);
        buffer
    }

    fn encoder(tx: std::sync::mpsc::Sender<EncodedPacket>) -> Encoder {
        let config = EncoderConfig {
            width: u32::try_from(W).unwrap(),
            height: u32::try_from(H).unwrap(),
            codec: VideoCodec::Hevc,
            fps: 60,
            bitrate_bps: 2_000_000,
            chroma: Chroma::Subsampled,
        };
        Encoder::new(config, move |packet| {
            let _receiver_gone = tx.send(packet);
        })
        .unwrap()
    }

    fn collect(rx: &std::sync::mpsc::Receiver<EncodedPacket>, n: usize) -> Vec<EncodedPacket> {
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut out = Vec::new();
        while out.len() < n {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(p) => out.push(p),
                Err(_) => break,
            }
        }
        out
    }

    /// One keyframe goes through a real session and comes out: the gate's probe of whether a
    /// runner's encoder answers at all (`xtask/src/gate.rs`, `videotoolbox_step`), so it stays
    /// the smallest coding there is.
    #[test]
    fn the_encoder_codes_one_frame() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        let keyframe = FrameOptions {
            force_keyframe: true,
            ..FrameOptions::default()
        };
        encoder.encode(&frame(0), 0, &keyframe).unwrap();
        encoder.flush().unwrap();
        let out = collect(&rx, 1);
        assert_eq!(
            out.iter()
                .map(|p| (p.pts_us, p.keyframe))
                .collect::<Vec<_>>(),
            [(0, true)]
        );
    }

    /// A refresh is always a picture that stands on its own: a delta off a reference where the
    /// session has them, a keyframe where it has none, and a keyframe asked for outright is not
    /// also flagged as a refresh.
    #[test]
    fn a_refresh_without_long_term_references_is_a_keyframe() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        assert_eq!(
            submission(false, true, true),
            (false, true),
            "a delta off a reference"
        );
        assert_eq!(
            submission(false, true, false),
            (true, false),
            "no references: a keyframe"
        );
        assert_eq!(
            submission(true, true, true),
            (true, false),
            "the keyframe answers it"
        );
        assert_eq!(submission(true, false, false), (true, false));
        assert_eq!(
            submission(false, false, true),
            (false, false),
            "a plain frame"
        );
    }

    /// The refresh flag comes back on the frame that asked for it and on no other, however
    /// many frames are in flight around it.
    #[test]
    fn the_refresh_flag_rides_with_its_own_frame() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        assert!(is_refresh(frame_refcon(true)));
        assert!(!is_refresh(frame_refcon(false)));
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        if !encoder.ltr_enabled() {
            onicode_desktop_testkit::live::skip("no LTR on this encoder; nothing to check");
            return;
        }
        let frames = 12_usize;
        for i in 0..frames {
            let options = FrameOptions {
                force_keyframe: i == 0,
                force_ltr_refresh: i == 7,
                acked_ltr: Vec::new(),
            };
            encoder
                .encode(&frame(i), u64::try_from(i).unwrap() * 16_667, &options)
                .unwrap();
        }
        encoder.flush().unwrap();
        let packets = collect(&rx, frames);
        assert_eq!(packets.len(), frames);
        let flagged: Vec<u64> = packets
            .iter()
            .filter(|p| p.ltr_refresh)
            .map(|p| p.pts_us)
            .collect();
        assert_eq!(
            flagged,
            vec![7 * 16_667],
            "only the frame that asked is a refresh"
        );
    }

    /// A number property of the live session, as it reads back.
    fn read_number(encoder: &Encoder, key: &CFString) -> Option<f64> {
        use objc2_video_toolbox::{VTSession, VTSessionCopyProperty};
        let ptr: NonNull<CFType> = NonNull::from(&**encoder.session);
        // SAFETY: VTSession.h: a compression session is a `VTSessionRef`; only read here.
        let session: &VTSession = unsafe { ptr.cast::<VTSession>().as_ref() };
        let mut out: *const CFType = ptr::null();
        // SAFETY: VTSession.h: the value comes back +1 through a `CFTypeRef *` out pointer.
        let status =
            unsafe { VTSessionCopyProperty(session, key, None, (&raw mut out).cast::<c_void>()) };
        let raw = NonNull::new(out.cast_mut()).filter(|_| status == 0)?;
        // SAFETY: +1 reference from the copy call (Copy rule).
        let value = unsafe { CFRetained::from_raw(raw) };
        value.downcast::<CFNumber>().ok()?.as_f64()
    }

    /// Frames `0..frames` through `encoder`, a keyframe first and a refresh at 7, layers
    /// switched off before `off`; how each came back, `L` for one nothing refers to.
    fn layered_marks(
        encoder: &Encoder,
        rx: &std::sync::mpsc::Receiver<EncodedPacket>,
        image: impl Fn(usize) -> CFRetained<CVPixelBuffer>,
        (frames, off): (usize, usize),
    ) -> Vec<EncodedPacket> {
        assert!(
            encoder.set_temporal_layers(true),
            "a measured session has layers"
        );
        for i in 0..frames {
            if i == off {
                assert!(!encoder.set_temporal_layers(false));
            }
            let options = FrameOptions {
                force_keyframe: i == 0,
                force_ltr_refresh: i == 7,
                acked_ltr: Vec::new(),
            };
            encoder
                .encode(&image(i), u64::try_from(i).unwrap() * 16_667, &options)
                .unwrap();
            encoder.flush().unwrap();
        }
        let packets = collect(rx, frames);
        assert_eq!(packets.len(), frames, "no frame dropped");
        let marks: String = packets
            .iter()
            .map(|p| if p.discardable { 'L' } else { 'B' })
            .collect();
        assert!(
            !packets[0].discardable && !packets[7].discardable,
            "{marks}"
        );
        let layered = &packets[1..off];
        let skippable = layered.iter().filter(|p| p.discardable).count();
        assert!(skippable >= 6, "about every other frame while on: {marks}");
        assert!(
            layered
                .windows(2)
                .all(|w| !(w[0].discardable && w[1].discardable)),
            "never two in a row: {marks}"
        );
        assert!(
            packets[off + 1..].iter().all(|p| !p.discardable),
            "off again: {marks}"
        );
        packets
    }

    /// With temporal layers on, every other frame comes back as one nothing refers to, never a
    /// keyframe or a refresh. They turn off again on the live session with both fractions back
    /// at 1.0, and every frame carries the encoder's own error.
    #[test]
    fn layers_mark_every_other_frame_and_turn_off_live() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        let packets = layered_marks(&encoder, &rx, frame, (24, 16));
        // SAFETY: framework-provided constant strings.
        let (fraction_key, bits_key) = unsafe {
            (
                kVTCompressionPropertyKey_BaseLayerFrameRateFraction,
                kVTCompressionPropertyKey_BaseLayerBitRateFraction,
            )
        };
        assert_eq!(read_number(&encoder, fraction_key), Some(1.0));
        assert_eq!(
            read_number(&encoder, bits_key),
            Some(1.0),
            "the base layer has every bit"
        );
        assert!(
            packets
                .iter()
                .all(|p| p.mse.is_some_and(|m| m.luma >= 0.0 && m.luma_psnr() > 20.0)),
            "every frame carries the encoder's error"
        );
        assert_eq!(encoder.frames_dropped(), 0);
    }

    /// A session made for 60 frames a second declares 120 through its first frame, whatever
    /// rate is asked of it before then, and the last rate asked once that frame is in; a session
    /// made for 30 declares its own rate from the start and takes a new one at once.
    #[test]
    fn a_session_declares_the_placement_rate_until_its_first_frame() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        // SAFETY: framework-provided constant string.
        let key = unsafe { kVTCompressionPropertyKey_ExpectedFrameRate };
        let open = |fps: u16| {
            let config = EncoderConfig {
                width: u32::try_from(W).unwrap(),
                height: u32::try_from(H).unwrap(),
                codec: VideoCodec::Hevc,
                fps,
                bitrate_bps: 2_000_000,
                chroma: Chroma::Subsampled,
            };
            Encoder::new(config, |_packet| {}).unwrap()
        };

        let interactive = open(60);
        assert_eq!(
            read_number(&interactive, key),
            Some(120.0),
            "placed as a 120 session"
        );
        interactive.set_frame_rate(30).unwrap();
        assert_eq!(
            read_number(&interactive, key),
            Some(120.0),
            "a rate asked before the frame waits"
        );
        interactive
            .encode(&frame(0), 0, &FrameOptions::default())
            .unwrap();
        assert_eq!(
            read_number(&interactive, key),
            Some(30.0),
            "and is told after it"
        );
        interactive.set_frame_rate(60).unwrap();
        assert_eq!(
            read_number(&interactive, key),
            Some(60.0),
            "then every rate at once"
        );

        let slow = open(30);
        assert_eq!(read_number(&slow, key), Some(30.0), "its own rate");
        slow.set_frame_rate(15).unwrap();
        assert_eq!(read_number(&slow, key), Some(15.0), "a new rate at once");
    }

    /// The other two sessions the worker opens take layers as the 4:2:0 HEVC one does.
    #[test]
    fn layers_on_the_full_chroma_and_h264_sessions() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let full = full_chroma_encoder(tx);
        layered_marks(&full, &rx, frame_444, (24, 16));
        let (tx, rx) = std::sync::mpsc::channel();
        let config = EncoderConfig {
            width: u32::try_from(W).unwrap(),
            height: u32::try_from(H).unwrap(),
            codec: VideoCodec::H264,
            fps: 60,
            bitrate_bps: 2_000_000,
            chroma: Chroma::Subsampled,
        };
        let h264 = Encoder::new(config, move |packet| {
            let _receiver_gone = tx.send(packet);
        })
        .unwrap();
        layered_marks(&h264, &rx, frame, (24, 16));
    }

    /// The stream is full range and says BT.709 in its VUI, so the client's decoder outputs the
    /// captured format without a conversion pass and its shader reads the matrix off the frame.
    #[test]
    fn the_stream_is_full_range_bt709_end_to_end() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        use objc2_core_media::{
            kCMFormatDescriptionExtension_FullRangeVideo, kCMFormatDescriptionExtension_YCbCrMatrix,
        };
        use objc2_core_video::{
            CVPixelBufferGetPixelFormatType, kCVImageBufferYCbCrMatrixKey,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        for i in 0..3 {
            let options = FrameOptions {
                force_keyframe: i == 0,
                ..FrameOptions::default()
            };
            encoder
                .encode(&frame(i), u64::try_from(i).unwrap() * 16_667, &options)
                .unwrap();
        }
        encoder.flush().unwrap();
        let packets = collect(&rx, 3);
        let keyframe = packets
            .first()
            .filter(|p| p.keyframe)
            .expect("a keyframe first");
        let unit = nal::AccessUnit::parse(&keyframe.data, nal::hevc::is_parameter_set).unwrap();
        let sets: Vec<Vec<u8>> = unit.parameter_sets().iter().map(|s| s.to_vec()).collect();
        let format = crate::decoder::format_description(VideoCodec::Hevc, &sets).unwrap();
        // SAFETY: framework-provided constant string; the description is valid.
        let full = unsafe { format.extension(kCMFormatDescriptionExtension_FullRangeVideo) };
        // SAFETY: as above.
        let matrix = unsafe { format.extension(kCMFormatDescriptionExtension_YCbCrMatrix) };
        let full = full
            .and_then(|v| v.downcast::<CFBoolean>().ok())
            .map(|b| b.as_bool());
        assert_eq!(full, Some(true), "the VUI says full range");
        let matrix = matrix
            .and_then(|v| v.downcast::<CFString>().ok())
            .map(|m| m.to_string());
        // SAFETY: framework-provided constant string.
        let bt709 = unsafe { kCVImageBufferYCbCrMatrix_ITU_R_709_2 }.to_string();
        assert_eq!(
            matrix.as_deref(),
            Some(bt709.as_str()),
            "the VUI says BT.709"
        );

        let (dtx, drx) = std::sync::mpsc::channel();
        let mut decoder = crate::Decoder::new(VideoCodec::Hevc, move |frame| {
            let image = frame.image.as_cv();
            let format = CVPixelBufferGetPixelFormatType(image);
            // SAFETY: framework-provided constant string; a null mode pointer is allowed.
            let matrix = unsafe { image.attachment(kCVImageBufferYCbCrMatrixKey, ptr::null_mut()) }
                .and_then(|v| v.downcast::<CFString>().ok())
                .map(|m| m.to_string());
            let _receiver_gone = dtx.send((format, matrix));
        });
        for p in &packets {
            decoder.decode(&p.data.clone().into(), p.pts_us).unwrap();
        }
        let (format, matrix) = drx
            .recv_timeout(Duration::from_secs(60))
            .expect("a decoded frame");
        assert_eq!(format, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
        assert_eq!(
            matrix.as_deref(),
            Some(bt709.as_str()),
            "the frame carries the matrix"
        );
    }

    /// A full-range 10-bit bi-planar 4:4:4 frame (`xf44`) whose Cb alternates column by column,
    /// detail that 4:2:0 averages away.
    fn frame_444(index: usize) -> CFRetained<CVPixelBuffer> {
        use objc2_core_video::{
            CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
            CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress,
            CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo rule: a valid out-pointer and no attributes.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                W,
                H,
                pixel_format(Chroma::Full),
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0);
        // SAFETY: +1 reference from the create call.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };
        // SAFETY: CoreVideo rule: lock before touching the planes.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(locked, 0);
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            // Two bytes a sample; the chroma plane interleaves Cb and Cr.
            let width = if plane == 0 { 2 * W } else { 4 * W };
            for y in 0..H {
                // SAFETY: the plane is locked, so row `y < H` starts inside its mapping.
                let start = unsafe { base.add(y * stride) };
                // SAFETY: the row spans `stride >= width` writable bytes of the locked plane.
                let row = unsafe { std::slice::from_raw_parts_mut(start, width) };
                for (x, cell) in row.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let value: u16 = match (plane, x % 2, (x / 2) % 2) {
                        (0, ..) => u16::try_from((x + y + index * 7) % 800 + 120).unwrap(),
                        (_, 0, 0) => 256, // Cb, even column
                        (_, 0, _) => 768, // Cb, odd column
                        _ => 512,         // Cr
                    };
                    // CVPixelBuffer.h: 10 bits in the high bits of a little-endian 16.
                    *cell = (value << 6).to_le_bytes();
                }
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0);
        buffer
    }

    fn full_chroma_encoder(tx: std::sync::mpsc::Sender<EncodedPacket>) -> Encoder {
        let config = EncoderConfig {
            width: u32::try_from(W).unwrap(),
            height: u32::try_from(H).unwrap(),
            codec: VideoCodec::Hevc,
            fps: 60,
            bitrate_bps: 8_000_000,
            chroma: Chroma::Full,
        };
        Encoder::new(config, move |packet| {
            let _receiver_gone = tx.send(packet);
        })
        .unwrap()
    }

    /// A 4:4:4 session on the low-latency encoder keeps its long-term references, says 4:4:4 in
    /// its SPS, and decodes in hardware to a 4:4:4 picture that still has the column-by-column
    /// colour a 4:2:0 stream averages to one value.
    #[test]
    fn a_full_chroma_stream_is_444_end_to_end() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        use objc2_core_video::{
            CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
            CVPixelBufferGetPixelFormatType, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
            CVPixelBufferUnlockBaseAddress,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = full_chroma_encoder(tx);
        assert_eq!(encoder.chroma(), Chroma::Full);
        assert!(
            encoder.ltr_enabled(),
            "the low-latency 4:4:4 session has LTR"
        );
        let frames = 5;
        for i in 0..frames {
            let options = FrameOptions {
                force_keyframe: i == 0,
                ..FrameOptions::default()
            };
            encoder
                .encode(&frame_444(i), u64::try_from(i).unwrap() * 16_667, &options)
                .unwrap();
        }
        encoder.flush().unwrap();
        let packets = collect(&rx, frames);
        assert_eq!(packets.len(), frames);
        let keyframe = packets
            .first()
            .filter(|p| p.keyframe)
            .expect("a keyframe first");
        let unit = nal::AccessUnit::parse(&keyframe.data, nal::hevc::is_parameter_set).unwrap();
        let formats: Vec<nal::hevc::SampleFormat> = unit
            .parameter_sets()
            .iter()
            .filter_map(|set| nal::hevc::sample_format(set))
            .collect();
        let full = nal::hevc::SampleFormat {
            chroma_format_idc: 3,
            bit_depth: 10,
        };
        assert_eq!(formats, vec![full], "the SPS says 4:4:4, 10-bit");

        let (dtx, drx) = std::sync::mpsc::channel();
        let mut decoder = crate::Decoder::new(VideoCodec::Hevc, move |frame| {
            let image = frame.image.as_cv();
            let format = CVPixelBufferGetPixelFormatType(image);
            // SAFETY: CoreVideo rule: lock before reading the planes.
            let locked =
                unsafe { CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
            assert_eq!(locked, 0);
            let base = CVPixelBufferGetBaseAddressOfPlane(image, 1).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(image, 1);
            // SAFETY: the chroma plane is locked; row `H / 2` holds `W` interleaved 16-bit CbCr
            // pairs, `4 * W <= stride` bytes, inside its mapping.
            let start = unsafe { base.add(H / 2 * stride) };
            // SAFETY: `4 * W` readable bytes of the locked plane start at `start`.
            let row = unsafe { std::slice::from_raw_parts(start, 4 * W) };
            let cb: Vec<u16> = row
                .as_chunks::<4>()
                .0
                .iter()
                .map(|&[lo, hi, _, _]| u16::from_le_bytes([lo, hi]) >> 6)
                .collect();
            // SAFETY: matches the lock above.
            let unlocked =
                unsafe { CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
            assert_eq!(unlocked, 0);
            let _receiver_gone = dtx.send((format, cb));
        });
        for p in &packets {
            decoder.decode(&p.data.clone().into(), p.pts_us).unwrap();
        }
        let (format, cb) = drx
            .recv_timeout(Duration::from_secs(60))
            .expect("a decoded frame");
        assert_eq!(
            format,
            pixel_format(Chroma::Full),
            "decoded straight to xf44"
        );
        let swing = cb
            .windows(2)
            .map(|w| u32::from(w[0].abs_diff(w[1])))
            .sum::<u32>()
            / u32::try_from(cb.len() - 1).unwrap();
        assert!(
            swing > 360,
            "Cb alternates 256/768 column by column, mean swing {swing}"
        );
    }

    /// Fed a 4:2:0 picture, the hardware would quietly encode 4:2:0 under a 4:4:4 profile; the
    /// session refuses it instead.
    #[test]
    fn a_full_chroma_session_refuses_a_subsampled_picture() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (tx, _rx) = std::sync::mpsc::channel();
        let encoder = full_chroma_encoder(tx);
        let err = encoder
            .encode(&frame(0), 0, &FrameOptions::default())
            .unwrap_err();
        assert!(
            matches!(err, CodecError::NotFullChroma(f) if f == pixel_format(Chroma::Subsampled)),
            "{err}"
        );
    }

    /// A picture of another size than the session's is refused, not coded: VideoToolbox takes
    /// one quietly and codes it into the session's size, which is what a capture still at the
    /// size before a resize would have become.
    #[test]
    fn a_picture_of_another_size_is_refused() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        let small = {
            let mut raw: *mut CVPixelBuffer = ptr::null_mut();
            // SAFETY: CoreVideo rule: a valid out-pointer and no attributes.
            let status = unsafe {
                objc2_core_video::CVPixelBufferCreate(
                    None,
                    W / 2,
                    H,
                    objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                    None,
                    NonNull::from(&mut raw),
                )
            };
            assert_eq!(status, 0);
            // SAFETY: +1 reference from the create call.
            unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) }
        };
        let keyframe = FrameOptions {
            force_keyframe: true,
            ..FrameOptions::default()
        };
        let err = encoder.encode(&small, 1, &keyframe).unwrap_err();
        assert!(
            matches!(
                err,
                CodecError::WrongSize { image, session }
                    if image == (160, 180) && session == (320, 180)
            ),
            "{err}"
        );
        encoder.encode(&frame(0), 2, &keyframe).unwrap();
        encoder.flush().unwrap();
        let out = collect(&rx, 1);
        assert_eq!(
            out.iter()
                .map(|p| (p.pts_us, p.keyframe))
                .collect::<Vec<_>>(),
            [(2, true)]
        );
    }

    /// A frame that comes back from a malfunctioning encoder takes the session with it: every
    /// frame handed to it afterwards is refused as [`CodecError::EncoderLost`], with the status
    /// the frame came back with. A frame merely dropped (rate control, a busy encoder) loses
    /// nothing.
    #[test]
    fn a_frame_back_from_a_malfunction_loses_the_session() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        let keyframe = FrameOptions {
            force_keyframe: true,
            ..FrameOptions::default()
        };
        encoder.encode(&frame(0), 1, &keyframe).unwrap();
        encoder.flush().unwrap();
        let refcon = Arc::as_ptr(&encoder.shared).cast_mut().cast::<c_void>();
        // SAFETY: the refcon is the encoder's own `Shared`, alive for the call; a null sample
        // with a status or the dropped flag is what VideoToolbox passes for a frame it gave up.
        unsafe {
            output_callback(
                refcon,
                ptr::null_mut(),
                0,
                VTEncodeInfoFlags::FrameDropped,
                ptr::null_mut(),
            );
        }
        encoder
            .encode(&frame(1), 2, &FrameOptions::default())
            .unwrap();
        encoder.flush().unwrap();
        // SAFETY: as above.
        unsafe {
            output_callback(
                refcon,
                ptr::null_mut(),
                kVTVideoEncoderMalfunctionErr,
                VTEncodeInfoFlags::empty(),
                ptr::null_mut(),
            );
        }
        for pts in [3, 4] {
            let refused = encoder.encode(&frame(2), pts, &keyframe);
            assert!(
                matches!(refused, Err(CodecError::EncoderLost { status }) if status == kVTVideoEncoderMalfunctionErr),
                "{refused:?}"
            );
        }
        let out = collect(&rx, 2);
        assert_eq!(
            out.iter().map(|p| p.pts_us).collect::<Vec<_>>(),
            [1, 2],
            "none after it"
        );
    }

    /// A busy encoder keeps its session: a frame that comes back with
    /// `kVTVideoEncoderNotAvailableNowErr` is skipped and the next is coded. Only a run of them
    /// [`BUSY_LOST`] long with no frame coded between loses the session, and a frame coded
    /// starts the run over.
    #[test]
    fn a_busy_encoder_loses_the_session_only_once_it_stays_busy() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let busy = kVTVideoEncoderNotAvailableNowErr;
        let (tx, rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        let keyframe = FrameOptions {
            force_keyframe: true,
            ..FrameOptions::default()
        };
        encoder.encode(&frame(0), 1, &keyframe).unwrap();
        encoder.flush().unwrap();
        let refcon = Arc::as_ptr(&encoder.shared).cast_mut().cast::<c_void>();
        // SAFETY: the refcon is the encoder's own `Shared`, alive for the call; a null sample
        // with a status is what VideoToolbox passes for a frame it gave up.
        unsafe {
            output_callback(
                refcon,
                ptr::null_mut(),
                busy,
                VTEncodeInfoFlags::empty(),
                ptr::null_mut(),
            );
        }
        encoder
            .encode(&frame(1), 2, &FrameOptions::default())
            .unwrap();
        encoder.flush().unwrap();
        let out = collect(&rx, 2);
        assert_eq!(
            out.iter().map(|p| p.pts_us).collect::<Vec<_>>(),
            [1, 2],
            "coded after it"
        );

        let shared = &encoder.shared;
        let at = |ms: u64| shared.born + Duration::from_millis(ms);
        let under = BUSY_LOST.saturating_sub(Duration::from_millis(1));
        shared.refused(busy, at(10_000));
        shared.refused(busy, at(10_000) + under);
        assert!(shared.lost().is_ok(), "busy for less than the run");
        shared.coded();
        shared.refused(busy, at(20_000));
        shared.refused(busy, at(20_000) + under);
        assert!(shared.lost().is_ok(), "a frame coded started the run over");
        shared.refused(busy, at(20_000) + BUSY_LOST);
        let refused = encoder.encode(&frame(2), 3, &FrameOptions::default());
        assert!(
            matches!(refused, Err(CodecError::EncoderLost { status }) if status == busy),
            "busy for the whole run: {refused:?}"
        );
    }

    /// A session the system invalidated refuses the frame handed to it as
    /// [`CodecError::EncoderLost`], the status VideoToolbox answered the submit with.
    #[test]
    fn an_invalidated_session_is_lost() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (tx, _rx) = std::sync::mpsc::channel();
        let encoder = encoder(tx);
        // SAFETY: invalidating a live session is always allowed; it stays retained, and the
        // encoder's `Drop` invalidating it again is a no-op.
        unsafe { encoder.session.invalidate() }
        let refused = encoder.encode(&frame(0), 1, &FrameOptions::default());
        assert!(
            matches!(refused, Err(CodecError::EncoderLost { status }) if encoder_lost(status)),
            "{refused:?}"
        );
    }

    /// 4:4:4 is an HEVC profile; H.264 has none on this encoder.
    #[test]
    fn full_chroma_is_hevc_only() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let config = EncoderConfig {
            width: u32::try_from(W).unwrap(),
            height: u32::try_from(H).unwrap(),
            codec: VideoCodec::H264,
            fps: 60,
            bitrate_bps: 2_000_000,
            chroma: Chroma::Full,
        };
        let err = Encoder::new(config, |_packet| {}).unwrap_err();
        assert!(
            matches!(err, CodecError::NoFullChroma(VideoCodec::H264)),
            "{err}"
        );
    }

    /// What turning VideoToolbox's output into a packet costs on its callback thread: a 62 KB
    /// P-frame and a 300 KB keyframe in four slices. `cargo xtask bench --filter
    /// packet_conversion_cost` runs it; `docs/MEASUREMENTS.md` records runs.
    #[test]
    #[ignore = "a measurement; run with `cargo xtask bench`"]
    fn packet_conversion_cost() {
        let bench = onicode_desktop_testkit::bench::Bench::new("codec.packet_conversion_cost");
        for (name, len) in [("p_frame", 62_000_usize), ("keyframe", 300_000)] {
            let body = access_unit(&[len / 4; 4]);
            let sample = sample_of(&body);
            let mut convert = bench.series(name);
            let mut bytes = 0;
            for _ in 0..2_000 {
                bytes =
                    convert.time(|| packet(&sample, VideoCodec::Hevc, false).unwrap().data.len());
            }
            eprintln!("{name} {len} B: {bytes} B out");
            convert.report().unwrap();
        }
    }
}
