//! NAL unit framing, pure and testable.
//!
//! An access unit travels as VideoToolbox writes it: each NAL unit behind its 4-byte big-endian
//! length, as in `hvcC`/`avcC`. A keyframe carries its parameter sets in front, as units of
//! their own, so a receiver can (re)build its decoder from any keyframe it reassembles; a
//! decoder takes the picture's units behind them as they are (`docs/decisions/video.md`, "HEVC
//! travels length-prefixed").

use crate::CodecError;

/// HEVC NAL unit types that carry parameter sets.
pub mod hevc {
    /// Video parameter set.
    pub const VPS: u8 = 32;
    /// Sequence parameter set.
    pub const SPS: u8 = 33;
    /// Picture parameter set.
    pub const PPS: u8 = 34;
    /// IDR with RADL pictures.
    pub const IDR_W_RADL: u8 = 19;
    /// IDR without leading pictures.
    pub const IDR_N_LP: u8 = 20;

    /// The type of an HEVC NAL unit: bits 1..7 of the first header byte.
    #[must_use]
    pub fn nal_type(nal: &[u8]) -> Option<u8> {
        nal.first().map(|b| (b >> 1) & 0x3f)
    }

    /// True for VPS/SPS/PPS.
    #[must_use]
    pub fn is_parameter_set(nal: &[u8]) -> bool {
        matches!(nal_type(nal), Some(VPS | SPS | PPS))
    }

    /// How an SPS samples its pictures (H.265 7.4.3.2).
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct SampleFormat {
        /// 1 for 4:2:0, 2 for 4:2:2, 3 for 4:4:4 (0 is monochrome).
        pub chroma_format_idc: u32,
        /// Bits per luma sample.
        pub bit_depth: u32,
    }

    /// The sample format of an SPS NAL unit (header included); `None` for anything that is not
    /// a well-formed SPS.
    #[must_use]
    pub fn sample_format(nal: &[u8]) -> Option<SampleFormat> {
        let head = crate::conformance::head(nal)?;
        Some(SampleFormat {
            chroma_format_idc: head.chroma_format_idc,
            bit_depth: head.bit_depth,
        })
    }

    /// The size a decoder outputs for an SPS NAL unit (header included): the coded size less
    /// its conformance window.
    #[must_use]
    pub fn shown_size(nal: &[u8]) -> Option<(u32, u32)> {
        crate::conformance::head(nal)?.shown()
    }
}

/// The 4-byte length in front of a NAL unit.
const PREFIX: usize = 4;

/// Iterate the NAL units of an access unit, without their lengths.
///
/// The walk stops at a length that runs past the end; [`check`] says whether there was one.
pub fn units(access_unit: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = access_unit;
    std::iter::from_fn(move || {
        let (unit, next) = split_first(rest).ok()??;
        rest = next;
        Some(unit)
    })
}

/// A unit and what follows it.
type Split<'a> = (&'a [u8], &'a [u8]);

/// The first unit and what follows it; `Ok(None)` at the end, an error for a length that runs
/// past it, a tail too short for a length, or a unit of no bytes (no header).
fn split_first(bytes: &[u8]) -> Result<Option<Split<'_>>, ()> {
    if bytes.is_empty() {
        return Ok(None);
    }
    let (prefix, rest) = bytes.split_first_chunk::<PREFIX>().ok_or(())?;
    let len = usize::try_from(u32::from_be_bytes(*prefix)).map_err(|_too_long| ())?;
    if len == 0 {
        return Err(());
    }
    let (unit, rest) = rest.split_at_checked(len).ok_or(())?;
    Ok(Some((unit, rest)))
}

/// Whether every length of `access_unit` lands on the next one and the last on its end.
///
/// # Errors
///
/// [`CodecError::MalformedNal`] with the offset of the first length that does not: those bytes
/// are not an access unit, and nothing of them should reach a decoder.
pub fn check(access_unit: &[u8]) -> Result<(), CodecError> {
    let mut rest = access_unit;
    loop {
        let offset = access_unit.len().wrapping_sub(rest.len());
        match split_first(rest) {
            Ok(Some((_unit, next))) => rest = next,
            Ok(None) => return Ok(()),
            Err(()) => return Err(CodecError::MalformedNal { offset }),
        }
    }
}

