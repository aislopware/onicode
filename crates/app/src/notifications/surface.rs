//! Stationary notification glass, matching GlassBackdrop and the native popover.
use gpui::prelude::*;
use gpui::{Div, canvas, div, px};

use crate::theme::*;

pub(crate) fn backdrop(radius: f32) -> impl IntoElement {
    let (light, opaque) = (is_light(), floating_fill());
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let quad = if light {
                gpui::fill(bounds, opaque)
            } else {
                window.paint_backdrop_blur_whole(bounds, gpui::Corners::all(px(radius)), px(24.));
                gpui::fill(bounds, content(0.02))
            };
            window.paint_quad(quad.corner_radii(px(radius)));
        },
    )
    .absolute()
    .size_full()
}
pub(crate) fn glass_frame() -> Div {
    div()
        .relative()
        .w(px(340.))
        .max_w_full()
        .overflow_hidden()
        .rounded(px(12.))
        .border_1()
        .border_color(content(0.1))
        .text_color(content(1.))
        .shadow_lg()
        .child(backdrop(12.))
}

/// Shared shell for the compact cards in the top-right notification stack.
pub(crate) fn toast_frame() -> Div {
    glass_frame()
        .w_full()
        .occlude()
        .border_dashed()
        .border_color(content(0.2))
        .bg(content(0.1))
}
