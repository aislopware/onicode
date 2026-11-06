//! [`edge_faded`]: a child whose whole subtree paints fading out toward the wrapper's
//! edges, per pixel ([`gpui::EdgeFade`]). Built for glass: over a see-through blurred
//! backdrop no painted overlay can fade content out, because what is behind the window is
//! not a paintable colour.
//!
//! GPUI nests fades edge by edge, so a label fading at its right inside a list fading at
//! its top and bottom fades at all three, and a fade gated on a scroll handle eases in with
//! the content hidden past its edge.

use gpui::{
    AnyElement, App, Bounds, EdgeFade, Edges, Element, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, ScrollHandle, Window, px,
};

/// Fade the child's content at its own edges: `top`/`bottom` select which edges, `band`
/// is the ramp height in px. Horizontal edges via [`EdgeFaded::fade_left`] and
/// [`EdgeFaded::fade_right`].
pub fn edge_faded(band: f32, top: bool, bottom: bool, child: impl IntoElement) -> EdgeFaded {
    EdgeFaded {
        band,
        band_top: None,
        band_bottom: None,
        inset_top: 0.0,
        edges: Edges {
            top,
            bottom,
            left: false,
            right: false,
        },
        scroll_y: None,
        overflow_y: None,
        scroll_x: None,
        child: child.into_any_element(),
    }
}

/// Vertical overflow, `(above, below)`, for custom scrollables.
type OverflowProbe = Box<dyn Fn(&App) -> (bool, bool)>;

pub struct EdgeFaded {
    band: f32,
    band_top: Option<f32>,
    band_bottom: Option<f32>,
    inset_top: f32,
    edges: Edges<bool>,
    scroll_y: Option<ScrollHandle>,
    overflow_y: Option<OverflowProbe>,
    scroll_x: Option<ScrollHandle>,
    child: AnyElement,
}

impl EdgeFaded {
    pub fn fade_left(mut self, on: bool) -> Self {
        self.edges.left = on;
        self
    }

    pub fn fade_right(mut self, on: bool) -> Self {
        self.edges.right = on;
        self
    }

    /// The ramp height at the top edge only: content fading across chrome of different
    /// heights, a short titlebar above and a tall composer stack below.
    pub fn band_top(mut self, px: f32) -> Self {
        self.band_top = Some(px);
        self
    }

    /// [`Self::band_top`], for the bottom edge.
    pub fn band_bottom(mut self, px: f32) -> Self {
        self.band_bottom = Some(px);
        self
    }

    /// Fade the top and bottom only as deep as `handle`'s content lies hidden past them,
    /// in full once a whole band is: `top`/`bottom` become enables.
    pub fn fade_overflow_y(mut self, handle: &ScrollHandle) -> Self {
        self.scroll_y = Some(handle.clone());
        self
    }

    /// [`Self::fade_overflow_y`] for custom scrollables without a [`ScrollHandle`].
    pub fn fade_overflow_y_with(
        mut self,
        overflow: impl Fn(&App) -> (bool, bool) + 'static,
    ) -> Self {
        self.overflow_y = Some(Box::new(overflow));
        self
    }

    /// [`Self::fade_overflow_y`] for the left and right edges.
    pub fn fade_overflow_x(mut self, handle: &ScrollHandle) -> Self {
        self.scroll_x = Some(handle.clone());
        self
    }

    /// A scrolling strip: [`Self::fade_overflow_x`].
    pub fn fade_scroll_x(self, handle: &ScrollHandle) -> Self {
        self.fade_overflow_x(handle)
    }

    /// A label running past its end: its last characters dim only as it grows a band too
    /// wide, not as soon as it is a fraction too wide. [`Self::fade_overflow_x`].
    pub fn fade_label_overflow(self, handle: &ScrollHandle) -> Self {
        self.fade_overflow_x(handle)
    }

    /// Pull the top edge `px` inside the wrapper. Content past an edge fades on to nothing,
    /// so content between the wrapper's top and the inset edge, under chrome such as the
    /// titlebar's text, is gone before it can overlap it.
    pub fn inset_top(mut self, px: f32) -> Self {
        self.inset_top = px;
        self
    }

