//! The host's encoder seam.
//!
//! What a screen stream needs from a video and an audio encoder, and the plain types that
//! cross it. Compiled on every target; each platform implements the traits in a module of its
//! own (VideoToolbox and `AudioConverter` on macOS).

pub use onicode_desktop_media::wire::Chroma;
use onicode_desktop_media::wire::VideoCodec;

use crate::CodecError;

/// Encoder settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EncoderConfig {
    /// Pixel width (even).
    pub width: u32,
    /// Pixel height (even).
    pub height: u32,
    /// Codec.
    pub codec: VideoCodec,
    /// Expected frame rate; drives the rate controller's window.
    pub fps: u16,
    /// Target bitrate, bits per second.
    pub bitrate_bps: u32,
    /// The colour the stream carries. [`Chroma::Full`] is HEVC only, and every picture must
    /// then be full-range 10-bit bi-planar 4:4:4 (`xf44`); the encoder refuses anything else
    /// rather than let the hardware quietly encode 4:2:0.
    pub chroma: Chroma,
}

/// Per-frame requests.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct FrameOptions {
    /// Encode an IDR.
    pub force_keyframe: bool,
    /// Encode a P-frame from an acknowledged long-term reference (an IDR if none is acked).
    pub force_ltr_refresh: bool,
    /// LTR tokens the receiver has acknowledged since the last frame.
    pub acked_ltr: Vec<u64>,
}

/// One encoded access unit: length-prefixed NAL units ([`crate::nal`]), the parameter sets in
/// front of a keyframe.
#[derive(Clone, PartialEq, Debug)]
pub struct EncodedPacket {
    /// Bitstream.
    pub data: Vec<u8>,
    /// IDR.
    pub keyframe: bool,
    /// Token the receiver must acknowledge for this frame to become a usable LTR.
    pub ltr_token: Option<u64>,
    /// This very frame was submitted with `force_ltr_refresh` (and the session has LTR).
    pub ltr_refresh: bool,
    /// Nothing later refers to this frame: the encoder's temporal layer 1
    /// ([`VideoEncoder::set_temporal_layers`]). Never a keyframe or a refresh.
    pub discardable: bool,
    /// How far the encoder's own reconstruction of this frame is from its source, when the
    /// session measures it.
    pub mse: Option<Mse>,
    /// The presentation timestamp passed to `encode`.
    pub pts_us: u64,
}

/// Mean squared error of an encoded frame against its source, per sample, on the encoder's own
/// reconstruction (which may lack the decoder's loop filters, so it can read a little off the
/// true figure).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mse {
    /// Luma.
    pub luma: f64,
    /// The mean of the two chroma planes, when the encoder gave both.
    pub chroma: Option<f64>,
}

impl Mse {
    /// Luma PSNR in dB for 8-bit samples; infinite for a lossless frame.
    #[must_use]
    pub fn luma_psnr(&self) -> f64 {
        10.0 * (255.0 * 255.0 / self.luma).log10()
    }
}

/// A hardware video encoder, as a screen stream drives it.
///
/// Every call comes from whichever thread has the frame (the capture queue) or the feedback (a
/// connection task), so an encoder is shared and takes `&self`. Packets come back through the
/// sink given at construction, on the encoder's own threads.
pub trait VideoEncoder: Send + Sync + Sized + 'static {
    /// A picture as the capture delivers it.
    type Image;

    /// A session for `config`; each encoded access unit goes to `sink`.
    ///
    /// # Errors
    ///
    /// The platform's encoder refused the configuration.
    fn new(
        config: EncoderConfig,
        sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError>;

    /// A session for one stripe of the pictures handed to it (`crate::stripes`): `config`
    /// describes the whole picture, every picture handed to [`Self::encode`] is a whole capture
    /// of that size, and the session codes the rows `stripe` names, a picture `coded_rows`
    /// high.
    ///
    /// # Errors
    ///
    /// [`CodecError::NoStripes`] for an encoder that codes no stripes (the default), else as
    /// [`Self::new`].
    fn stripe(
        _config: EncoderConfig,
        _stripe: crate::stripes::Stripe,
        _sink: impl Fn(EncodedPacket) + Send + Sync + 'static,
    ) -> Result<Self, CodecError> {
        Err(CodecError::NoStripes)
    }

    /// Whether this machine codes a `width` × `height` picture carrying `chroma` faster as two
    /// stripes at once than whole, timed now (`crate::stripes::side_by_side`): a few hundred
    /// milliseconds of the encode engines, once per size. `None` for an encoder that codes no
    /// stripes (the default), or a picture too short to split.
    ///
    /// # Errors
    ///
    /// When a session for the timing is refused.
    fn side_by_side(
        _width: u32,
        _height: u32,
        _chroma: Chroma,
    ) -> Result<Option<crate::stripes::SideBySide>, CodecError> {
        Ok(None)
    }

    /// Submit one picture; `pts_us` comes back on its packet.
    ///
    /// # Errors
    ///
    /// The encoder refused the frame.
    fn encode(
        &self,
        image: &Self::Image,
        pts_us: u64,
        options: &FrameOptions,
    ) -> Result<(), CodecError>;

    /// Change the target bitrate, bits per second.
    ///
    /// # Errors
    ///
    /// The encoder refused the property.
    fn set_bitrate(&self, bps: u32) -> Result<(), CodecError>;

    /// Tell the rate controller the cadence changed.
    ///
    /// # Errors
    ///
    /// The encoder refused the property.
    fn set_frame_rate(&self, fps: u16) -> Result<(), CodecError>;

    /// Turn temporal layers on or off from the next frame: with them on, every other frame is
    /// one nothing later refers to ([`EncodedPacket::discardable`]), which a receiver may skip
    /// when it cannot repair it. Returns whether the session writes layers now. An encoder
    /// without them writes none, whatever it is asked.
    ///
    /// # Errors
    ///
    /// The encoder refused the property.
    fn set_temporal_layers(&self, _on: bool) -> Result<bool, CodecError> {
        Ok(false)
    }

    /// Frames the session gave up since it opened (its rate control dropped them, or they
    /// failed); 0 for an encoder that never does.
    fn frames_dropped(&self) -> u64 {
        0
    }
}

/// An Opus encoder for the captured audio: 48 kHz interleaved stereo float in, 10 ms packets
/// out.
pub trait AudioEncoder: Send + Sized + 'static {
    /// A fresh encoder.
    ///
    /// # Errors
    ///
    /// The platform has no encoder to give.
    fn new() -> Result<Self, CodecError>;

    /// Feed samples; every whole packet they complete goes to `out`, in order.
    ///
    /// # Errors
    ///
    /// The encoder failed on the samples.
    fn push(&mut self, samples: &[f32], out: impl FnMut(&[u8])) -> Result<(), CodecError>;
}
