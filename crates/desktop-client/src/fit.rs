//! Where a stream's picture goes in a view, and a point's way back to the stream.
//!
//! The picture keeps its aspect: it fills the view along one axis and is centred along the
//! other, the rest left as bars (letterbox, or pillarbox). Input goes the other way: a point in
//! the view becomes stream pixels, the unit `ScreenInput` speaks. Pure arithmetic, shared by the
//! macOS view and iOS.

/// A picture placed in a view.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Fit {
    /// The picture's rectangle in the view: `x`, `y`, `width`, `height`, in view units.
    pub rect: Rect,
    /// The stream's size in pixels.
    pub stream: (f32, f32),
    /// View units per stream pixel.
    pub scale: f32,
}

/// A rectangle in view units, origin top left.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// Whether `(x, y)` is inside, edges included.
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x <= self.x + self.width && y <= self.y + self.height
    }
}

impl Fit {
    /// A stream `stream` pixels in size in a view `view` units in size; `None` while either has
    /// no area (a view not laid out yet, a stream not opened).
    #[must_use]
    pub fn new(view: (f32, f32), stream: (u32, u32)) -> Option<Self> {
        let stream = (pixels(stream.0), pixels(stream.1));
        let usable = |v: f32| v.is_finite() && v > 0.0;
        if !(usable(view.0) && usable(view.1) && usable(stream.0) && usable(stream.1)) {
            return None;
        }
        let scale = (view.0 / stream.0).min(view.1 / stream.1);
        let (width, height) = (stream.0 * scale, stream.1 * scale);
        let rect = Rect {
            x: (view.0 - width) / 2.0,
            y: (view.1 - height) / 2.0,
            width,
            height,
        };
        Some(Self {
            rect,
            stream,
            scale,
        })
    }

    /// The stream pixel under view point `(x, y)`; `None` on the bars.
    #[must_use]
    pub fn to_stream(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        self.rect
            .contains(x, y)
            .then(|| self.to_stream_clamped(x, y))
    }

    /// The stream pixel nearest view point `(x, y)`: a drag that leaves the picture keeps
    /// pressing at its edge.
    #[must_use]
    pub fn to_stream_clamped(&self, x: f32, y: f32) -> (f32, f32) {
        let sx = (x - self.rect.x) / self.scale;
        let sy = (y - self.rect.y) / self.scale;
        self.clamp(sx, sy)
    }

    /// Stream pixel `(x, y)`'s point in the view.
    #[must_use]
    pub fn to_view(&self, x: f32, y: f32) -> (f32, f32) {
        (self.rect.x + x * self.scale, self.rect.y + y * self.scale)
    }

    /// A move of `(dx, dy)` view units as stream pixels: a trackpad-style finger moves the
    /// pointer as far on the Mac as it moved on the glass.
    #[must_use]
    pub fn delta_to_stream(&self, dx: f32, dy: f32) -> (f32, f32) {
        (dx / self.scale, dy / self.scale)
    }

    /// `(x, y)` kept inside the stream, edges included.
    #[must_use]
    pub fn clamp(&self, x: f32, y: f32) -> (f32, f32) {
        let fix = |v: f32, max: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, max) };
        (fix(x, self.stream.0), fix(y, self.stream.1))
    }
}

/// A pixel count or coordinate as a float: exact for anything a display has (under 2^16).
#[must_use]
pub fn pixels(v: u32) -> f32 {
    u16::try_from(v).map_or(f32::from(u16::MAX), f32::from)
}

