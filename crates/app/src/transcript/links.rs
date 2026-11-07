//! AgentMarkdown's file-link actions, with browser actions for web links.
use std::path::Path;

use gpui::{AnyElement, Entity, Point, Subscription};
use onicode_protocol::files;

use super::*;
use crate::popover::{Menu, MenuAction, MenuEvent, MenuItem, menu_popover};

#[derive(Default)]
pub(super) struct State {
    pub(super) open: Option<OpenMenu>,
    error: Option<String>,
}
pub(super) struct OpenMenu {
    position: Point<Pixels>,
    menu: Entity<Menu>,
    _events: Subscription,
}
enum Target {
    File {
        path: String,
        location: Option<(usize, usize)>,
        base: Option<String>,
    },
    Web(String),
}
impl Target {
    fn resolve(url: &str, base: Option<&str>) -> Option<Self> {
        let lower = url.to_ascii_lowercase();
        if lower.starts_with("https://")
            || lower.starts_with("http://")
            || lower.starts_with("mailto:")
        {
            return Some(Self::Web(url.into()));
        }
        let peer = |path: &str| onicode_protocol::ProjectRef::parse_uri(path).is_some();
        if url.is_empty()
            || url.starts_with(['#', '?'])
            || (url.contains("://") && !url.starts_with("file://") && !peer(url))
        {
            return None;
        }
        // In a session on another engine, the path comes back as its URI there.
        let (path, location) = crate::file_editor::file_location(url, base.unwrap_or(""));
        if !Path::new(&path).is_absolute() && !peer(&path) {
            return None;
        }
        Some(Self::File {
            path,
            location,
            base: base.map(str::to_owned),
        })
    }

    fn items(&self) -> Vec<MenuItem> {
        match self {
            Self::Web(_) => vec![
                MenuAction::new("open-web", "Open Link").into(),
                MenuAction::new("copy-link", "Copy Link Address").into(),
            ],
            Self::File { base, .. } => {
                let reveal = if cfg!(target_os = "macos") {
                    "Reveal in Finder"
                } else if cfg!(target_os = "windows") {
                    "Reveal in File Explorer"
                } else {
                    "Open Containing Folder"
                };
                let mut items = vec![
                    MenuAction::new("open-monocode", "Open in Onicode").into(),
                    MenuAction::new("open-default", "Open in Default App").into(),
                    MenuAction::new("reveal", reveal).into(),
                    MenuItem::Separator,
                    MenuAction::new("copy-path", "Copy Path").into(),
                ];
                if base.is_some() {
                    items.push(MenuAction::new("copy-relative-path", "Copy Relative Path").into());
                }
                items
            }
        }
    }
}
impl TranscriptView {
    pub(super) fn open_link_menu(
        &mut self,
        request: &markdown::LinkMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = Target::resolve(&request.url, self.file_link_base.as_deref()) else {
            return;
        };
        self.selection.hide_menu();
        self.links.error = None;
        self.links.open = None;
        // Replacing an open menu must restore to the transcript, rather than
        // retaining a focus handle owned by the menu that is being discarded.
        if let Some(focus) = &self.selection_focus {
            window.focus(focus, cx);
        }
        let label = if matches!(&target, Target::File { .. }) {
            "File link actions"
        } else {
            "Link actions"
        };
        let menu = cx.new(|cx| Menu::new(label, target.items(), window, cx));
        let events = cx.subscribe_in(
            &menu,
            window,
            move |this, _, event: &MenuEvent, window, cx| {
                this.links.open = None;
                if let MenuEvent::Picked(id, _) = event {
                    this.run_link_action(id, &target, window, cx);
                }
                cx.notify();
            },
        );
        self.links.open = Some(OpenMenu {
            position: request.position,
            menu,
            _events: events,
        });
        cx.stop_propagation();
        cx.notify();
    }

    fn run_link_action(
        &mut self,
        action: &str,
        target: &Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (action, target) {
            ("open-web", Target::Web(url)) => cx.open_url(url),
            ("copy-link", Target::Web(url)) => {
                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))
            }
            ("open-monocode", Target::File { path, location, .. }) => {
                let target = location.map_or_else(
                    || path.clone(),
                    |(line, column)| format!("{path}:{line}:{column}"),
                );
                window.dispatch_action(Box::new(crate::file_editor::OpenPath(target)), cx);
            }
            // A file on another engine is copied as that engine names it, as its agent does.
            ("copy-path", Target::File { path, .. }) => {
                cx.write_to_clipboard(ClipboardItem::new_string(
                    onicode_protocol::ProjectRef::parse_uri(path)
                        .map_or_else(|| path.clone(), |peer| peer.path),
                ))
            }
            ("copy-relative-path", Target::File { path, base, .. }) => cx.write_to_clipboard(
                ClipboardItem::new_string(files::display_path(path, base.as_deref())),
            ),
            ("open-default" | "reveal", Target::File { path, .. }) => {
                let external = action == "open-default";
                let done = if external {
                    crate::explorer::open_with_default_app(Path::new(path), cx)
                } else {
                    crate::explorer::reveal_path(Path::new(path), cx)
                };
                if let Err(error) = done {
                    let action = if external {
                        "open the file in its default app"
                    } else {
                        "complete the file action"
                    };
                    self.links.error = Some(format!("Could not {action}: {error}"));
                    cx.notify();
                }
            }
            _ => {}
        }
    }

    pub(super) fn link_overlays(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .when_some(self.links.open.as_ref(), |d, open| {
                d.child(
                    menu_popover(&open.menu, Anchor::Point(open.position), cx)
                        .allow_outside_press(),
                )
            })
            .when_some(self.links.error.as_ref(), |d, error| {
                d.child(
                    div()
                        .id("transcript-link-error")
                        .role(Role::Alert)
                        .absolute()
                        .bottom(px(16.))
                        .right(px(16.))
                        .max_w(px(384.))
                        .flex()
                        .items_start()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(0xf87171))
                        .bg(rgb(0x252525))
                        .text_size(px(12.))
                        .text_color(rgb(0xfca5a5))
                        .child(error.clone())
                        .child(
                            div()
                                .id("dismiss-link-error")
                                .role(Role::Button)
                                .aria_label("Dismiss file action error")
                                .flex_shrink_0()
                                .cursor_pointer()
                                .child("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.links.error = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}