/// Append `nal` behind its length.
///
/// # Errors
///
/// A unit of no bytes, or one longer than a 4-byte length can say.
pub fn push(out: &mut Vec<u8>, nal: &[u8]) -> Result<(), CodecError> {
    let malformed = CodecError::MalformedNal { offset: out.len() };
    let len = u32::try_from(nal.len()).map_err(|_too_long| malformed)?;
    if len == 0 {
        return Err(malformed);
    }
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(nal);
    Ok(())
}

/// One access unit, split in a walk over its lengths: the parameter sets in front of it, and
/// where the picture's units start. Those stay length-prefixed, as a decoder takes them.
#[derive(Debug)]
pub struct AccessUnit<'a> {
    parameter_sets: Vec<&'a [u8]>,
    picture: usize,
}

impl<'a> AccessUnit<'a> {
    /// Walk `access_unit` once.
    ///
    /// # Errors
    ///
    /// A length that runs past the end ([`check`]).
    pub fn parse(
        access_unit: &'a [u8],
        is_parameter_set: fn(&[u8]) -> bool,
    ) -> Result<Self, CodecError> {
        let mut parameter_sets = Vec::new();
        let mut rest = access_unit;
        let mut picture = None;
        loop {
            let offset = access_unit.len().wrapping_sub(rest.len());
            match split_first(rest) {
                Ok(Some((unit, next))) => {
                    if picture.is_none() {
                        if is_parameter_set(unit) {
                            parameter_sets.push(unit);
                        } else {
                            picture = Some(offset);
                        }
                    }
                    rest = next;
                }
                Ok(None) => break,
                Err(()) => return Err(CodecError::MalformedNal { offset }),
            }
        }
        Ok(Self {
            parameter_sets,
            picture: picture.unwrap_or(access_unit.len()),
        })
    }

    /// The parameter sets, in stream order, without their lengths.
    #[must_use]
    pub fn parameter_sets(&self) -> &[&'a [u8]] {
        &self.parameter_sets
    }