/// A signed pixel coordinate as a float, held to what a display has.
#[must_use]
pub fn coordinate(v: i32) -> f32 {
    i16::try_from(v).map_or(
        if v < 0 {
            f32::from(i16::MIN)
        } else {
            f32::from(i16::MAX)
        },
        f32::from,
    )
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn close(a: f32, b: f32, scale: f32) -> bool {
        (a - b).abs() <= 1e-3 * scale.max(1.0) * a.abs().max(b.abs()).max(1.0)
    }

    proptest! {
        /// The picture keeps its aspect, stays inside the view, fills it along one axis and is
        /// centred along the other.
        #[test]
        fn the_picture_fills_one_axis_and_is_centred(
            vw in 1.0_f32..4000.0, vh in 1.0_f32..4000.0,
            sw in 1_u32..8000, sh in 1_u32..8000,
        ) {
            let fit = Fit::new((vw, vh), (sw, sh)).unwrap();
            let r = fit.rect;
            let (sw, sh) = (pixels(sw), pixels(sh));
            prop_assert!(close(r.width / r.height, sw / sh, 1.0));
            prop_assert!(r.x >= -1e-3 && r.y >= -1e-3);
            prop_assert!(r.x + r.width <= vw + 1e-2 && r.y + r.height <= vh + 1e-2);
            prop_assert!(close(r.width, vw, 1.0) || close(r.height, vh, 1.0));
            prop_assert!(close(r.x * 2.0 + r.width, vw, 1.0));
            prop_assert!(close(r.y * 2.0 + r.height, vh, 1.0));
        }

        /// A point on the picture goes to the stream and back to itself; a point on the bars
        /// maps to nothing, and clamped it lands on the picture's edge.
        #[test]
        fn a_point_round_trips_and_the_bars_map_to_nothing(
            vw in 10.0_f32..4000.0, vh in 10.0_f32..4000.0,
            sw in 16_u32..8000, sh in 16_u32..8000,
            fx in 0.0_f32..=1.0, fy in 0.0_f32..=1.0,
        ) {
            let fit = Fit::new((vw, vh), (sw, sh)).unwrap();
            let (x, y) = (fit.rect.x + fx * fit.rect.width, fit.rect.y + fy * fit.rect.height);
            let (sx, sy) = fit.to_stream(x, y).unwrap();
            let (bx, by) = fit.to_view(sx, sy);
            prop_assert!(close(bx, x, 1.0) && close(by, y, 1.0), "{x},{y} -> {sx},{sy} -> {bx},{by}");
            let (cx, cy) = fit.to_stream_clamped(-5.0, vh + 5.0);
            prop_assert!(cx.abs() < f32::EPSILON);
            prop_assert!(cy >= 0.0 && cy <= fit.stream.1);
            if fit.rect.x > 1.0 {
                prop_assert!(fit.to_stream(fit.rect.x / 2.0, vh / 2.0).is_none());
            }
        }

        /// A move scaled to the stream and back is the move.
        #[test]
        fn a_delta_scales_by_the_fit(
            vw in 10.0_f32..4000.0, vh in 10.0_f32..4000.0,
            sw in 16_u32..8000, sh in 16_u32..8000,
            dx in -500.0_f32..500.0, dy in -500.0_f32..500.0,
        ) {
            let fit = Fit::new((vw, vh), (sw, sh)).unwrap();
            let (sx, sy) = fit.delta_to_stream(dx, dy);
            prop_assert!(close(sx * fit.scale, dx, 1.0) && close(sy * fit.scale, dy, 1.0));
        }

        /// Whatever goes in, a clamped point is inside the stream.
        #[test]
        fn a_clamped_point_is_always_inside(x in any::<f32>(), y in any::<f32>()) {
            let fit = Fit::new((800.0, 600.0), (1920, 1080)).unwrap();
            let (cx, cy) = fit.clamp(x, y);
            prop_assert!((0.0..=1920.0).contains(&cx) && (0.0..=1080.0).contains(&cy));
        }
    }

    #[test]
    fn nothing_fits_where_there_is_no_area() {
        assert_eq!(Fit::new((0.0, 600.0), (1920, 1080)), None);
        assert_eq!(Fit::new((800.0, 600.0), (0, 1080)), None);
        assert_eq!(Fit::new((f32::NAN, 600.0), (1920, 1080)), None);
    }

    #[test]
    fn a_wide_stream_in_a_tall_view_is_letterboxed() {
        let fit = Fit::new((1000.0, 1000.0), (2000, 1000)).unwrap();
        assert_eq!(
            fit.rect,
            Rect {
                x: 0.0,
                y: 250.0,
                width: 1000.0,
                height: 500.0
            }
        );
        assert_eq!(fit.to_stream(500.0, 500.0), Some((1000.0, 500.0)));
        assert_eq!(fit.to_stream(500.0, 100.0), None, "on the bar");
    }
}
