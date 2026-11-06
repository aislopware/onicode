use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Focusable, IntoElement, KeyDownEvent, MouseButton, Render, Window, div, px,
};

#[cfg(target_os = "macos")]
use super::model::Presentation;
use super::{BrowserEvent, BrowserSurface};
use crate::theme::Theme;
use crate::{icons, surface_chrome};

fn button(
    id: &'static str,
    label: &'static str,
    glyph: &'static str,
    enabled: bool,
    theme: &Theme,
    _cx: &mut Context<BrowserSurface>,
) -> gpui::Stateful<gpui::Div> {
    // Match Files/History chrome, including focus-preserving mouse-down and
    // tooltips. Disabled controls have neither a pointer cursor nor a handler.
    crate::files::toolbar_button(id, label)
        .when(!enabled, |el| el.cursor_default().opacity(0.35))
        .child(
            icons::icon(glyph)
                .size(px(surface_chrome::ICON_SIZE))
                .text_color(theme.text_muted),
        )
}

impl BrowserSurface {
    pub(super) fn key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let address_focused = self.address.focus_handle(cx).is_focused(window);
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        if key == "escape" && self.annotating() {
            self.stop_annotation(cx);
            cx.stop_propagation();
            return;
        }
        let primary = if cfg!(target_os = "macos") {
            mods.platform
        } else {
            mods.control
        };
        if address_focused && !primary && !mods.alt {
            match key {
                "enter" if !mods.shift => self.submit(window, cx),
                "escape" => {
                    let url = self.page.url.clone().unwrap_or_default();
                    self.address.update(cx, |input, cx| input.set_text(url, cx));
                    self.address_edited = false;
                    self.validation = None;
                    window.focus(&self.focus, cx);
                }
                _ => return,
            }
        } else {
            return;
        }
        cx.stop_propagation();
    }

    fn preview_body(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let snapshot = &self.previews;
        let available = snapshot.error.is_none();
        let subtitle = if snapshot.remote {
            "Running on your device"
        } else {
            "Running locally"
        };
        let mut content = div()
            .w_full()
            .max_w(px(280.0))
            .flex_shrink_0()
            .my_auto()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .mb(px(4.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(theme.text_muted)
                    .child(subtitle),
            );
        for service in &snapshot.services {
            let url = service.url(snapshot.proxy_port);
            let row_url = url.clone();
            let border_strong = theme.border_strong;
            let label = if snapshot.remote {
                format!("{} · localhost:{}", service.device_name, service.port)
            } else {
                format!("localhost:{}", service.port)
            };
            content = content.child(
                div()
                    .id(gpui::SharedString::from(format!(
                        "preview-row-{}",
                        service.id
                    )))
                    .w_full()
                    .h(px(56.0))
                    .px(px(14.0))
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border)
                    .bg(crate::theme::ink(0.02))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .when(available, |el| {
                        el.cursor_pointer()
                            .hover(move |style| {
                                style
                                    .bg(crate::theme::ink(0.05))
                                    .border_color(border_strong)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.navigate(&row_url, window, cx)
                            }))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(13.0))
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .truncate()
                                    .child(service.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(crate::typography::ui_rems(11.0))
                                    .text_color(theme.text_muted)
                                    .truncate()
                                    .child(label),
                            ),
                    )
                    .child(
                        div()
                            .map(|el| {
                                #[cfg(feature = "browser-fixture")]
                                if snapshot
                                    .services
                                    .first()
                                    .is_some_and(|first| first.id == service.id)
                                {
                                    let position = self.fixture_preview_open.clone();
                                    return el.on_children_prepainted(move |bounds, _, _| {
                                        position.set(bounds.first().map(|bounds| bounds.center()));
                                    });
                                }
                                el
                            })
                            .id(gpui::SharedString::from(format!(
                                "open-preview-{}",
                                service.id
                            )))
                            .h(px(28.0))
                            .px(px(6.0))
                            .flex_shrink_0()
                            .rounded(px(6.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text_muted)
                            .role(gpui::Role::Button)
                            .aria_label(format!("Open {} preview", service.name))
                            .when(available, |el| {
                                el.cursor_pointer()
                                    .hover(|style| style.bg(crate::theme::ink(0.05)))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.navigate(&url, window, cx)
                                    }))
                            })
                            .when(!available, |el| el.opacity(0.4))
                            .child("Open"),
                    ),
            );
        }
        if snapshot.services.is_empty() {
            let message = if self.previews_loading {
                "Looking for dev servers…"
            } else if snapshot.remote {
                "Start a dev server in this project on your other device. Its preview will appear here when that device is online."
            } else {
                "Start a dev server in this project. It will appear here automatically, ready to open."
            };
            content = content.child(
                div()
                    .p(px(16.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme.border)
                    .text_size(crate::typography::ui_rems(12.0))
                    .line_height(px(19.0))
                    .text_color(theme.text_muted)
                    .child(message),
            );
        }
        if let Some(error) = &snapshot.error {
            content = content.child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .line_height(px(17.0))
                    .text_color(theme.text_muted)
                    .child(error.clone()),
            );
        }
        content = content.child(
            div()
                .id("preview-enter-address")
                .mt(px(8.0))
                .text_size(crate::typography::ui_rems(11.0))
                .text_color(theme.text_muted)
                .cursor_pointer()
                .role(gpui::Role::Button)
                .aria_label("Enter a website address")
                .on_click(cx.listener(|this, _, window, cx| this.focus_address(window, cx)))
                .child("Or enter a website address"),
        );
        div()
            .id("browser-previews")
            .size_full()
            .overflow_y_scroll()
            .p(px(16.0))
            .flex()
            .flex_col()
            .items_center()
            .child(content)
            .into_any_element()
    }

    fn empty_body(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        if self.page.url.is_none() && self.previews_task.is_some() {
            return self.preview_body(theme, cx);
        }
        let external = !cfg!(target_os = "macos");
        let has_error = self.page.error.is_some();
        let title = if has_error {
            "Couldn’t load this page"
        } else if external && self.page.url.is_some() {
            "Opened in your browser"
        } else {
            "Preview your work"
        };
        let description = if let Some(error) = &self.page.error {
            error.clone()
        } else if external {
            "Open a website or local app in your default browser. Embedded browsing is available on macOS.".into()
        } else {
            "Preview your local app or keep a website beside your conversation.".into()
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(300.0))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .size(px(44.0))
                            .rounded(px(12.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.surface_raised.opacity(0.5))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                icons::icon(icons::GLOBE)
                                    .size(px(22.0))
                                    .text_color(theme.text_muted),
                            ),
                    )
                    .child(
                        div()
                            .mt(px(4.0))
                            .text_size(crate::typography::ui_rems(14.0))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_center()
                            .text_size(crate::typography::ui_rems(12.0))
                            .line_height(px(19.0))
                            .text_color(theme.text_muted)
                            .child(description),
                    )
                    .child(
                        div()
                            .mt(px(6.0))
                            .id("browser-empty-action")
                            .h(px(28.0))
                            .px(px(10.0))
                            .rounded(px(6.0))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.surface_raised)
                            .cursor_pointer()
                            .role(gpui::Role::Button)
                            .aria_label(if has_error {
                                "Retry page"
                            } else {
                                "Enter an address"
                            })
                            .hover(|style| style.bg(crate::theme::wash(0.10)))
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(crate::typography::ui_rems(12.0))
                            .text_color(theme.text)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if has_error {
                                    this.reload(cx);
                                } else {
                                    this.focus_address(window, cx);
                                }
                            }))
                            .child(if has_error {
                                "Try again"
                            } else {
                                "Enter an address"
                            })
                            .when(!has_error, |el| {
                                el.child(
                                    div()
                                        .text_size(crate::typography::ui_rems(10.0))
                                        .text_color(theme.text_faint)
                                        .child(if cfg!(target_os = "macos") {
                                            "⌘L"
                                        } else {
                                            "Ctrl L"
                                        }),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }
}

impl Render for BrowserSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).clone();
        let focused = self.address.focus_handle(cx).is_focused(window);
        let external = !cfg!(target_os = "macos");
        let has_page = self.page.url.is_some();
        let back = button(
            "browser-back",
            "Back",
            icons::ARROW_LEFT,
            self.page.can_back,
            &theme,
            cx,
        )
        .when(self.page.can_back, |el| {
            el.on_click(cx.listener(|this, _, _, _| this.history(false)))
        });
        let forward = button(
            "browser-forward",
            "Forward",
            icons::ARROW_RIGHT,
            self.page.can_forward,
            &theme,
            cx,
        )
        .when(self.page.can_forward, |el| {
            el.on_click(cx.listener(|this, _, _, _| this.history(true)))
        });
        let reload = button(
            "browser-reload",
            "Reload page",
            icons::REFRESH,
            has_page,
            &theme,
            cx,
        )
        .when(has_page, |el| {
            el.on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
        });
        let address = surface_chrome::input()
            .id("browser-address")
            .when(self.validation.is_some(), |el| {
                el.border_1().border_color(theme.danger)
            })
            .child(
                icons::icon(icons::GLOBE)
                    .size(px(12.0))
                    .flex_none()
                    .text_color(theme.text_faint),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(16.0))
                    .overflow_hidden()
                    .child(self.address.clone()),
            )
            .when(focused || self.address_edited, |el| {
                el.child(
                    div()
                        .id("browser-go")
                        .size(px(18.0))
                        .flex_none()
                        .rounded(px(4.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .role(gpui::Role::Button)
                        .aria_label("Go to address")
                        .hover(|s| s.bg(crate::theme::wash(0.10)))
                        .on_mouse_down(MouseButton::Left, |_, w, _| w.prevent_default())
                        .on_click(cx.listener(|this, _, w, cx| this.submit(w, cx)))
                        .tooltip(crate::settings::widgets::text_tooltip("Go to address"))
                        .child(
                            icons::icon(icons::RETURN)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        ),
                )
            });
        let open = button(
            "browser-external",
            "Open in default browser",
            icons::ARROW_UP_RIGHT,
            has_page,
            &theme,
            cx,
        )
        .when(has_page, |el| {
            el.on_click(cx.listener(|this, _, _, cx| this.open_external(cx)))
        });
        // Annotation picks an element on the page into the composer.
        let annotating = self.annotating();
        let can_annotate = self.can_annotate();
        let annotate = crate::files::toolbar_button(
            "browser-annotate",
            if annotating {
                "Stop annotating"
            } else {
                "Annotate an element"
            },
        )
        .when(!can_annotate, |el| el.cursor_default().opacity(0.35))
        .when(annotating, |el| el.bg(crate::theme::wash(0.10)))
        .child(
            icons::icon(icons::CURSOR_SQUARE)
                .size(px(surface_chrome::ICON_SIZE))
                .text_color(if annotating {
                    theme.accent
                } else {
                    theme.text_muted
                }),
        )
        .when(can_annotate, |el| {
            el.on_click(cx.listener(|this, _, _, cx| this.toggle_annotation(cx)))
        });
        let toolbar = surface_chrome::toolbar(&theme)
            .when(!external, |el| el.child(back).child(forward).child(reload))
            .child(address)
            .when(!external, |el| el.child(annotate))
            .child(open);

        let body = div()
            .id("browser-page")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            // Empty tabs share the shell's glass, like the sidebar tab picker.
            // Keep an opaque backing while native web content is loading.
            .when(has_page, |body| body.bg(theme.bg));
        // The page sits under GPUI's layer in a hole `native_view` cuts; what
        // GPUI paints after it (menus, tooltips, the drag preview) lands on top,
        // and GPUI's hit test decides where the page takes the pointer.
        #[cfg(target_os = "macos")]
        let body = match &self.native {
            Some(native) if self.page.error.is_none() => {
                if self.presentation == Presentation::Hidden {
                    body
                } else {
                    body.child(
                        gpui::composition::native_view(native.host())
                            .id("browser-native")
                            .absolute()
                            .inset_0()
                            .track_focus(&self.page_focus)
                            // A press on GPUI content takes the keyboard back.
                            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                                if this.page_focus.is_focused(window) {
                                    window.focus(&this.focus, cx);
                                }
                            }))
                            // GPUI owns a drag: keep the pointer off the page.
                            .when(self.presentation == Presentation::Passthrough, |page| {
                                page.child(div().absolute().inset_0().occlude())
                            }),
                    )
                }
            }
            _ => body.child(self.empty_body(&theme, cx)),
        };
        #[cfg(not(target_os = "macos"))]
        let body = body.child(self.empty_body(&theme, cx));

        let remote_loopback = self.remote
            && self
                .page
                .url
                .as_deref()
                .and_then(|s| url::Url::parse(s).ok())
                .is_some_and(|u| {
                    // (A preview's `*.localhost` name goes through the proxy,
                    // on whichever port it got.)
                    super::model::loopback(&u)
                        && !(u.port().is_some()
                            && u.host_str()
                                .is_some_and(|host| host.ends_with(".localhost")))
                });
        div().id("browser-surface").size_full().flex().flex_col().track_focus(&self.focus)
            .key_context("Browser").on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(|this, _: &super::Reload, _, cx| this.reload(cx)))
            .on_action(cx.listener(|this, _: &super::FocusAddress, w, cx| this.focus_address(w, cx)))
            .on_action(cx.listener(|_, _: &super::NewTab, _, cx| cx.emit(BrowserEvent::NewTab(None))))
            .on_action(cx.listener(|_, _: &super::CloseTab, _, cx| cx.emit(BrowserEvent::Close)))
            .on_action(cx.listener(|this, _: &super::Back, _, _| this.history(false)))
            .on_action(cx.listener(|this, _: &super::Forward, _, _| this.history(true)))
            // Mouse side buttons drive the shell's route history everywhere
            // else; over the browser they are inert.
            .on_mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Back), |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Navigate(gpui::NavigationDirection::Forward), |_, _, cx| cx.stop_propagation())
            .child(toolbar)
            .when_some(self.validation.clone(), |el, message| el.child(div().px(px(12.0)).py(px(8.0)).text_size(crate::typography::ui_rems(11.0)).text_color(theme.danger).child(message)))
            .when(remote_loopback, |el| el.child(div().px(px(12.0)).py(px(8.0)).border_b_1().border_color(theme.border)
                .text_size(crate::typography::ui_rems(11.0)).text_color(theme.text_muted)
                .child("Localhost opens on this device. Open a detected preview from a new tab to reach your other device.")))
            .child(body)
            .when(external, |el| el.child(div().h(px(26.0)).px(px(10.0)).flex().items_center().gap(px(5.0)).border_t_1().border_color(theme.border)
                .text_size(crate::typography::ui_rems(10.0)).text_color(theme.text_faint)
                .child(icons::icon(icons::ARROW_UP_RIGHT).size(px(11.0))).child("Opens in your default browser")))
    }
}
