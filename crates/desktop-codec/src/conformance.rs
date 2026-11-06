//! The HEVC conformance window: a picture coded at a padded size, shown at its true one.
//!
//! The low-latency encoder queues any picture whose sides are not both multiples of 16, behind
//! a padding copy of its own (`docs/decisions/video.md`, "Stream sides padded to 16"), so the
//! worker captures into a padded surface with the picture at its top-left and codes that. The SPS
//! then says which part is the picture: `conformance_window_flag` and four offsets in chroma sample
//! units (H.265 7.3.2.2, 7.4.3.2). Every decoder crops by it, so the client's buffers come out at
//! the true size and nothing past the worker knows about the padding. This module reads an SPS far
//! enough to find the window, writes the new one, and copies every other bit as it was.

use crate::CodecError;
use crate::nal::{self, hevc};

/// What an SPS says up to and including its conformance window (H.265 7.3.2.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Head {
    /// 0 monochrome, 1 4:2:0, 2 4:2:2, 3 4:4:4.
    pub chroma_format_idc: u32,
    /// 4:4:4 coded as three monochrome planes.
    pub separate_colour_plane: bool,
    /// `pic_width_in_luma_samples`, the coded width.
    pub width: u32,
    /// `pic_height_in_luma_samples`, the coded height.
    pub height: u32,
    /// `[left, right, top, bottom]` in chroma units, `None` without a window.
    pub window: Option<[u32; 4]>,
    /// Bits per luma sample.
    pub bit_depth: u32,
    /// RBSP bit where `conformance_window_flag` sits.
    pub window_at: usize,
    /// RBSP bit just past the window's offsets (past the flag when there is none).
    pub after_window: usize,
}

impl Head {
    /// Luma samples per unit of a window offset, `(SubWidthC, SubHeightC)` (H.265 table 6-1).
    pub(crate) const fn units(&self) -> (u32, u32) {
        match (self.chroma_format_idc, self.separate_colour_plane) {
            (1, _) => (2, 2),
            (2, _) => (2, 1),
            _ => (1, 1),
        }
    }

    /// The size a decoder outputs: the coded size less the window.
    pub(crate) fn shown(&self) -> Option<(u32, u32)> {
        let [left, right, top, bottom] = self.window.unwrap_or_default();
        let (sw, sh) = self.units();
        let across = left.checked_add(right)?.checked_mul(sw)?;
        let down = top.checked_add(bottom)?.checked_mul(sh)?;
        Some((
            self.width.checked_sub(across)?,
            self.height.checked_sub(down)?,
        ))
    }
}

/// Parse an SPS NAL unit (header included) up to its bit depth.
pub(crate) fn head(nal: &[u8]) -> Option<Head> {
    if hevc::nal_type(nal)? != hevc::SPS {
        return None;
    }
    head_of(&unescape(nal.get(2..)?))
}

fn head_of(rbsp: &[u8]) -> Option<Head> {
    let mut bits = Reader { bytes: rbsp, at: 0 };
    // sps_video_parameter_set_id u(4), sps_max_sub_layers_minus1 u(3), nesting flag u(1).
    bits.skip(4)?;
    let sub_layers = usize::try_from(bits.read(3)?).ok()?;
    bits.skip(1)?;
    // profile_tier_level: the general profile and level, 88 + 8 bits (H.265 7.3.3).
    bits.skip(96)?;
    let mut present = [(false, false); 7];
    for layer in present.iter_mut().take(sub_layers) {
        *layer = (bits.flag()?, bits.flag()?);
    }
    if sub_layers > 0 {
        bits.skip(2_usize.checked_mul(8_usize.checked_sub(sub_layers)?)?)?;
    }
    for &(profile, level) in present.iter().take(sub_layers) {
        if profile {
            bits.skip(88)?;
        }
        if level {
            bits.skip(8)?;
        }
    }
    bits.ue()?; // sps_seq_parameter_set_id
    let chroma_format_idc = bits.ue()?;
    let separate_colour_plane = chroma_format_idc == 3 && bits.flag()?;
    let width = bits.ue()?;
    let height = bits.ue()?;
    let window_at = bits.at;
    let window = if bits.flag()? {
        Some([bits.ue()?, bits.ue()?, bits.ue()?, bits.ue()?])
    } else {
        None
    };
    let after_window = bits.at;
    let bit_depth = bits.ue()?.checked_add(8)?;
    Some(Head {
        chroma_format_idc,
        separate_colour_plane,
        width,
        height,
        window,
        bit_depth,
        window_at,
        after_window,
    })
}

/// `sps` (an SPS NAL unit, header included) with its conformance window set so a decoder
/// outputs the top-left `shown` of the coded picture; every other bit is copied as it was.
///
/// `None` when `sps` is not a well-formed SPS, or `shown` is larger than the coded picture or
/// not a whole number of chroma units short of it (an odd side of a 4:2:0 picture), or the
/// rewrite does not read back as showing `shown`.
#[must_use]
pub fn crop_sps(sps: &[u8], shown: (u32, u32)) -> Option<Vec<u8>> {
    if hevc::nal_type(sps)? != hevc::SPS {
        return None;
    }
    let header = sps.get(..2)?;
    let rbsp = unescape(sps.get(2..)?);
    let head = head_of(&rbsp)?;
    let (sw, sh) = head.units();
    let right = offset(head.width, shown.0, sw)?;
    let bottom = offset(head.height, shown.1, sh)?;
    // The rbsp_stop_one_bit: everything before it is syntax, everything after is alignment.
    let last = rbsp.iter().rposition(|&b| b != 0)?;
    let stop = last
        .checked_mul(8)?
        .checked_add(7)?
        .checked_sub(usize::try_from(rbsp.get(last)?.trailing_zeros()).ok()?)?;
    let mut out = Writer::default();
    out.copy(&rbsp, 0, head.window_at)?;
    if right == 0 && bottom == 0 {
        out.bit(false);
    } else {
        out.bit(true);
        for v in [0, right, 0, bottom] {
            out.ue(v)?;
        }
    }
    out.copy(
        &rbsp,
        head.after_window,
        stop.checked_add(1)?.checked_sub(head.after_window)?,
    )?;
    let mut nal = header.to_vec();
    escape_into(&out.finish(), &mut nal);
    // The head is all of the syntax parsed here, so a last set bit that is syntax rather than
    // the stop bit (a malformed SPS) was dropped above: the rewrite must read back as asked.
    (self::head(&nal)?.shown() == Some(shown)).then_some(nal)
}

