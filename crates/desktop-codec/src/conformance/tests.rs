use pretty_assertions::assert_eq;

use super::*;
use crate::nal::hevc;
use crate::nal::tests::{SPS_420, SPS_444, SPS_444_10, access_unit};

#[test]
fn exp_golomb_round_trips() {
    let values: Vec<u32> = (0..600)
        .chain((1..32).flat_map(|k| {
            let p = 1_u32 << k;
            [p - 2, p - 1, p]
        }))
        .chain([u32::MAX - 1])
        .collect();
    let mut w = Writer::default();
    for &v in &values {
        w.ue(v).unwrap();
        w.bit(true);
    }
    let bytes = w.finish();
    let mut r = Reader {
        bytes: &bytes,
        at: 0,
    };
    for &v in &values {
        assert_eq!(r.ue(), Some(v), "value {v}");
        assert_eq!(r.flag(), Some(true), "marker after {v}");
    }
    let mut w = Writer::default();
    w.ue(0).unwrap();
    w.ue(4).unwrap();
    assert_eq!(
        w.finish(),
        vec![0b1001_0100],
        "1, then 00101, padded with zeros"
    );
}

#[test]
fn emulation_prevention_round_trips() {
    let cases: [(&[u8], &[u8]); 6] = [
        (&[0, 0, 0], &[0, 0, 3, 0]),
        (&[0, 0, 1, 7], &[0, 0, 3, 1, 7]),
        (&[0, 0, 2], &[0, 0, 3, 2]),
        (&[0, 0, 3, 0, 0, 3], &[0, 0, 3, 3, 0, 0, 3, 3]),
        (&[0, 0, 4, 0, 0], &[0, 0, 4, 0, 0]),
        (&[5, 0, 0, 0, 0, 1], &[5, 0, 0, 3, 0, 0, 3, 1]),
    ];
    for (rbsp, escaped) in cases {
        let mut out = Vec::new();
        escape_into(rbsp, &mut out);
        assert_eq!(out, escaped.to_vec(), "escape {rbsp:?}");
        assert_eq!(unescape(escaped), rbsp.to_vec(), "unescape {escaped:?}");
    }
}

/// The encoder's own SPSs at 320 × 180 code 320 × 192 and carry a bottom window; the head
/// reads it back as the size the encoder was given.
#[test]
fn the_head_reads_the_coded_size_and_the_window() {
    for (sps, chroma, (sw, sh)) in [
        (&SPS_420[..], 1, (2, 2)),
        (&SPS_444, 3, (1, 1)),
        (&SPS_444_10, 3, (1, 1)),
    ] {
        let head = head(sps).unwrap();
        assert_eq!(head.chroma_format_idc, chroma);
        assert_eq!(head.units(), (sw, sh));
        assert_eq!(
            (head.width, head.height),
            (320, 192),
            "coded in whole 16-pixel blocks"
        );
        assert_eq!(head.shown(), Some((320, 180)), "{head:?}");
        assert_eq!(hevc::shown_size(sps), Some((320, 180)));
    }
}

/// Rewriting a window to the one the encoder wrote reproduces its SPS bit for bit, so the
/// rewrite copies everything around the window exactly.
#[test]
fn cropping_to_the_encoders_own_window_is_the_identity() {
    for sps in [&SPS_420[..], &SPS_444, &SPS_444_10] {
        assert_eq!(crop_sps(sps, (320, 180)), Some(sps.to_vec()));
    }
}

#[test]
fn a_cropped_sps_shows_the_size_asked_and_keeps_its_format() {
    for sps in [&SPS_420[..], &SPS_444, &SPS_444_10] {
        let head = head(sps).unwrap();
        let coded = (head.width, head.height);
        for shown in [coded, (320, 190), (300, 176), (2, 2)] {
            let cropped = crop_sps(sps, shown).unwrap();
            assert_eq!(hevc::shown_size(&cropped), Some(shown), "{shown:?}");
            assert_eq!(hevc::sample_format(&cropped), hevc::sample_format(sps));
            let again = super::head(&cropped).unwrap();
            assert_eq!(
                (again.width, again.height),
                coded,
                "the coded size is untouched"
            );
            assert_eq!(
                again.window.is_some(),
                shown != coded,
                "no window when nothing is cut"
            );
        }
    }
    // 4:4:4 counts its window in single samples, so odd sides are fine.
    let odd = crop_sps(&SPS_444, (319, 181)).unwrap();
    assert_eq!(hevc::shown_size(&odd), Some((319, 181)));
}

#[test]
fn a_window_that_cannot_be_is_refused() {
    assert_eq!(crop_sps(&SPS_420, (319, 180)), None, "an odd 4:2:0 side");
    assert_eq!(crop_sps(&SPS_420, (320, 193)), None, "taller than coded");
    assert_eq!(crop_sps(&SPS_444, (336, 180)), None, "wider than coded");
    assert_eq!(crop_sps(&SPS_420, (0, 180)), None, "nothing shown");
    assert_eq!(crop_sps(&[0x44, 0x01, 0xc0], (320, 180)), None, "a PPS");
    assert_eq!(crop_sps(&SPS_420[..20], (320, 180)), None, "cut off");
}

