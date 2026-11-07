//! The terminal's find panel: the file editor's Find row (monocode's editorSearch panel, without
//! its Replace row) over the mirror's own scrollback. The query runs on the viewer's copy of the
//! terminal, so it answers in the same frame whether the shell runs on this Mac or another, and
//! output that arrives while it is open is searched again a moment later. Matches fill their
//! cells as the editor's do; the current one takes the selection's fill.
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, Context, Entity, Focusable, Hsla, Role, Subscription, Task, Window, div, px, rgb,
};
use onicode_term::search::Query;

use super::{
    DismissFind, Find as FindAction, FindNext, FindPrevious, MatchCase, Regex, Terminal, WholeWord,
    selection_fill,
};
use crate::input::{self, Input};
use crate::right_panel::{icon_action, stroke};
use crate::theme::*;

/// How long the open find waits after the grid changed before it reads it again: output keeps
/// its matches current without reading a streaming scrollback on every write.
const REFRESH: Duration = Duration::from_millis(100);

/// The open find panel.
pub(super) struct Find {
    field: Entity<Input>,
    case: bool,
    word: bool,
    regex: bool,
    /// Reads the changed grid again once [`REFRESH`] has passed.
    refresh: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

/// What a match fills its cells with, as the editor's: the current one takes the selection's.
pub(super) fn found_fill(current: bool) -> Hsla {
    if current {
        return selection_fill();
    }
    let mut fill: Hsla = rgb(0xe2c08d).into();
    fill.a = 0.46;
    fill
}

impl Terminal {
    /// Opens the find panel, its query the selection when that is one line, and focuses it.
    pub(super) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.is_none() {
            let field = cx.new(|cx| {
                let mut field = Input::field("Find", "Find", content(0.35), cx);
                field.set_font_size(12.);
                field
            });
            let subscriptions = [
                cx.subscribe(&field, |this, _, _: &input::Changed, cx| this.run_find(cx)),
                cx.subscribe(&field, |this, _, _: &input::Submitted, cx| {
                    this.step_find(false, cx);
                }),
            ];
            if let Some(selected) = self
                .mirror
                .selection_text()
                .filter(|text| !text.contains('\n'))
            {
                field.update(cx, |field, cx| field.set_text(selected, cx));
            }
            self.find = Some(Find {
                field,
                case: false,
                word: false,
                regex: false,
                refresh: None,
                _subscriptions: subscriptions,
            });
            self.run_find(cx);
        }
        if let Some(find) = &self.find {
            window.focus(&find.field.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Finds the panel's query, moving to the match nearest the bottom of the view.
    pub(super) fn run_find(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &self.find else {
            return;
        };
        let query = Query {
            text: find.field.read(cx).content.to_string(),
            case: find.case,
            word: find.word,
            regex: find.regex,
        };
        self.mirror.find(query);
        cx.notify();
    }

    /// The next match down, or up with `backwards`, from the grid as it is now. Without a panel
    /// open the keys are someone else's.
    pub(super) fn step_find(&mut self, backwards: bool, cx: &mut Context<Self>) {
        if self.find.is_none() {
            cx.propagate();
            return;
        }
        self.mirror.refresh_find();
        self.mirror.find_step(backwards);
        cx.notify();
    }

    fn toggle_find(&mut self, toggle: impl FnOnce(&mut Find), cx: &mut Context<Self>) {
        if let Some(find) = &mut self.find {
            toggle(find);
            self.run_find(cx);
        }
    }

    /// Closes the panel and gives the keyboard back to the shell; Escape reaches the shell when
    /// no panel is open.
    pub(super) fn dismiss_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.find.take().is_some() {
            self.mirror.end_find();
            window.focus(&self.focus, cx);
            cx.notify();
        } else {
            cx.propagate();
        }
    }

    /// Reads the grid again soon when it changed under the open find.
    pub(super) fn schedule_find_refresh(&mut self, cx: &mut Context<Self>) {
        let stale = self.mirror.finding().is_some_and(|finding| finding.stale);
        let Some(find) = self
            .find
            .as_mut()
            .filter(|find| stale && find.refresh.is_none())
        else {
            return;
        };
        find.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(REFRESH).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(find) = &mut this.find {
                    find.refresh = None;
                }
                if this.mirror.refresh_find() {
                    cx.notify();
                }
            });
        }));
    }

    /// The panel as the editor's Find row draws it: the query with its count, the case, word and
    /// regular expression toggles, previous, next and close.
    pub(super) fn find_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.find.as_ref()?;
        let finding = self.mirror.finding().unwrap_or_default();
        let empty = find.field.read(cx).content.is_empty();
        let count = if finding.invalid {
            "Invalid regex".into()
        } else if empty {
            String::new()
        } else if finding.total == 0 {
            "No results".into()
        } else {
            format!(
                "{} of {}{}",
                finding.current.map_or(0, |at| at + 1),
                finding.total.min(999),
                if finding.total > 999 { "+" } else { "" }
            )
        };
        let bad = finding.invalid || (!empty && finding.total == 0);
        let query = div()
            .h(px(26.))
            .min_w_0()
            .flex_1()
            .flex()
            .items_center()
            .px_2()
            .rounded_md()
            .border_1()
            .border_color(if bad {
                rgb(0xf87171).opacity(0.55).into()
            } else {
                content(0.1)
            })
            .bg(content(0.06))
            .child(div().flex_1().min_w_0().child(find.field.clone()))
            .child(
                div()
                    .min_w(px(85.))
                    .text_right()
                    .text_size(px(11.))
                    .text_color(if bad {
                        rgb(0xf87171).into()
                    } else {
                        content(0.45)
                    })
                    .child(count),
            );
        let toggle = |id, label: &'static str, title: &'static str, on| {
            div()
                .id(id)
                .role(Role::Button)
                .aria_label(title)
                .aria_toggled(if on {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_size(px(11.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(content(if on { 1. } else { 0.6 }))
                .when(on, |d| d.bg(accent_color().opacity(0.25)))
                .hover(|s| s.bg(content(0.1)))
                .child(label)
        };
        let any = finding.total > 0;
        Some(
            div()
                .key_context("TerminalFind")
                .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "enter" && event.keystroke.modifiers.shift {
                        this.step_find(true, cx);
                        cx.stop_propagation();
                    }
                }))
                .on_action(cx.listener(|this, _: &FindAction, window, cx| {
                    this.open_find(window, cx);
                }))
                .on_action(cx.listener(|this, _: &FindNext, _, cx| this.step_find(false, cx)))
                .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.step_find(true, cx)))
                .on_action(cx.listener(|this, _: &DismissFind, window, cx| {
                    this.dismiss_find(window, cx);
                }))
                .on_action(cx.listener(|this, _: &MatchCase, _, cx| {
                    this.toggle_find(|find| find.case = !find.case, cx);
                }))
                .on_action(cx.listener(|this, _: &WholeWord, _, cx| {
                    this.toggle_find(|find| find.word = !find.word, cx);
                }))
                .on_action(cx.listener(|this, _: &Regex, _, cx| {
                    this.toggle_find(|find| find.regex = !find.regex, cx);
                }))
                .w_full()
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .py_1()
                .border_b_1()
                .border_color(stroke())
                .font_family(crate::fonts::mono())
                .child(query)
                .child(
                    toggle("terminal-match-case", "Aa", "Match Case", find.case).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.toggle_find(|find| find.case = !find.case, cx);
                        }),
                    ),
                )
                .child(
                    toggle("terminal-whole-word", "ab", "Match Whole Word", find.word).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.toggle_find(|find| find.word = !find.word, cx);
                        }),
                    ),
                )
                .child(
                    toggle(
                        "terminal-use-regex",
                        ".*",
                        "Use Regular Expression",
                        find.regex,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.toggle_find(|find| find.regex = !find.regex, cx);
                    })),
                )
                .child(
                    icon_action("terminal-find-prev", "Previous Match", "chevron-up", any)
                        .on_click(cx.listener(|this, _, _, cx| this.step_find(true, cx))),
                )
                .child(
                    icon_action("terminal-find-next", "Next Match", "chevron-down", any)
                        .on_click(cx.listener(|this, _, _, cx| this.step_find(false, cx))),
                )
                .child(
                    icon_action("terminal-close-find", "Close", "x", true)
                        .size(px(24.))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.dismiss_find(window, cx);
                        })),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn find_runs_over_the_scrollback_and_gives_the_keyboard_back(cx: &mut gpui::TestAppContext) {
        let window = cx.update(|cx| {
            super::super::bind(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| Terminal::new("/tmp".into(), cx))
            })
            .unwrap()
        });
        window
            .update(cx, |terminal, _, _| {
                let output = (0..40)
                    .map(|n| format!("line {n:02}"))
                    .collect::<Vec<_>>()
                    .join("\r\n");
                terminal.mirror.feed(output.as_bytes());
            })
            .unwrap();
        cx.refresh().unwrap();
        window
            .update(cx, |terminal, w, cx| terminal.open_find(w, cx))
            .unwrap();
        window
            .update(cx, |terminal, _, cx| {
                let field = terminal.find.as_ref().unwrap().field.clone();
                field.update(cx, |field, cx| field.set_text("line 1".into(), cx));
            })
            .unwrap();
        cx.run_until_parked();
        cx.refresh().unwrap();
        window
            .update(cx, |terminal, w, cx| {
                let finding = terminal.mirror.finding().unwrap();
                assert_eq!((finding.total, finding.current), (10, Some(9)));
                let field = terminal.find.as_ref().unwrap().field.focus_handle(cx);
                assert!(field.is_focused(w), "the query takes the keyboard");
                terminal.step_find(false, cx);
                assert_eq!(terminal.mirror.finding().unwrap().current, Some(0));
                terminal.toggle_find(|find| find.case = true, cx);
                assert_eq!(terminal.mirror.finding().unwrap().total, 10);
                terminal.toggle_find(|find| find.regex = true, cx);
                assert_eq!(terminal.mirror.finding().unwrap().total, 10);
            })
            .unwrap();
        cx.refresh().unwrap();
        window
            .update(cx, |terminal, w, cx| {
                terminal.dismiss_find(w, cx);
                assert!(terminal.find.is_none());
                assert_eq!(terminal.mirror.finding(), None);
                assert!(
                    terminal.focus.is_focused(w),
                    "the shell has the keyboard again"
                );
            })
            .unwrap();
    }
}
