//! Small CoreFoundation / VideoToolbox helpers shared by the encoder and decoder.

use std::ptr::NonNull;

use objc2_core_foundation::{CFBoolean, CFString, CFType};
#[cfg(target_os = "macos")]
use objc2_core_foundation::{CFNumber, CFRetained};
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_video_toolbox::{VTSession, VTSessionSetProperty};

use crate::CodecError;

/// Turn an `OSStatus` into a `Result`.
pub(crate) const fn check(call: &'static str, status: i32) -> Result<(), CodecError> {
    if status == 0 {
        Ok(())
    } else {
        Err(CodecError::Os { call, status })
    }
}

/// Set one property on a compression or decompression session.
///
/// `session` is the session object viewed as a plain CF type; both session kinds are
/// `VTSessionRef`s in the C API even though objc2 models them as unrelated opaque types.
pub(crate) fn set_property(
    session: &CFType,
    key: &CFString,
    value: &CFType,
    call: &'static str,
) -> Result<(), CodecError> {
    let ptr: NonNull<CFType> = NonNull::from(session);
    // SAFETY: `VTCompressionSessionRef` and `VTDecompressionSessionRef` are both `VTSessionRef`
    // (VTSession.h: "VTSessionRef ... is a CFTypeRef that VTCompressionSessionRef and
    // VTDecompressionSessionRef are compatible with"); the pointee is never mutated through
    // this reference and lives as long as `session`.
    let session: &VTSession = unsafe { ptr.cast::<VTSession>().as_ref() };
    // SAFETY: key and value are valid CF objects for the duration of the call.
    let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
    check(call, status)
}

/// A boolean property value.
pub(crate) fn boolean(value: bool) -> &'static CFType {
    CFBoolean::new(value)
}

/// An integer property value.
#[cfg(target_os = "macos")]
pub(crate) fn int(value: i64) -> CFRetained<CFNumber> {
    CFNumber::new_i64(value)
}

/// A float property value.
#[cfg(target_os = "macos")]
pub(crate) fn float(value: f64) -> CFRetained<CFNumber> {
    CFNumber::new_f64(value)
}

/// A microsecond timestamp as a `CMTime`.
pub(crate) fn time_us(us: u64) -> CMTime {
    CMTime {
        value: i64::try_from(us).unwrap_or(i64::MAX),
        timescale: 1_000_000,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

/// Microseconds from a `CMTime`, `None` when invalid. The one conversion every CoreMedia
/// timestamp in the media path goes through.
pub fn micros(time: CMTime) -> Option<u64> {
    if !time.flags.contains(CMTimeFlags::Valid) || time.timescale <= 0 {
        return None;
    }
    let value = u128::from(u64::try_from(time.value).ok()?);
    let scale = u128::from(u32::try_from(time.timescale).ok()?);
    // The worker clock is nanoseconds since boot (about 1e15 after days of uptime), so the
    // product needs more than 64 bits.
    u64::try_from(value.saturating_mul(1_000_000).checked_div(scale)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micros_survives_large_worker_clock_values() {
        // 12 days of uptime on the nanosecond worker clock.
        let ns = 12 * 24 * 3600 * 1_000_000_000_i64;
        let t = CMTime {
            value: ns,
            timescale: 1_000_000_000,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        assert_eq!(micros(t), Some(12 * 24 * 3600 * 1_000_000));
        assert_eq!(micros(time_us(123_456)), Some(123_456));
        let invalid = CMTime {
            value: 0,
            timescale: 0,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        assert_eq!(micros(invalid), None);
    }
}
