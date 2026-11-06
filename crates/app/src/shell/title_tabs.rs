//! All title-strip commands operate on the same visible, ordered snapshot.
use onicode_core::files::path_key;

use super::*;
use crate::workspace::TitleTab;

impl Shell {
    /// The workspace keys a tab by one of its sessions. Replacing that session
    /// changes the key, but the same slot and selected width stay on screen.
    pub(super) fn replace_title_tab_key(&mut self, from: i64, to: i64) {
        if from == to {
            return;
        }
        let previous = TitleTab::Session(from);
        let next = TitleTab::Session(to);
        self.title_tab_motion.replace_key(&previous, next.clone());
        for selected in [
            &mut self.title_tab_selection.0,
            &mut self.title_tab_selection.1,
        ] {
            if selected.as_ref() == Some(&previous) {
                *selected = Some(next.clone());
            }
        }
        let shift = self.tab_shifts.remove(&previous);
        self.tab_shifts.remove(&next);
        if let Some(shift) = shift {
            self.tab_shifts.insert(next, shift);
        }
        self.pane_ticks.remove(&from);
        self.pane_ticks.remove(&to);
        if self.tab_hover == Some(from) {
            self.tab_hover = Some(to);
        }
        if self.pin_card_hover.as_ref() == Some(&previous) {
            self.pin_card_hover = Some(TitleTab::Session(to));
        }
    }

    pub(super) fn tab_scope(&self) -> String {
        format!(
            "{}\0{}",
            path_key(&self.workspace.project),
            path_key(self.working_copy())
        )
    }

    /// Every tab this scope shows, pinned ones first in pin order, then the
    /// strip's own. Shortcuts and cycling walk this order.
    pub(super) fn visible_tabs(&self, cx: &App) -> Vec<TitleTab> {
        let mut tabs = self.scoped_tabs(cx);
        let pinned = &self.workspace.pinned_tabs;
        // Stable, so the strip keeps its own order behind the pins.
        tabs.sort_by_key(|tab| pinned.iter().position(|p| p == tab).unwrap_or(usize::MAX));
        tabs
    }

    /// The tabs drawn as plates in the strip, right of the pinned deck.
    /// Drags, drops and strip indices count only these.
    pub(super) fn strip_tabs(&self, cx: &App) -> Vec<TitleTab> {
        self.scoped_tabs(cx)
            .into_iter()
            .filter(|tab| !self.workspace.pinned_tabs.contains(tab))
            .collect()
    }

    fn scoped_tabs(&self, cx: &App) -> Vec<TitleTab> {
        self.workspace
            .title_order
            .iter()
            .filter(|tab| match tab {
                TitleTab::Search => !self.workspace.preferences.search_panel,
                TitleTab::App(view) => {
                    !self.workspace.classic_layout
                        && (*view != app_views::AppView::Notes || self.workspace.preferences.notes)
                }
                TitleTab::Session(tab) => {
                    self.workspace.tabs.contains(tab) && {
                        let id = self.workspace.scope_pane(*tab);
                        self.session_summaries
                            .contains_key(&self.workspace.session_for_pane(id))
                            && self
                                .session_metadata(self.workspace.session_for_pane(id), cx)
                                .summary
                                .cwd
                                == self.workspace.project
                            && self.in_worktree_scope(id, cx)
                    }
                }
            })
            .cloned()
            .collect()
    }

    pub(super) fn active_title_tab(&self) -> Option<TitleTab> {
        if let Some(view) = self.active_app_view {
            return Some(TitleTab::App(view));
        }
        if self.search_page() {
            return Some(TitleTab::Search);
        }
        self.workspace.active_tab().map(TitleTab::Session)
    }

    /// The active tab follows live focus; other tabs keep their last displayed
    /// pane instead of borrowing the currently selected tab's preview focus.
    pub(super) fn tab_review_focused(&self, tab: i64) -> bool {
        if self.active_title_tab() == Some(TitleTab::Session(tab)) {
            self.review_focused
        } else {
            self.pane_ticks
                .get(&tab)
                .is_some_and(|tick| tick.shown == (self.workspace.tab_focus(tab), true))
        }
    }