/// Only the SPS of a keyframe's parameter sets changes; the slices after it are copied.
#[test]
fn an_access_unit_has_its_sps_rewritten_in_place() {
    let vps = [0x40, 0x01, 0x0c];
    let pps = [0x44, 0x01, 0xc0, 0x72];
    let idr = [0x26, 0x01, 0xaf, 0x00, 0x00, 0x03, 0x01, 0x55];
    let unit = |sps: &[u8]| -> Vec<u8> { access_unit(&[&vps, sps, &pps, &idr]) };
    let mut data = unit(&SPS_420);
    crop_access_unit(&mut data, (300, 170)).unwrap();
    assert_eq!(data, unit(&crop_sps(&SPS_420, (300, 170)).unwrap()));

    let delta = access_unit(&[&idr]);
    let mut data = delta.clone();
    crop_access_unit(&mut data, (300, 170)).unwrap();
    assert_eq!(data, delta, "no parameter sets, nothing to do");

    let mut data = unit(&SPS_420);
    assert!(
        matches!(
            crop_access_unit(&mut data, (321, 170)),
            Err(CodecError::MalformedNal { .. })
        ),
        "a window that cannot be"
    );
    assert_eq!(data, unit(&SPS_420), "left as it was");
}

/// SPSs the M1 Max's low-latency encoder wrote (2026-09-29, probe P1 in `tests/chroma444.rs`):
/// Main at 3024 × 1964 and at 3024 × 1968, Main 4:4:4 10 at 3456 × 2234 and at 3456 × 2240.
const VT_3024X1964: &str = "420103016000000300b000000300000300990000a0017a2007b1f78808ee45210b9f84f42fa86f543faa823d5529b81010081fc20104";
const VT_3024X1968: &str = "420103016000000300b000000300000300990000a0017a2007b162023b914842e7e13d0bea1bd50feaa08f554a6e04040207f08041";
const VT_3456X2234_444: &str = "420103040800000300bc0800000300009900009000360400460f9db103772291973f09e85f50dea94dc0808040fe100820";
const VT_3456X2240_444: &str = "420103040800000300bc08000003000099000090003604004609b103772291973f09e85f50dea94dc0808040fe100820";

fn bytes(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

/// The padded session's SPS with its window rewritten is, bit for bit, the SPS the encoder
/// writes when it is given the true size itself, and the other way round.
#[test]
fn the_rewrite_is_the_encoders_own_sps_for_the_true_size() {
    for (native, padded, shown, coded) in [
        (VT_3024X1964, VT_3024X1968, (3024, 1964), (3024, 1968)),
        (
            VT_3456X2234_444,
            VT_3456X2240_444,
            (3456, 2234),
            (3456, 2240),
        ),
    ] {
        let (native, padded) = (bytes(native), bytes(padded));
        assert_eq!(hevc::shown_size(&native), Some(shown));
        assert_eq!(hevc::shown_size(&padded), Some(coded));
        assert_eq!(crop_sps(&padded, shown), Some(native.clone()), "{shown:?}");
        assert_eq!(crop_sps(&native, coded), Some(padded), "{coded:?}");
    }
}

/// What rewriting a keyframe's SPS costs the encoder's output thread: a 3024 × 1964 keyframe
/// of about 160 kB, its SPS rewritten and the slices behind it moved.
#[test]
#[ignore = "a measurement; run in release with --ignored --nocapture"]
fn keyframe_crop_time() {
    let sps = bytes(VT_3024X1968);
    let slices: Vec<u8> = (0..160_000_u32).map(|i| (i % 251) as u8 | 0x10).collect();
    let idr = [&[0x26, 0x01][..], &slices].concat();
    let unit = access_unit(&[&sps, &idr]);
    let rounds = 2_000_u32;
    let started = std::time::Instant::now();
    for _ in 0..rounds {
        let mut data = unit.clone();
        crop_access_unit(&mut data, (3024, 1964)).unwrap();
        std::hint::black_box(data);
    }
    let with_crop = started.elapsed() / rounds;
    let started = std::time::Instant::now();
    for _ in 0..rounds {
        std::hint::black_box(unit.clone());
    }
    let copy_only = started.elapsed() / rounds;
    eprintln!("MEASURE crop keyframe=160kB crop+copy={with_crop:?} copy={copy_only:?}");
}

/// An SPS whose syntax runs up to its last set bit, with no stop bit behind it, is refused
/// rather than rewritten into one that no longer parses: the rewrite took that bit for the stop
/// bit and dropped it (fuzz target `nal`).
#[test]
fn an_sps_without_its_stop_bit_is_not_rewritten_into_a_broken_one() {
    const SPS: [u8; 24] = [
        0x43, 0x00, 0x00, 0x00, 0x18, 0x43, 0x01, 0x0c, 0x01, 0xff, 0x00, 0x06, 0x60, 0x00, 0x00,
        0x13, 0x00, 0xb0, 0x00, 0x00, 0x3b, 0x0c, 0x01, 0x00,
    ];
    let shown = (320, 7936);
    let cropped = crop_sps(&SPS, shown);
    assert_eq!(
        cropped.as_deref().map(hevc::shown_size),
        cropped.as_ref().map(|_| Some(shown))
    );
}
