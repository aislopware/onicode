//! TabLabel.tsx: leave short labels clear and fade the last 40px only on overflow.
use gpui::prelude::*;
use gpui::{ContentMask, SharedString, TextAlign, TextRun, canvas, px};

pub(crate) fn tab_label(text: impl Into<SharedString>) -> impl IntoElement {
    let text = text.into();
    canvas(
        move |bounds, window, _| {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            let run = TextRun {
                len: text.len(),
                font: style.font(),
                color: style.color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let line = window.text_system().shape_line(
                text.clone(),
                font_size,
                std::slice::from_ref(&run),
                None,
            );
            let overflow = line.width() > bounds.size.width && bounds.size.width > px(0.);
            #[cfg(not(target_os = "macos"))]
            let line = if overflow {
                // Other backends use the same measured ramp at glyph centres.
                let runs = text
                    .char_indices()
                    .map(|(ix, ch)| {
                        let mut run = run.clone();
                        let x = (line.x_for_index(ix) + line.x_for_index(ix + ch.len_utf8())) / 2.;
                        run.len = ch.len_utf8();
                        run.color.a *= ((bounds.size.width - x) / px(40.)).clamp(0., 1.);
                        run
                    })
                    .collect::<Vec<_>>();
                window
                    .text_system()
                    .shape_line(text.clone(), font_size, &runs, None)
            } else {
                line
            };
            (line, overflow)
        },
        |bounds, (line, overflow), window, cx| {
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                let mut paint = |window: &mut gpui::Window| {
                    let _ = line.paint(
                        bounds.origin,
                        bounds.size.height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                };
                // Overflowing labels fade out over their last 40px.
                #[cfg(target_os = "macos")]
                if overflow {
                    let fade = crate::mask::mask_fade(gpui::Edges {
                        right: px(40.),
                        ..Default::default()
                    });
                    window.with_edge_fade(bounds, fade, paint);
                    return;
                }
                #[cfg(not(target_os = "macos"))]
                let _ = overflow;
                paint(window);
            });
        },
    )
    .size_full()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use gpui::{App, Context, Render, TestAppContext, Window, div};

    use super::*;
    use crate::mask::probe::{fade_widths, sprite_fades};

    struct Label {
        width: f32,
        text: &'static str,
    }
    impl Render for Label {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(self.width))
                .h(px(20.))
                .text_size(px(13.))
                .child(tab_label(self.text))
        }
    }
    #[gpui::test]
    fn short_labels_stay_clear_and_overflow_is_remeasured_after_resize(cx: &mut TestAppContext) {
        // Glyphs carry the fade they are drawn in: shape and rasterize them with CoreText.
        let cx = &mut TestAppContext::build_with_text_system(
            cx.dispatcher.clone(),
            None,
            std::sync::Arc::new(gpui_apple::AppleTextSystem::new()),
        );
        let window = cx.update(|cx: &mut App| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| Label {
                    width: 100.,
                    text: "Short",
                })
            })
            .unwrap()
        });
        let masked = |cx: &mut TestAppContext| {
            cx.refresh().unwrap();
            window
                .update(cx, |_, window, _| {
                    let scale = window.scale_factor();
                    sprite_fades(window)
                        .iter()
                        .filter(|ramps| fade_widths(ramps, scale) == [0., 0., 40., 0.])
                        .count()
                })
                .unwrap()
        };
        assert_eq!(masked(cx), 0);
        window
            .update(cx, |label, _, cx| {
                label.text = "A long session title that cannot fit in this tab";
                cx.notify();
            })
            .unwrap();
        assert_eq!(masked(cx), 1);
        window
            .update(cx, |label, _, cx| {
                label.width = 700.;
                cx.notify();
            })
            .unwrap();
        assert_eq!(masked(cx), 0);
    }
}
