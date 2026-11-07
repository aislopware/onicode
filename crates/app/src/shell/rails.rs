//! The sidebar's session cards: folder members hang off a line under their folder (`step`),
//! and the original sidebar draws the full, three-line SessionCard.
use super::*;

/// `.zen-phase-step`: a step branching off the transcript's tool rail, its row 2px lower inside
/// the step's padding.
pub(super) fn step(last: bool, child: impl IntoElement) -> gpui::Div {
    div()
        .relative()
        .pl(px(20.))
        .py(px(2.))
        .child(crate::activity::rail_branch_at(last, true, 2.))
        .child(child)
}

impl Shell {
    /// A session row.
    pub(super) fn session_card(
        &self,
        id: i64,
        now: &crate::inbox::RelativeNow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.workspace.classic_layout {
            return self.classic_session_card_at_depth(id, false, false, now, cx);
        }
        self.session_row(id, now, cx)
    }

    /// A folder member's compact row.
    pub(super) fn nested_session_card(
        &self,
        id: i64,
        now: &crate::inbox::RelativeNow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.workspace.classic_layout {
            return self.classic_session_card_at_depth(id, true, false, now, cx);
        }
        self.session_row_with(id, true, now, cx)
    }

    /// The original sidebar uses the full, three-line SessionCard. Folder
    /// members omit only the model header.
    pub(super) fn classic_session_card_at_depth(
        &self,
        id: i64,
        nested: bool,
        compact: bool,
        now: &crate::inbox::RelativeNow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(row) = self.rename_row(&session_groups::RenameTarget::Session(id), cx) {
            return row;
        }
        let work_item = self.work_item_badge(id, false, cx);
        let session = self.session_metadata(id, cx);
        let selected = self.selected_sessions.contains(&id);
        let active = self.workspace.active == Some(id) && !self.app_view_open();
        let draft = session.has_saved_draft();
        let approval = session.needs_approval();
        let header = !compact;
        let saved = self.session_details.get(&id);
        let (model, branch) = self
            .sessions
            .get(&id)
            .and_then(|view| view.read(cx).sidebar_details())
            .unwrap_or_else(|| {
                (
                    crate::pickers::cached_model_name(
                        Kind::from_id(&session.summary.provider),
                        saved.and_then(|details| details.model.as_deref()),
                    ),
                    None,
                )
            });
        let cwd = self
            .session_worktrees
            .get(&id)
            .and_then(|cwd| cwd.as_deref())
            .unwrap_or(&session.summary.cwd);
        let branch = branch
            .or_else(|| saved.and_then(|details| details.branch.clone()))
            .or_else(|| {
                let git = self.git.read(cx);
                (git.root == cwd)
                    .then(|| git.index.as_ref().and_then(|index| index.branch.clone()))
                    .flatten()
            });
        let repo = std::path::Path::new(&session.summary.cwd)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&session.summary.cwd);
        let git_label = if saved.is_some_and(|details| details.worktree_removed) {
            "No branch".to_string()
        } else {
            match branch.filter(|branch| !branch.is_empty()) {
                Some(branch) if !repo.is_empty() => format!("{repo}/{branch}"),
                Some(branch) => branch,
                None => repo.to_owned(),
            }
        };
        let model = if session.summary.provider == "claude" {
            model
                .strip_prefix("Claude ")
                .map(|name| SharedString::from(name.to_owned()))
                .unwrap_or(model)
        } else {
            model
        };
        let status = self.classic_session_status(id, &session, now, cx);
        let title = div()
            .debug_selector(move || format!("session-title-{id}"))
            .flex_1()
            .min_w_0()
            .h(px(17.875))
            .text_size(px(13.))
            .line_height(px(17.875))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(content(1.))
            .child(crate::particle_text::particle_text(session.display_title()));
        let body = div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .when(header, |d| {
                d.child(
                    div()
                        .h(px(16.5))
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(provider_icon(&session.summary.provider, 14.))
                                .child(
                                    div()
                                        .debug_selector(move || format!("session-model-{id}"))
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(11.))
                                        .line_height(px(16.5))
                                        .text_color(content(0.5))
                                        .child(model),
                                ),
                        )
                        .child(status),
                )
            })
            .child(
                div()
                    .when(header, |d| d.mt_1())
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .when(session.summary.pinned, |d| {
                        d.child(icon("pin", 12.).text_color(content(0.45)))
                    })
                    .child(title)
                    .when(!header, |d| {
                        d.child(
                            div()
                                .pl_2()
                                .child(self.classic_session_status(id, &session, now, cx)),
                        )
                    }),
            )
            .child(
                div()
                    .mt_1()
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(11.))
                            .line_height(px(16.5))
                            .text_color(content(0.45))
                            .when(!git_label.is_empty(), |d| {
                                d.child(icon("git-branch", 12.)).child(
                                    div()
                                        .debug_selector(move || format!("session-workspace-{id}"))
                                        .min_w_0()
                                        .truncate()
                                        .child(git_label),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_px()
                            .child(
                                div()
                                    .size(px(20.))
                                    .when(self.session_hover == Some(id), |d| {
                                        d.child(self.session_archive_button(id, &session, cx))
                                    }),
                            )
                            .children(work_item)
                            .when(
                                self.automation_state.session_automations.contains_key(&id),
                                |d| d.child(icon("zap", 12.).text_color(rgb(0xfbbf24))),
                            ),
                    ),
            );
        let anchor = self
            .session_row_bounds
            .get(&id)
            .cloned()
            .unwrap_or_default();
        self.sidebar_list.track_anchor(anchor.clone());
        let mut selected_fill = crate::theme::accent_color();
        selected_fill.a = 0.15;
        div()
            .id(("sidebar-session", id as usize))
            .debug_selector(move || format!("sidebar-session-{id}"))
            .group("session-row")
            .role(Role::Button)
            .aria_label(session.display_title())
            .aria_selected(selected || active)
            .aria_description(format!(
                "{} conversation",
                Kind::from_id(&session.summary.provider).label()
            ))
            .cursor_pointer()
            .relative()
            .w_full()
            .border_1()
            .border_color(content(0.))
            .rounded_md()
            .px(px(if nested { 6. } else { 10. }))
            .pt(px(if compact { 6. } else { 8. }))
            .pb(px(if compact { 6. } else { 8. }))
            .flex()
            .flex_col()
            .text_color(content(0.8))
            .when(selected, |d| d.bg(selected_fill))
            .when(!selected && approval, |d| d.bg(content(0.2)))
            .when(!selected && !approval && active, |d| d.bg(content(0.1)))
            .when(draft || approval, |d| {
                d.border_dashed()
                    .border_color(content(if active || selected || approval {
                        0.3
                    } else {
                        0.25
                    }))
            })
            .when(!selected && !approval && !active, |d| {
                d.hover(|s| s.bg(content(0.05)))
            })
            .when(
                self.sidebar_drop == Some(session_groups::ListDrop::Session(id)),
                |d| {
                    let mut fill = crate::theme::accent_color();
                    fill.a = 0.2;
                    d.bg(fill)
                },
            )
            .when(self.deleting.contains(&id), |d| d.opacity(0.5))
            .child(body)
            .child(anchor.probe())
            .on_hover(cx.listener(move |this, hovered, window, cx| {
                // Original cards carry their details inline, so hovering only
                // warms the existing metadata path; it does not open a preview.
                this.hover_session(id, anchor.clone(), *hovered, false, window, cx);
            }))
            .on_click(
                cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                    if std::mem::take(&mut this.skip_session_click) {
                        return;
                    }
                    this.select_sidebar_session(id, event.modifiers(), window, cx);
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, _| {
                    this.skip_session_click = false;
                    this.tab_drag = None;
                    this.pane_drag = Some(PaneDrag {
                        source: DragSource::Session(id),
                        start: event.position,
                        position: event.position,
                        active: false,
                        over: None,
                        insert: None,
                    });
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.context_selection = !this.selected_sessions.contains(&id);
                    if this.context_selection {
                        this.selected_sessions.clear();
                        this.selected_sessions.insert(id);
                    }
                    this.open_menu(
                        MenuTarget::Session(id),
                        Anchor::Point(event.position),
                        window,
                        cx,
                    );
                }),
            )
            .into_any_element()
    }

    /// The original search applies the same fuzzy query independently to the
    /// title, model, harness, and repository/branch fields.
    pub(super) fn classic_session_matches_query(&self, id: i64, cx: &App) -> bool {
        let query = self.query.trim();
        if query.is_empty() {
            return true;
        }
        let session = self.session_metadata(id, cx);
        let saved = self.session_details.get(&id);
        let live = self
            .sessions
            .get(&id)
            .and_then(|view| view.read(cx).sidebar_details());
        let model = live
            .as_ref()
            .map(|(model, _)| model.to_string())
            .unwrap_or_else(|| {
                crate::pickers::cached_model_name(
                    Kind::from_id(&session.summary.provider),
                    saved.and_then(|details| details.model.as_deref()),
                )
                .to_string()
            });
        let cwd = self
            .session_worktrees
            .get(&id)
            .and_then(|cwd| cwd.as_deref())
            .unwrap_or(&session.summary.cwd);
        let branch = live
            .and_then(|(_, branch)| branch)
            .or_else(|| saved.and_then(|details| details.branch.clone()))
            .or_else(|| {
                let git = self.git.read(cx);
                (git.root == cwd)
                    .then(|| git.index.as_ref().and_then(|index| index.branch.clone()))
                    .flatten()
            });
        let repo = std::path::Path::new(&session.summary.cwd)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        let git = match branch {
            Some(branch) if !repo.is_empty() => format!("{repo}/{branch}"),
            Some(branch) => branch,
            None => repo.to_owned(),
        };
        [
            session.display_title(),
            session.summary.title.clone(),
            saved
                .and_then(|details| details.model.clone())
                .unwrap_or_default(),
            model,
            session.summary.provider.clone(),
            git,
        ]
        .iter()
        .any(|field| onicode_protocol::search::fuzzy(query, field).is_some())
    }

    fn classic_session_status(
        &self,
        id: i64,
        session: &session_loading::Metadata<'_>,
        now: &crate::inbox::RelativeNow,
        _cx: &Context<Self>,
    ) -> AnyElement {
        let status = div()
            .flex_shrink_0()
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(11.))
            .line_height(px(16.5))
            .text_color(content(0.45));
        if session.needs_approval() {
            status
                .text_color(rgb(0xfbbf24))
                .child(icon("circle-alert", 12.))
                .child("Need approval")
                .into_any_element()
        } else if session.responding() {
            status
                .text_color(crate::theme::accent_color())
                .children(self.session_marks.get(&id).cloned())
                .child("Working...")
                .into_any_element()
        } else if self.unseen_finished.contains(&id) {
            status
                .text_color(rgb(0x34d399))
                .child(icon("check", 12.))
                .child("Done")
                .into_any_element()
        } else if session.has_saved_draft() {
            status
                .text_color(content(0.55))
                .child(icon("circle-dashed-hi", 12.))
                .child("Draft")
                .into_any_element()
        } else {
            status
                .child(now.relative(session.summary.updated_at))
                .into_any_element()
        }
    }
}
