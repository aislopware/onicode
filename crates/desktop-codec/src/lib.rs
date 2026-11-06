//! Hardware video codecs behind a small, callback-driven API.
//!
//! Ported from slopty's `slopty-codec` (MIT, same authors) with its tests and probes. Comments
//! citing `docs/decisions/*.md` and `docs/MEASUREMENTS.md` name slopty's records, where each
//! choice was measured. Tests that open a `VideoToolbox` session or the audio output run only when
//! asked (`ONICODE_VT_TESTS=1`, `ONICODE_AUDIO_LIVE=1`; `onicode_desktop_testkit::live`), since
//! hosted runners' paravirtual encoder hangs a process.
//!
//! * [`nal`] — pure length-prefixed NAL unit framing, used by both halves and on every platform.
//! * [`conformance`] — the SPS conformance window that shows a picture coded at a padded size at
//!   its true one.
//! * [`stripes`] — one picture coded as two stripes on the two encode engines: the layout both ends
//!   share, and on macOS the stripe pictures and the gate that says whether they pay.
//! * [`video`] — the host's encoder seam, [`VideoEncoder`] and [`AudioEncoder`], on every platform;
//!   `VideoToolbox` and `Opus` implement it on macOS.
//! * `Encoder` (macOS) — a `VTCompressionSession` tuned for interactive streaming: low-latency rate
//!   control, no reordering, infinite GOP with long-term references, length-prefixed units out.
//! * `Decoder` (macOS, iOS) — a `VTDecompressionSession` fed the units as they arrive; it rebuilds
//!   its format description from the parameter sets in front of each keyframe.
//! * [`audio`] — Opus through `AudioConverter` (encode on the host, decode anywhere) and a player
//!   that renders from its jitter ring on the output unit's I/O thread.
//!
//! Output is delivered on VideoToolbox's own threads through the sink closure given at
//! construction; sinks must be cheap (hand the packet to a channel).

// RealtimeSanitizer's attribute on the audio render callback, in a nightly deep lane only
// (`--cfg onicode_rtsan`).
#![cfg_attr(onicode_rtsan, feature(sanitize))]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

#[cfg(target_vendor = "apple")]
pub mod audio;
pub mod conformance;
pub mod nal;
pub mod stripes;
pub mod video;

#[cfg(target_vendor = "apple")]
mod cf;
#[cfg(target_vendor = "apple")]
mod decoder;
#[cfg(target_os = "macos")]
mod encoder;

#[cfg(target_vendor = "apple")]
pub use audio::Opus;
#[cfg(target_vendor = "apple")]
pub use cf::micros;
#[cfg(target_vendor = "apple")]
pub use decoder::{
    DecodeFailure, DecodeOutcome, DecodedFrame, Decoder, PixelBuffer, session_lost, warm_up,
};
#[cfg(all(target_os = "macos", feature = "experiments"))]
pub use encoder::RateControl;
#[cfg(target_os = "macos")]
pub use encoder::{Encoder, VideoToolbox, pixel_format};
pub use video::{
    AudioEncoder, Chroma, EncodedPacket, EncoderConfig, FrameOptions, Mse, VideoEncoder,
};

