//! Whether a stream is coded as two stripes on the two encode engines, or as one picture.
//!
//! Stripes halve the encode of a picture larger than one engine's pixel rate, and cost a scroll
//! twice the bits: scrolled text enters every coded region at its bottom edge, and two stripes
//! have two such edges (`docs/decisions/video.md`, "Two stripes halve the encode at 3K and
//! above"). So a stream is striped by its size alone: from [`STRIPED_FROM`] pixels, where one
//! engine's encode of the whole picture no longer fits the frame budget the stream aims for.
//!
//! The size is fixed rather than timed on this Mac: a timing at the stream's open coded beside
//! its first keyframe, one that waited for idle engines left a new size unstriped until it had
//! run, and a spend gate with holds rebuilt the sessions (keyframes) as the rate moved. A Mac with
//! one encode engine codes the stripes in turn, no faster than whole, and pays the scroll's
//! bits for nothing; the Macs this is built for have two.

/// The surface, in pixels, from which a stream is striped: four fifths of 3024 × 1968.
///
/// 3024 × 1968 (a 14-inch MacBook Pro's display made at Retina) codes whole in 15 ms and 1080p
/// in 6 (MEASUREMENTS.md, "stripes across the two encode engines"). The encode grows with the
/// pixels, so 12 ms, past which stripes paid in the timed gate this replaces, falls at four
/// fifths of 3024 × 1968: about 4.76 million pixels. A Retina display made at a full-screen
/// window's size on a 14- or 16-inch MacBook Pro or a 5K display is striped, as the timing
/// striped it; a 2560 × 1600 one (4.1 million) is not.
pub const STRIPED_FROM: u64 = 3024 * 1968 * 4 / 5;

/// Whether a `width` × `height` surface is coded as two stripes.
#[must_use]
pub fn striped(width: u32, height: u32) -> bool {
    u64::from(width).saturating_mul(u64::from(height)) >= STRIPED_FROM
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Retina display made full screen on a 14-inch MacBook Pro and a 5K one are striped;
    /// 2560 × 1600 and 1080p are not.
    #[test]
    fn large_surfaces_are_striped() {
        assert!(striped(3024, 1968));
        assert!(striped(5120, 2880));
        assert!(!striped(2560, 1600));
        assert!(!striped(1920, 1088));
        assert!(!striped(u32::MAX, 0));
    }
}