    /// Where the picture's units start, lengths included; the end of the unit when it carries no
    /// picture.
    #[must_use]
    pub const fn picture_at(&self) -> usize {
        self.picture
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    /// `units` behind their lengths.
    pub(crate) fn access_unit(units: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for nal in units {
            push(&mut out, nal).unwrap();
        }
        out
    }

    #[test]
    fn units_come_back_without_their_lengths() {
        let unit = [0, 0, 0, 3, 0x40, 1, 2, 0, 0, 0, 1, 0x44];
        let units: Vec<&[u8]> = units(&unit).collect();
        assert_eq!(units, vec![&[0x40, 1, 2][..], &[0x44]]);
        assert_eq!(super::units(&[]).count(), 0);
        assert_eq!(access_unit(&[&[0x40, 1, 2], &[0x44]]), unit.to_vec());
    }

    /// A unit of no bytes has no header: no encoder writes one, and one that arrives is not
    /// handed on to a decoder.
    #[test]
    fn an_empty_unit_is_malformed() {
        assert!(matches!(
            check(&[0, 0, 0, 0]),
            Err(CodecError::MalformedNal { offset: 0 })
        ));
        assert!(matches!(
            check(&[0, 0, 0, 1, 0x26, 0, 0, 0, 0]),
            Err(CodecError::MalformedNal { offset: 5 })
        ));
        assert_eq!(
            units(&[0, 0, 0, 1, 0x26, 0, 0, 0, 0]).count(),
            1,
            "the walk stops there"
        );
        AccessUnit::parse(&[0, 0, 0, 0, 0, 0, 0, 1, 0x26], hevc::is_parameter_set).unwrap_err();
        push(&mut Vec::new(), &[]).expect_err("nor is one written");
    }

    /// A length past the end or a tail shorter than a length is refused, never truncated, and
    /// the walk stops there.
    #[test]
    fn a_malformed_length_is_an_error() {
        let offset = |bytes: &[u8]| match check(bytes) {
            Err(CodecError::MalformedNal { offset }) => Some(offset),
            _other => None,
        };
        assert_eq!(offset(&[0, 0, 0, 9, 1]), Some(0), "runs past the end");
        assert_eq!(offset(&[0, 0]), Some(0), "shorter than a length");
        assert_eq!(
            offset(&[0, 0, 0, 1, 0x26, 0, 0]),
            Some(5),
            "a stub after a good unit"
        );
        assert_eq!(
            offset(&[0xff, 0xff, 0xff, 0xff, 1]),
            Some(0),
            "a huge length"
        );
        assert_eq!(offset(&[0, 0, 0, 1, 0x26]), None);
        assert_eq!(
            offset(&[]),
            None,
            "an access unit of no units is well formed"
        );
        assert_eq!(
            units(&[0, 0, 0, 1, 0x26, 0, 0]).count(),
            1,
            "the walk stops at the stub"
        );
        AccessUnit::parse(&[0, 0, 0, 1, 0x26, 0, 0], hevc::is_parameter_set).unwrap_err();
    }

    #[test]
    fn the_parameter_sets_in_front_are_split_from_the_picture() {
        let sets: [&[u8]; 3] = [&[0x40, 1], &[0x42, 2], &[0x44, 3]];
        let mut unit = access_unit(&sets);
        let picture_at = unit.len();
        unit.extend(access_unit(&[&[0x26, 0xff], &[0x44, 9]]));
        let parsed = AccessUnit::parse(&unit, hevc::is_parameter_set).unwrap();
        assert_eq!(parsed.parameter_sets(), &sets);
        assert_eq!(
            parsed.picture_at(),
            picture_at,
            "a set behind the picture stays in it"
        );
        let delta = access_unit(&[&[0x02, 1]]);
        let parsed = AccessUnit::parse(&delta, hevc::is_parameter_set).unwrap();
        assert_eq!((parsed.parameter_sets().len(), parsed.picture_at()), (0, 0));
        let sets_only = access_unit(&sets);
        let parsed = AccessUnit::parse(&sets_only, hevc::is_parameter_set).unwrap();
        assert_eq!(parsed.picture_at(), sets_only.len(), "no picture");
    }

    #[test]
    fn hevc_types() {
        assert_eq!(hevc::nal_type(&[0x40, 1]), Some(hevc::VPS));
        assert_eq!(hevc::nal_type(&[0x42, 1]), Some(hevc::SPS));
        assert_eq!(hevc::nal_type(&[0x44, 1]), Some(hevc::PPS));
        assert_eq!(hevc::nal_type(&[0x26, 1]), Some(hevc::IDR_W_RADL));
        assert!(hevc::is_parameter_set(&[0x44]));
        assert!(!hevc::is_parameter_set(&[0x26]));
        assert_eq!(hevc::nal_type(&[]), None);
    }

    /// SPSs the M1 Max's low-latency encoder wrote at 320×180 (2026-09-28) for Main, Main 4:4:4
    /// and Main 4:4:4 10: two temporal sub-layers and emulation-prevention bytes, so the parser
    /// walks both.
    pub(crate) const SPS_420: [u8; 86] = [
        0x42, 0x01, 0x03, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0xb0, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x03, 0x00, 0x3f, 0x00, 0x00, 0xa0, 0x0a, 0x08, 0x0c, 0x1f, 0x3e, 0x20, 0x10, 0xee, 0x45,
        0x20, 0x82, 0xe7, 0xe1, 0x3d, 0x0b, 0xea, 0x1b, 0xd5, 0x0f, 0xea, 0xa0, 0x8f, 0x55, 0x41,
        0x3e, 0xaa, 0xa0, 0xaf, 0x55, 0x54, 0x17, 0xea, 0xaa, 0xa0, 0xcf, 0x55, 0x55, 0x41, 0xbe,
        0xaa, 0xaa, 0xa0, 0xef, 0x55, 0x55, 0x54, 0x1f, 0xea, 0xaa, 0xaa, 0xa0, 0x43, 0xd5, 0x55,
        0x55, 0x52, 0x9b, 0x81, 0x01, 0x00, 0x81, 0xfc, 0x20, 0x10, 0x40,
    ];
    pub(crate) const SPS_444: [u8; 85] = [
        0x42, 0x01, 0x03, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0xbe, 0x08, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x5d, 0x00, 0x00, 0x90, 0x01, 0x41, 0x01, 0x83, 0xe3, 0x71, 0x00, 0x87, 0x72, 0x29,
        0x04, 0x17, 0x3f, 0x09, 0xe8, 0x5f, 0x50, 0xde, 0xa8, 0x7f, 0x55, 0x04, 0x7a, 0xaa, 0x09,
        0xf5, 0x55, 0x05, 0x7a, 0xaa, 0xa0, 0xbf, 0x55, 0x55, 0x06, 0x7a, 0xaa, 0xaa, 0x0d, 0xf5,
        0x55, 0x55, 0x07, 0x7a, 0xaa, 0xaa, 0xa0, 0xff, 0x55, 0x55, 0x55, 0x02, 0x1e, 0xaa, 0xaa,
        0xaa, 0x94, 0xdc, 0x08, 0x08, 0x04, 0x0f, 0xe1, 0x00, 0x82,
    ];

    pub(crate) const SPS_444_10: [u8; 86] = [
        0x42, 0x01, 0x03, 0x04, 0x08, 0x00, 0x00, 0x03, 0x00, 0xbc, 0x08, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x5d, 0x00, 0x00, 0x90, 0x01, 0x41, 0x01, 0x83, 0xe3, 0x5b, 0x10, 0x08, 0x77, 0x22,
        0x90, 0x41, 0x73, 0xf0, 0x9e, 0x85, 0xf5, 0x0d, 0xea, 0x87, 0xf5, 0x50, 0x47, 0xaa, 0xa0,
        0x9f, 0x55, 0x50, 0x57, 0xaa, 0xaa, 0x0b, 0xf5, 0x55, 0x50, 0x67, 0xaa, 0xaa, 0xa0, 0xdf,
        0x55, 0x55, 0x50, 0x77, 0xaa, 0xaa, 0xaa, 0x0f, 0xf5, 0x55, 0x55, 0x50, 0x21, 0xea, 0xaa,
        0xaa, 0xa9, 0x4d, 0xc0, 0x80, 0x80, 0x40, 0xfe, 0x10, 0x08, 0x20,
    ];

    #[test]
    fn the_sps_says_which_chroma_format_the_stream_is() {
        let format = |chroma_format_idc, bit_depth| {
            Some(hevc::SampleFormat {
                chroma_format_idc,
                bit_depth,
            })
        };
        assert_eq!(hevc::sample_format(&SPS_420), format(1, 8), "Main");
        assert_eq!(hevc::sample_format(&SPS_444), format(3, 8), "Main 4:4:4");
        assert_eq!(
            hevc::sample_format(&SPS_444_10),
            format(3, 10),
            "Main 4:4:4 10"
        );
        let pps = [0x44, 0x01, 0xc0, 0x72, 0xf0, 0x5b, 0x24];
        assert_eq!(hevc::sample_format(&pps), None, "not an SPS");
        assert_eq!(
            hevc::sample_format(&SPS_444[..12]),
            None,
            "cut off in the profile"
        );
        assert_eq!(
            hevc::sample_format(&SPS_444[..24]),
            None,
            "cut off before the bit depth"
        );
        assert_eq!(hevc::sample_format(&[]), None);
    }
}