/// Codec failures. The `OSStatus` codes are VideoToolbox's (`kVT*Err`, negative).
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum CodecError {
    /// A `VideoToolbox` or `CoreMedia` call failed.
    #[error("{call} failed: OSStatus {status}")]
    Os {
        /// The API that failed.
        call: &'static str,
        /// The status code.
        status: i32,
    },
    /// The system took the encoder session away: no frame handed to it comes out again, and
    /// only a new session codes. `status` is what its frames came back with.
    #[error("the encoder session is gone: OSStatus {status}")]
    EncoderLost {
        /// The status code.
        status: i32,
    },
    /// The bitstream has no parameter sets and no decoder exists yet.
    #[error("no parameter sets seen yet; waiting for a keyframe")]
    NoParameterSets,
    /// An access unit whose NAL lengths do not add up.
    #[error("malformed NAL unit length at byte {offset}")]
    MalformedNal {
        /// Where the bad length starts.
        offset: usize,
    },
    /// The codec is not supported on this platform.
    #[error("unsupported codec {0:?}")]
    Unsupported(onicode_desktop_media::wire::VideoCodec),
    /// The encoder offers no 4:4:4 profile for this codec.
    #[error("no 4:4:4 encoder for {0:?}")]
    NoFullChroma(onicode_desktop_media::wire::VideoCodec),
    /// A 4:4:4 session was handed a picture in another pixel format (the four-character code);
    /// the hardware would have encoded it as 4:2:0.
    #[error("a 4:4:4 session needs xf44 pictures, got {0:#010x}")]
    NotFullChroma(u32),
    /// A session was handed a picture of another size than its own: a capture from before a
    /// resize, which the encoder would have coded into the session's size.
    #[error("a {session:?} session was handed a {image:?} picture")]
    WrongSize {
        /// The picture's width and height.
        image: (usize, usize),
        /// The session's.
        session: (usize, usize),
    },
    /// A picture in another pixel format than the one asked for (four-character codes).
    #[error("a picture in {got:#010x} where {expected:#010x} was asked for")]
    PixelFormat {
        /// The format asked for.
        expected: u32,
        /// The picture's.
        got: u32,
    },
    /// This encoder codes no stripes ([`VideoEncoder::stripe`]).
    #[error("this encoder codes no stripes")]
    NoStripes,
    /// This platform has no audio encoder (`docs/decisions/platform.md`, "Linux seams").
    #[error("no audio encoder on this platform")]
    NoAudio,
    /// The process was started to open no `VideoToolbox` session ([`NO_VIDEOTOOLBOX`]), and
    /// `call` would have opened one.
    #[error("{call} refused: this process opens no VideoToolbox session (ONICODE_NO_VIDEOTOOLBOX)")]
    Refused {
        /// The session's create call.
        call: &'static str,
    },
}

/// The variable that, set in a process's environment, has every `VideoToolbox` session the
/// process would open refused with [`CodecError::Refused`], encoder and decoder alike.
///
/// A virtual Mac's paravirtual codec keeps two clients of every process that opened a session
/// until the guest restarts, and a guest's codec that wedges holds each process that opens one
/// in the kernel as it exits (`docs/decisions/video.md`, "A virtual Mac's encoder stops for good
/// past its 1020th client"). The test runs that code no video set it, so a test that opens a
/// session there fails at once, naming the call, and belongs in the `videotoolbox` test group.
pub const NO_VIDEOTOOLBOX: &str = "ONICODE_NO_VIDEOTOOLBOX";

/// Refuse `call`, a session's create, when this process opens no `VideoToolbox` session
/// ([`NO_VIDEOTOOLBOX`], read once).
#[cfg(target_vendor = "apple")]
fn admit(call: &'static str) -> Result<(), CodecError> {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    admitted(
        *OFF.get_or_init(|| std::env::var_os(NO_VIDEOTOOLBOX).is_some()),
        call,
    )
}

/// `call`, refused while `VideoToolbox` is switched `off` for the process.
#[cfg(target_vendor = "apple")]
const fn admitted(off: bool, call: &'static str) -> Result<(), CodecError> {
    if off {
        Err(CodecError::Refused { call })
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[cfg(target_vendor = "apple")]
mod switch_tests {
    use super::*;

    /// With `VideoToolbox` switched off, a session's create is refused, named; with it on, it
    /// goes ahead.
    #[test]
    fn a_process_switched_off_has_its_sessions_refused() {
        let call = "VTDecompressionSessionCreate";
        assert!(matches!(admitted(true, call), Err(CodecError::Refused { call: c }) if c == call));
        admitted(false, call).unwrap();
        let said = CodecError::Refused { call }.to_string();
        assert!(
            said.contains(call) && said.contains(NO_VIDEOTOOLBOX),
            "{said}"
        );
    }
}