    /// The region that fades and how, or `None` when nothing fades.
    fn resolve(&self, mut bounds: Bounds<Pixels>, cx: &App) -> Option<(Bounds<Pixels>, EdgeFade)> {
        let band = |on: bool, band: Option<f32>| {
            if on {
                px(band.unwrap_or(self.band))
            } else {
                Pixels::ZERO
            }
        };
        let width = Edges {
            top: band(self.edges.top, self.band_top),
            bottom: band(self.edges.bottom, self.band_bottom),
            left: band(self.edges.left, None),
            right: band(self.edges.right, None),
        };
        if width == Edges::default() {
            return None;
        }
        let mut fade = EdgeFade::new(width);
        if let Some(scroll) = &self.scroll_y {
            let hidden = hidden_past(scroll);
            fade = fade.hidden(Edges {
                left: Pixels::MAX,
                right: Pixels::MAX,
                ..hidden
            });
        }
        if let Some(scroll) = &self.scroll_x {
            let hidden = hidden_past(scroll);
            fade = fade.hidden(Edges {
                top: Pixels::MAX,
                bottom: Pixels::MAX,
                ..hidden
            });
        }
        if let Some(overflow) = &self.overflow_y {
            let (above, below) = overflow(cx);
            let depth = |on: bool| if on { 1.0 } else { 0.0 };
            fade = fade.depth(Edges {
                top: depth(above),
                bottom: depth(below),
                left: 1.0,
                right: 1.0,
            });
        }
        let depth = fade.depth;
        let fades = |width: Pixels, depth: f32| width > Pixels::ZERO && depth > 0.0;
        if !(fades(width.top, depth.top)
            || fades(width.bottom, depth.bottom)
            || fades(width.left, depth.left)
            || fades(width.right, depth.right))
        {
            return None;
        }
        let inset = px(self.inset_top).clamp(Pixels::ZERO, bounds.size.height);
        bounds.origin.y += inset;
        bounds.size.height -= inset;
        Some((bounds, fade))
    }
}

/// Runs `f` inside `resolved`'s fade, if any.
fn faded<R>(
    resolved: Option<(Bounds<Pixels>, EdgeFade)>,
    window: &mut Window,
    f: impl FnOnce(&mut Window) -> R,
) -> R {
    #[cfg(test)]
    tests::PAINTED.with_borrow_mut(|painted| painted.push(resolved));
    match resolved {
        Some((bounds, fade)) => window.with_edge_fade(bounds, fade, f),
        None => f(window),
    }
}

/// How far `scroll`'s content runs past each edge.
fn hidden_past(scroll: &ScrollHandle) -> Edges<Pixels> {
    let (offset, max) = (scroll.offset(), scroll.max_offset());
    Edges {
        top: (-offset.y).max(Pixels::ZERO),
        bottom: (max.y + offset.y).max(Pixels::ZERO),
        left: (-offset.x).max(Pixels::ZERO),
        right: (max.x + offset.x).max(Pixels::ZERO),
    }
}

impl Element for EdgeFaded {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    // Prepaint and paint each enter the fade worked out from the scroll state they see:
    // prepaint lays the child out with last frame's, paint uses the offsets that layout
    // clamped, so content that shrank while scrolled drops its fade in the same frame.
    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let resolved = self.resolve(bounds, cx);
        faded(resolved, window, |window| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let resolved = self.resolve(bounds, cx);
        faded(resolved, window, |window| self.child.paint(window, cx));
    }
}

impl IntoElement for EdgeFaded {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::prelude::*;
    use gpui::{Context, Render, Styled, div};

    use super::*;

    type Entered = Option<(Bounds<Pixels>, EdgeFade)>;

    thread_local! {
        /// Every fade an [`EdgeFaded`] entered (or `None`), in order.
        pub(super) static PAINTED: RefCell<Vec<Entered>> = RefCell::default();
    }

