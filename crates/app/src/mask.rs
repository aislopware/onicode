//! The masks monocode's GPUI drew with marker quads, on gpui-fast's own: edge fades shaped as its
//! CSS gradient masks ([`mask_fade`]) and a rounded clip over a whole subtree ([`rounded_clip`]).
//! A fade that follows scroll state is `gpui::edge_fade_by`, worked out from the content's layout
//! in the frame it lays out, as monocode painted its fade markers after the content.
use gpui::{
    AnyElement, App, Bounds, EdgeFade, Edges, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Window, px,
};

/// A fade as monocode's CSS gradient masks draw it: each edge with a width ramps linearly from
/// nothing at the edge to whole a width in, and where an axis' two ramps meet the lesser shows.
pub(crate) fn mask_fade(width: Edges<Pixels>) -> EdgeFade {
    EdgeFade::new(width).linear().min_combined()
}

/// [`mask_fade`] of the top and bottom edges.
pub(crate) fn mask_fade_y(top: Pixels, bottom: Pixels) -> EdgeFade {
    mask_fade(Edges {
        top,
        bottom,
        ..Edges::default()
    })
}

/// Clips `child`'s whole subtree, its own background and border included, to its bounds with
/// corners of `radius` ([`Window::with_rounded_clip`]): what it paints is cut per pixel along the
/// arcs, with an antialiased edge, so what lies under the corners shows through.
pub(crate) fn rounded_clip(radius: f32, child: impl IntoElement) -> RoundedClip {
    RoundedClip {
        radius,
        child: child.into_any_element(),
    }
}

pub(crate) struct RoundedClip {
    radius: f32,
    child: AnyElement,
}

impl IntoElement for RoundedClip {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RoundedClip {
    type PrepaintState = ();
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        // The same clip as paint: it narrows the content mask, so hitboxes past the bounds are
        // clipped as the paint is, and retention notes the clip the child is drawn in.
        window.with_rounded_clip(bounds, px(self.radius), |window| {
            self.child.prepaint(window, cx)
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_rounded_clip(bounds, px(self.radius), |window| {
            self.child.paint(window, cx)
        });
    }
}

/// What the last frame painted with the masks above, for tests.
#[cfg(test)]
pub(crate) mod probe {
    use gpui::Window;

    /// The group fades the last frame drew (`Window::with_group_fade`, an edge fade's
    /// `.group()`): each group's region and fade.
    pub(crate) fn group_fades(
        window: &Window,
    ) -> Vec<(gpui::Bounds<gpui::ScaledPixels>, gpui::GroupFade)> {
        window
            .painted_scene()
            .layer_masks()
            .iter()
            .filter(|cut| cut.begin)
            .filter_map(|cut| Some((cut.clipped_bounds(), cut.mask.fade?)))
            .collect()
    }

    /// How far in from each edge of a group fade — left, top, right, bottom — its ramp runs, in
    /// points at `scale` to a hundredth; 0 for an edge that does not fade.
    pub(crate) fn group_fade_widths(fade: &gpui::GroupFade, scale: f32) -> [f32; 4] {
        let width = fade.width;
        [width.left, width.top, width.right, width.bottom]
            .map(|width| (width / scale * 100.).round() / 100.)
    }
}