    pub(super) fn remember_title_tab(&mut self, tab: TitleTab) {
        if !self.workspace.title_order.contains(&tab) {
            self.workspace.title_order.push(tab.clone());
        }
        self.workspace.scope_focus.insert(self.tab_scope(), tab);
    }

    pub(super) fn open_title_tab(
        &mut self,
        tab: &TitleTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match tab {
            TitleTab::Search => self.open_app_search(window, cx),
            TitleTab::App(view) => match view {
                app_views::AppView::Inbox => self.open_inbox(window, cx),
                app_views::AppView::Notes => self.open_notes(window, cx),
                app_views::AppView::Automations => self.open_automations(window, cx),
            },
            TitleTab::Session(id) => {
                let focus = self.workspace.tab_focus(*id);
                let review_focused = self.tab_review_focused(*id);
                self.open(focus, window, cx);
                if review_focused
                    && self.workspace.active == Some(focus)
                    && let Some(review) = self.active_review(cx)
                {
                    self.review_focused = true;
                    review.update(cx, |r, cx| r.focus_active(window, cx));
                }
            }
        }
    }

    pub(super) fn restore_scope_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let tab = self.workspace.scope_focus.get(&self.tab_scope()).cloned();
        if let Some(tab) = tab.filter(|tab| self.visible_tabs(cx).contains(tab)) {
            self.open_title_tab(&tab, window, cx);
            return true;
        }
        false
    }

    pub(super) fn run_title_tab_menu(
        &mut self,
        tab: TitleTab,
        action: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tabs = self.visible_tabs(cx);
        let Some(index) = tabs.iter().position(|entry| *entry == tab) else {
            return;
        };
        if matches!(action, "pin" | "unpin") {
            self.toggle_pin_tab(tab, cx);
            return;
        }
        if action == "close" {
            if let Some(id) = tab.session() {
                self.close(id, window, cx);
            } else {
                self.close_title_tabs(&[tab], None, window, cx);
            }
            return;
        }
        // Like a browser's pinned tabs, these survive the bulk closes.
        let closing = tabs
            .into_iter()
            .enumerate()
            .filter(|(_, entry)| !self.workspace.pinned_tabs.contains(entry))
            .filter(|(ix, entry)| match action {
                "others" => *entry != tab,
                "right" => *ix > index,
                "left" => *ix < index,
                "all" => true,
                _ => false,
            })
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        self.close_title_tabs(&closing, (action == "others").then_some(tab), window, cx);
    }

    pub(super) fn close_title_tabs(
        &mut self,
        tabs: &[TitleTab],
        preferred: Option<TitleTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let panes = tabs
            .iter()
            .filter(|tab| self.workspace.title_order.contains(tab))
            .filter_map(TitleTab::session)
            .flat_map(|id| self.workspace.layout(id).leaf_ids())
            .collect::<Vec<_>>();
        let tabs = tabs.to_vec();
        self.confirm_close_terminal_panes(&panes, window, cx, move |this, window, cx| {
            this.finish_close_title_tabs(&tabs, preferred, window, cx);
        });
    }

    fn finish_close_title_tabs(
        &mut self,
        tabs: &[TitleTab],
        preferred: Option<TitleTab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.visible_tabs(cx);
        let closing: HashSet<_> = tabs
            .iter()
            .filter(|tab| self.workspace.title_order.contains(tab))
            .cloned()
            .collect();
        if closing.is_empty() {
            return;
        }
        let active = self.active_title_tab();
        let replace = active.as_ref().is_some_and(|tab| closing.contains(tab));
        // Pins never close on their own, so the strip keeps one tab beside
        // them: closing its last falls back to a blank session, not a pin.
        let strip_left = before
            .iter()
            .any(|tab| !closing.contains(tab) && !self.workspace.pinned_tabs.contains(tab));
        let pins_left = before
            .iter()
            .any(|tab| !closing.contains(tab) && self.workspace.pinned_tabs.contains(tab));
        let next = if replace && pins_left && !strip_left {
            None
        } else if replace {
            preferred
                .filter(|tab| before.contains(tab) && !closing.contains(tab))
                .or_else(|| {
                    let index = before.iter().position(|tab| Some(tab) == active.as_ref())?;
                    // Browsers select the next tab to the right, then the one to the left.
                    before[index + 1..]
                        .iter()
                        .chain(before[..index].iter().rev())
                        .find(|tab| !closing.contains(*tab))
                        .cloned()
                })
        } else {
            None
        };
        self.navigation_revision += 1;
        self.close_menu(cx);
        self.tab_drag = None;
        self.tab_shifts.clear();
        self.tab_hover = None;
        self.tab_scroll_task = None;
        self.cancel_pane_drag(cx);
        let session_active = self.workspace.active;
        for tab in &closing {
            if let TitleTab::App(view) = tab
                && self.active_app_view == Some(*view)
            {
                self.leave_app_views(cx);
            }
            if *tab == TitleTab::Search {
                self.leave_app_search(cx);
                self.saved_search = None;
            }
            if let TitleTab::Session(id) = tab {
                self.workspace.close(*id);
            }
        }
        self.workspace
            .title_order
            .retain(|tab| !closing.contains(tab));
        self.workspace
            .scope_focus
            .retain(|_, tab| !closing.contains(tab));
        self.workspace.prune_pinned_tabs();
        if replace {
            self.workspace.active = None;
            self.review_focused = false;
            self.review_slide = None;
            self.pane_motion = None;
            if let Some(tab) = next {
                self.open_title_tab(&tab, window, cx);
            } else {
                self.ensure_project_session(window, cx);
            }
        } else {
            self.workspace.active =
                session_active.filter(|id| self.workspace.tab_of(*id).is_some());
        }
        self.history
            .prune(&self.workspace.open_sessions(), self.workspace.active);
        if self.workspace.active.is_none() && !self.app_view_open() {
            self.focus_active(window, cx);
        }
        self.persist(cx);
        cx.notify();
    }

    /// Moves `tab` to strip slot `to`; pinned tabs keep their places.
    pub(super) fn reorder_title_tab(&mut self, tab: &TitleTab, to: usize, cx: &App) {
        let mut visible = self.strip_tabs(cx);
        let Some(from) = visible.iter().position(|entry| entry == tab) else {
            return;
        };
        let entry = visible.remove(from);
        visible.insert(to.min(visible.len()), entry);
        let set: HashSet<_> = visible.iter().cloned().collect();
        let mut ordered = visible.into_iter();
        for entry in &mut self.workspace.title_order {
            if set.contains(entry) {
                *entry = ordered.next().unwrap();
            }
        }
        self.workspace.tabs = self
            .workspace
            .title_order
            .iter()
            .filter_map(TitleTab::session)
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::shell::tests::window;

    #[gpui::test]
    fn rapid_new_tabs_follow_request_order(cx: &mut TestAppContext) {
        let (window, _, first) = window(cx);
        let providers = [Kind::Claude, Kind::Codex, Kind::Claude, Kind::Codex];
        window
            .update(cx, |shell, _, cx| {
                for provider in providers {
                    shell.create(provider, cx);
                }
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let tabs = shell.visible_tabs(cx);
                assert_eq!(tabs.len(), 5);
                assert_eq!(tabs[0], TitleTab::Session(first));
                for (tab, provider) in tabs[1..].iter().zip(providers) {
                    assert_eq!(
                        shell.sessions[&tab.session().unwrap()]
                            .read(cx)
                            .summary
                            .provider,
                        provider.id()
                    );
                }
                assert_eq!(shell.active_title_tab().as_ref(), tabs.last());
            })
            .unwrap();
    }
}