    /// A whole frame: a retained view that read nothing new would replay its last paint.
    fn redraw(cx: &mut gpui::VisualTestContext) {
        cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    fn painted() -> Vec<Entered> {
        PAINTED.take()
    }

    #[gpui::test]
    fn menu_fades_follow_scroll_and_disappear_when_content_fits(cx: &mut gpui::TestAppContext) {
        struct Menu {
            rows: usize,
            scroll: ScrollHandle,
        }
        impl Render for Menu {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().w(px(300.0)).child(crate::popover::faded_menu_list(
                    &self.scroll,
                    crate::popover::menu_scroll_list("menu", &self.scroll)
                        .max_h(px(100.0))
                        .flex()
                        .flex_col()
                        .children((0..self.rows).map(|_| div().h(px(40.0)).flex_none())),
                ))
            }
        }
        let (menu, cx) = cx.add_window_view(|_, _| Menu {
            rows: 10,
            scroll: ScrollHandle::new(),
        });
        cx.run_until_parked();
        let scroll = menu.read_with(cx, |menu, _| menu.scroll.clone());
        let max = scroll.max_offset().y;
        assert!(max > px(100.0));
        for (offset, top, bottom) in [
            (px(0.0), 0.0, 1.0),
            (-max / 2.0, 1.0, 1.0),
            (-max, 1.0, 0.0),
        ] {
            scroll.set_offset(gpui::point(px(0.0), offset));
            painted();
            redraw(cx);
            let (_, fade) = painted()
                .last()
                .copied()
                .flatten()
                .expect("overflow fades the menu list");
            assert_eq!((fade.depth.top, fade.depth.bottom), (top, bottom));
            assert_eq!(fade.width.top, px(12.0));
        }
        menu.update(cx, |menu, cx| {
            menu.rows = 1;
            cx.notify();
        });
        painted();
        redraw(cx);
        assert_eq!(scroll.max_offset().y, px(0.0));
        assert_eq!(
            painted().last(),
            Some(&None),
            "shrinking content clears both fades in the same paint"
        );
    }

    #[gpui::test]
    fn bands_and_the_inset_shape_the_region(cx: &mut gpui::TestAppContext) {
        struct Fixture;
        impl Render for Fixture {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().child(
                    edge_faded(24.0, true, true, div().w(px(300.0)).h(px(100.0)))
                        .band_top(12.0)
                        .band_bottom(30.0)
                        .inset_top(7.0)
                        .fade_right(true),
                )
            }
        }
        let (_, cx) = cx.add_window_view(|_, _| Fixture);
        painted();
        redraw(cx);
        let (bounds, fade) = painted().last().copied().flatten().expect("a fade");
        assert_eq!(bounds.origin.y, px(7.0));
        assert_eq!(bounds.size.height, px(93.0));
        assert_eq!(
            fade.width,
            Edges {
                top: px(12.0),
                bottom: px(30.0),
                left: Pixels::ZERO,
                right: px(24.0),
            }
        );
        assert_eq!(fade.depth, Edges::all(1.0));
    }

    #[gpui::test]
    fn a_label_fade_eases_in_with_its_overflow(cx: &mut gpui::TestAppContext) {
        struct Label {
            width: f32,
            scroll: ScrollHandle,
        }
        impl Render for Label {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                edge_faded(
                    20.0,
                    false,
                    false,
                    div()
                        .id("label")
                        .w(px(self.width))
                        .overflow_hidden()
                        .track_scroll(&self.scroll)
                        .flex()
                        .child(div().w(px(200.0)).h(px(20.0)).flex_none()),
                )
                .fade_right(true)
                .fade_label_overflow(&self.scroll)
            }
        }
        let (label, cx) = cx.add_window_view(|_, _| Label {
            width: 240.0,
            scroll: ScrollHandle::new(),
        });
        let mut previous = 0.0;
        // Narrowing in 1 px steps: the right edge deepens without a jump, from none while
        // the label fits to full one band past it.
        for width in (170..=200).rev() {
            label.update(cx, |label, cx| {
                label.width = width as f32;
                cx.notify();
            });
            painted();
            redraw(cx);
            let depth = painted()
                .last()
                .copied()
                .flatten()
                .map_or(0.0, |(_, fade)| fade.depth.right);
            if width == 200 {
                assert_eq!(depth, 0.0, "a label that fits does not fade");
            }
            assert!(depth >= previous && depth - previous < 0.2);
            previous = depth;
        }
        assert_eq!(previous, 1.0);
    }
}