/// Offset units that crop `coded` to `shown`, `unit` luma samples each.
fn offset(coded: u32, shown: u32, unit: u32) -> Option<u32> {
    let cut = coded.checked_sub(shown)?;
    (cut.checked_rem(unit)? == 0 && shown > 0)
        .then(|| cut.checked_div(unit))
        .flatten()
}

/// Rewrite, in place, every SPS among the parameter sets that open an access unit, with its
/// length ([`crate::nal`]).
///
/// Each then shows the top-left `shown` of the coded pictures ([`crop_sps`]). A unit with no
/// parameter sets is left as it is.
///
/// # Errors
///
/// An SPS that cannot be cropped to `shown`; the unit is left as it was.
pub fn crop_access_unit(data: &mut Vec<u8>, shown: (u32, u32)) -> Result<(), CodecError> {
    let base = data.as_ptr().addr();
    let mut spans = Vec::new();
    for nal in nal::units(data).take_while(|nal| hevc::is_parameter_set(nal)) {
        if hevc::nal_type(nal) != Some(hevc::SPS) {
            continue;
        }
        let at = nal.as_ptr().addr().wrapping_sub(base);
        let malformed = CodecError::MalformedNal { offset: at };
        let cropped = crop_sps(nal, shown).ok_or(malformed)?;
        let mut unit = Vec::with_capacity(cropped.len().saturating_add(4));
        nal::push(&mut unit, &cropped)?;
        let start = at.checked_sub(4).ok_or(malformed)?;
        let end = at.checked_add(nal.len()).ok_or(malformed)?;
        spans.push((start..end, unit));
    }
    for (span, unit) in spans.into_iter().rev() {
        data.splice(span, unit);
    }
    Ok(())
}

/// The RBSP of an escaped NAL payload: each `00 00 03` loses its `03` (H.265 7.4.2).
fn unescape(escaped: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(escaped.len());
    let mut zeros = 0_u8;
    for &b in escaped {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros.saturating_add(1) } else { 0 };
        out.push(b);
    }
    out
}

/// Escape an RBSP onto `out`: a `03` after any two zero bytes that a byte of 0–3 follows, so
/// no start code can appear inside the unit (H.265 7.4.2).
fn escape_into(rbsp: &[u8], out: &mut Vec<u8>) {
    let mut zeros = 0_u8;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros.saturating_add(1) } else { 0 };
        out.push(b);
    }
}

/// An RBSP read bit by bit, most significant first.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn flag(&mut self) -> Option<bool> {
        let bit = bit_at(self.bytes, self.at)?;
        self.at = self.at.checked_add(1)?;
        Some(bit)
    }

    fn read(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0_u32, |acc, _| {
            Some(acc.checked_shl(1)? | u32::from(self.flag()?))
        })
    }

    fn skip(&mut self, n: usize) -> Option<()> {
        let to = self.at.checked_add(n)?;
        (to <= self.bytes.len().checked_mul(8)?).then(|| self.at = to)
    }

    /// An unsigned Exp-Golomb code, H.265 9.2.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0_u32;
        while !self.flag()? {
            zeros = zeros.checked_add(1).filter(|z| *z < 32)?;
        }
        1_u32
            .checked_shl(zeros)?
            .checked_sub(1)?
            .checked_add(self.read(zeros)?)
    }
}

fn bit_at(bytes: &[u8], at: usize) -> Option<bool> {
    let byte = bytes.get(at.checked_div(8)?)?;
    let shift = 7_u32.checked_sub(u32::try_from(at.checked_rem(8)?).ok()?)?;
    Some(byte.checked_shr(shift)? & 1 == 1)
}

/// An RBSP written bit by bit, most significant first.
#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
    /// Bits used in the last byte, 0 when it is full or there is none.
    used: u32,
}

impl Writer {
    fn bit(&mut self, set: bool) {
        if self.used == 0 {
            self.bytes.push(0);
        }
        if set && let Some(last) = self.bytes.last_mut() {
            *last |= 0x80_u8.checked_shr(self.used).unwrap_or(0);
        }
        self.used = self.used.wrapping_add(1) & 7;
    }

    /// An unsigned Exp-Golomb code, H.265 9.2.
    fn ue(&mut self, v: u32) -> Option<()> {
        let code = u64::from(v).checked_add(1)?;
        let len = 64_u32.checked_sub(code.leading_zeros())?;
        for _ in 1..len {
            self.bit(false);
        }
        for i in (0..len).rev() {
            self.bit(code.checked_shr(i)? & 1 == 1);
        }
        Some(())
    }

    /// Copy `n` bits of `from` starting at bit `at`.
    fn copy(&mut self, from: &[u8], at: usize, n: usize) -> Option<()> {
        for i in at..at.checked_add(n)? {
            self.bit(bit_at(from, i)?);
        }
        Some(())
    }

    /// The bytes, the last one padded with zero bits.
    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests;
