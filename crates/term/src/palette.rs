//! The terminal's colours: TerminalView's `ANSI_DARK` palette for the first 16 indexes, then
//! xterm's colour cube and grey ramp. The host answers OSC 4 queries from it and every mirror
//! paints from it, so a program asking for a colour is told the one it sees.
use libghostty_vt::style::{Palette, RgbColor};

/// TerminalView's `ANSI_DARK` palette.
pub const ANSI: [u32; 16] = [
    0x1d2428, 0xf87171, 0x4ade80, 0xfbbf24, 0x60a5fa, 0xc084fc, 0x22d3ee, 0xe8eef2, 0x64748b,
    0xfca5a5, 0x86efac, 0xfde68a, 0x93c5fd, 0xd8b4fe, 0x67e8f9, 0xf8fafc,
];

/// The colour of palette index `index`, as `0xRRGGBB`.
#[must_use]
pub fn indexed_rgb(index: u8) -> u32 {
    match index {
        0..=15 => ANSI[usize::from(index)],
        16..=231 => {
            let i = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + u32::from(v) * 40 };
            (level(i / 36) << 16) | (level((i / 6) % 6) << 8) | level(i % 6)
        }
        _ => {
            let v = 8 + (u32::from(index) - 232) * 10;
            (v << 16) | (v << 8) | v
        }
    }
}

/// `0xRRGGBB` as libghostty's colour.
#[must_use]
pub fn rgb(value: u32) -> RgbColor {
    let [_, r, g, b] = value.to_be_bytes();
    RgbColor { r, g, b }
}

/// libghostty's colour as `0xRRGGBB`.
#[must_use]
pub fn hex(color: RgbColor) -> u32 {
    u32::from_be_bytes([0, color.r, color.g, color.b])
}

/// The whole 256-colour palette, for libghostty.
pub(crate) fn palette() -> Palette {
    Palette(std::array::from_fn(|index| {
        rgb(indexed_rgb(u8::try_from(index).unwrap_or(u8::MAX)))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_follow_the_dark_palette_and_xterm_cube() {
        assert_eq!(indexed_rgb(1), 0xf87171);
        assert_eq!(indexed_rgb(16), 0x000000);
        assert_eq!(indexed_rgb(196), 0xff0000);
        assert_eq!(indexed_rgb(232), 0x080808);
        assert_eq!(indexed_rgb(255), 0xeeeeee);
        assert_eq!(hex(rgb(0x4da3f5)), 0x4da3f5);
        assert_eq!(hex(palette().0[196]), 0xff0000);
    }
}
