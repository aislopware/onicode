//! Ordered dithering for the empty session's ink field: a tone in [0, 1]
//! becomes a share of inked 3px cells through a 4×4 Bayer matrix, so the
//! smoke reads as halftone texture instead of flat translucent colour.
use gpui::{Bounds, BoxShadow, Corners, Hsla, Pixels, Point, Window, fill, point, px, size};

use crate::theme::content;

/// Cell side in logical pixels.
const CELL: f32 = 3.;
const BAYER4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// Whether the cell at (col, row) is inked for `tone`. Thresholds sit
/// halfway between steps, so a tone of 0.25 inks exactly a quarter.
pub(crate) fn inked(col: i32, row: i32, tone: f32) -> bool {
    tone > (BAYER4[((row & 3) * 4 + (col & 3)) as usize] as f32 + 0.5) / 16.
}

/// Whether the interface draws dark ink on a light surface. Glows turn
/// muddy there, so they are softer.
pub(crate) fn light() -> bool {
    content(1.).l < 0.5
}

/// A grid of cells from a pixel-snapped origin.
pub(crate) struct Grid {
    origin: Point<Pixels>,
    cols: i32,
    rows: i32,
}
impl Grid {
    pub fn new(origin: Point<Pixels>, width: f32, height: f32) -> Self {
        Self {
            origin: point(px(origin.x.as_f32().round()), px(origin.y.as_f32().round())),
            cols: (width / CELL).floor().max(0.) as i32,
            rows: (height / CELL).floor().max(0.) as i32,
        }
    }

    /// The centre of a cell, relative to the origin.
    pub fn center(col: i32, row: i32) -> (f32, f32) {
        ((col as f32 + 0.5) * CELL, (row as f32 + 0.5) * CELL)
    }

    /// Paints each cell `ink` colours, one quad per horizontal run of a colour.
    pub fn paint(&self, window: &mut Window, mut ink: impl FnMut(i32, i32) -> Option<Hsla>) {
        for row in 0..self.rows {
            let mut run: Option<(i32, Hsla)> = None;
            for col in 0..=self.cols {
                let next = if col < self.cols { ink(col, row) } else { None };
                if let (Some((_, a)), Some(b)) = (run, next)
                    && a == b
                {
                    continue;
                }
                if let Some((start, color)) = run {
                    window.paint_quad(fill(
                        Bounds::new(
                            self.origin + point(px(start as f32 * CELL), px(row as f32 * CELL)),
                            size(px((col - start) as f32 * CELL), px(CELL)),
                        ),
                        color,
                    ));
                }
                run = next.map(|c| (col, c));
            }
        }
    }
}

/// A soft bloom over `bounds`, inside as well as around it, so it shows
/// through the gaps of a dithered fill painted on top.
pub(crate) fn glow(window: &mut Window, bounds: Bounds<Pixels>, color: Hsla, strength: f32) {
    let mut color = color;
    color.a = strength * if light() { 0.4 } else { 1. };
    // A drop shadow skips its own element; shrinking the element to a line
    // and spreading back out covers the whole rectangle.
    let spread = bounds.size.width.min(bounds.size.height) / 2.;
    window.paint_drop_shadows(
        Bounds::new(
            bounds.origin + point(spread, spread),
            size(
                bounds.size.width - spread * 2.,
                bounds.size.height - spread * 2.,
            ),
        ),
        Corners::default(),
        &[BoxShadow {
            color,
            offset: point(px(0.), px(0.)),
            blur_radius: px(14.),
            spread_radius: spread,
            inset: false,
        }],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_ink_their_share_of_a_tile() {
        for steps in 0..=16 {
            let tone = steps as f32 / 16.;
            let count = (0..4)
                .flat_map(|y| (0..4).map(move |x| (x, y)))
                .filter(|&(x, y)| inked(x, y, tone))
                .count();
            assert_eq!(count, steps);
        }
    }
}
