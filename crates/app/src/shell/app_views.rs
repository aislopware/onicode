//! The three application destinations share one navigation and focus boundary.
use super::*;
pub(super) use crate::workspace::AppView;
use crate::{automations, inbox, notes};

impl Shell {
    pub(super) fn app_view_open(&self) -> bool {
        self.active_app_view.is_some()
    }

    pub(super) fn app_view_label(&self) -> Option<&'static str> {
        self.active_app_view.map(|view| match view {
            AppView::Inbox => "Inbox",
            AppView::Notes => "Notes",
            AppView::Automations => "Automations",
        })
    }

    pub(super) fn app_title_tab(
        &self,
        view: AppView,
        width: f32,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let active = self.active_app_view == Some(view);
        let close = active || width >= TAB_CLOSE_MIN_WIDTH;
        div()
            .id(SharedString::from(format!(
                "app-title-tab-{}",
                view.label()
            )))
            .debug_selector(move || format!("app-title-tab-{}", view.label()))
            .group("app-title-tab")
            .role(Role::Tab)
            .aria_label(view.label())
            .aria_selected(active)
            .cursor_pointer()
            .w(px(width))
            .h(px(TITLE_BAR_HEIGHT))
            .flex_shrink_0()
            .flex()
            .items_center()
            .relative()
            .child(
                title_tab_plate(active, false)
                    .text_color(content(if active { 1. } else { 0.5 }))
                    .w_full()
                    .min_w_0()
                    .when(close && width >= TAB_CLOSE_MIN_WIDTH, |d| d.pr_7())
                    .when(width < NARROW_TAB, |d| {
                        d.px(px(((width - TAB_TILE.w) / 2.).max(0.)))
                    })
                    .gap(px(TAB_TILE_GAP))
                    .group_hover("app-title-tab", |s| {
                        s.text_color(content(1.)).bg(title_tab_fill(active, true))
                    })
                    .child(mark_tile(
                        &TAB_TILE,
                        Face::Neutral,
                        icon(view.icon(), 14.).text_color(crate::tab_tile::tile_ink()),
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .text_size(px(TITLE_TAB_TEXT_SIZE))
                            .child(view.label()),
                    ),
            )
            .when(close, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!(
                            "close-app-tab-{}",
                            view.label()
                        )))
                        .debug_selector(move || format!("close-app-tab-{}", view.label()))
                        .role(Role::Button)
                        .aria_label(format!("Close {}", view.label()))
                        .absolute()
                        .right_1()
                        .top(px(10.))
                        .size(px(20.))
                        .rounded_sm()
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.)
                        .group_hover("app-title-tab", |s| s.opacity(1.))
                        .hover(|s| s.bg(content(0.1)))
                        .child(icon("close", 12.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_title_tabs(&[TitleTab::App(view)], None, window, cx);
                        })),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event, _, cx| {
                    this.start_tab_drag(TitleTab::App(view), event, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open_menu(
                        MenuTarget::AppTab(view),
                        Anchor::Point(event.position),
                        window,
                        cx,
                    )
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_title_tabs(&[TitleTab::App(view)], None, window, cx);
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if std::mem::take(&mut this.skip_tab_click) {
                    return;
                }
                this.open_title_tab(&TitleTab::App(view), window, cx);
            }))
    }

    pub(super) fn leave_app_views(&mut self, cx: &mut Context<Self>) {
        if self.active_app_view == Some(AppView::Notes)
            && let Some(notes) = &self.notes
        {
            notes.update(cx, |notes, cx| notes.flush(cx));
        }
        self.active_app_view = None;
        self.invalidate_chrome(cx);
    }

    pub(super) fn close_app_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_app_views(cx);
        if self.workspace.active.is_none() {
            let previous = self
                .workspace
                .project_focus
                .get(&self.workspace.project)
                .copied()
                .filter(|id| self.workspace.tab_of(*id).is_some());
            if let Some(id) = previous {
                self.activate_session(id, window, cx);
            }
        }
        self.ensure_project_session(window, cx);
        self.update_chat_visibility(cx);
        self.sync_right_panel(cx);
        self.focus_active(window, cx);
        if let Some(tab) = self.active_title_tab() {
            self.remember_title_tab(tab);
        }
        self.persist(cx);
        cx.notify();
    }

    fn prepare_app_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_app_search(cx);
        self.close_preview(cx);
        self.close_menu(cx);
        if self.settings.is_some() {
            self.close_settings(window, cx);
        }
        self.navigation_revision += 1;
        self.review_focused = false;
        self.pending_composer_focus = None;
    }

    fn show_app_view(&mut self, view: AppView, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_app_view != Some(view) {
            self.leave_app_views(cx);
        }
        self.active_app_view = Some(view);
        if !self.workspace.classic_layout {
            self.remember_title_tab(TitleTab::App(view));
            self.reveal_active_tab(cx);
        }
        self.update_chat_visibility(cx);
        self.sync_right_panel(cx);
        self.invalidate_chrome(cx);
        self.focus_app_view(window, cx);
        self.persist(cx);
        cx.notify();
    }

    pub(super) fn app_view_body(&self) -> Option<AnyElement> {
        match self.active_app_view? {
            AppView::Notes => self.notes.clone().map(IntoElement::into_any_element),
            AppView::Inbox => self.inbox.clone().map(IntoElement::into_any_element),
            AppView::Automations => self.automations.clone().map(IntoElement::into_any_element),
        }
    }

    pub(super) fn focus_app_view(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let focus = match self.active_app_view {
            Some(AppView::Notes) => self.notes.as_ref().map(|view| view.focus_handle(cx)),
            Some(AppView::Inbox) => self.inbox.as_ref().map(|view| view.focus_handle(cx)),
            Some(AppView::Automations) => {
                self.automations.as_ref().map(|view| view.focus_handle(cx))
            }
            None => return false,
        };
        if let Some(focus) = focus {
            window.focus(&focus, cx);
        }
        true
    }

    pub(super) fn open_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_app_view(window, cx);
        if self.notes.is_none() {
            let view = cx.new(|cx| notes::Notes::new(self.engine.clone(), cx));
            self.app_view_events.push(cx.subscribe_in(
                &view,
                window,
                |this, _, event: &notes::Event, window, cx| match event {
                    notes::Event::Close => this.close_app_view(window, cx),
                    notes::Event::OpenSource(key) => match this.mirror.row_for_key(key) {
                        Some(id) => this.open(id, window, cx),
                        None => {
                            if let Some(notes) = &this.notes {
                                notes.update(cx, |notes, cx| notes.source_missing(cx));
                            }
                        }
                    },
                    notes::Event::AddToChat(note) => this.note_to_chat(note.clone(), window, cx),
                },
            ));
            self.app_view_events
                .push(cx.observe(&view, |this, notes, cx| {
                    if let Some(count) = notes.read(cx).count()
                        && this.note_count != Some(count)
                    {
                        this.note_count = Some(count);
                        this.notify_sidebar(cx);
                    }
                }));
            self.notes = Some(view);
        }
        if let Some(notes) = &self.notes {
            let cwd = self.workspace.project.clone();
            let projects = self.workspace.projects.clone();
            notes.update(cx, |notes, cx| {
                notes.set_appearances(self.workspace.project_appearance.clone(), cx);
                notes.set_context(cwd, projects, cx);
                notes.refresh(cx);
            });
        }
        self.show_app_view(AppView::Notes, window, cx);
    }

    pub(super) fn open_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_app_view(window, cx);
        self.ensure_background_inbox(window, cx);
        if let Some(inbox) = &self.inbox {
            inbox.update(cx, |inbox, cx| inbox.refresh(cx));
        }
        self.show_app_view(AppView::Inbox, window, cx);
    }

    pub(super) fn ensure_background_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox.is_none() {
            let projects = self.workspace.projects.iter().map(Into::into).collect();
            let cwd = self.workspace.project.clone().into();
            let view = cx.new(|cx| inbox::Inbox::new(self.engine.clone(), projects, cwd, cx));
            self.app_view_events.push(cx.subscribe_in(
                &view,
                window,
                |this, _, event: &inbox::Event, window, cx| match event {
                    inbox::Event::Close => this.close_app_view(window, cx),
                    inbox::Event::OpenSession(id) => this.open(*id, window, cx),
                    inbox::Event::StartSession { item, prompt } => {
                        this.start_inbox_session(item.as_ref().clone(), prompt.clone(), cx);
                    }
                    inbox::Event::ItemsChanged(items) => {
                        this.inbox_automation_events(items.clone(), cx)
                    }
                },
            ));
            self.app_view_events
                .push(cx.observe(&view, |this, inbox, cx| {
                    if let Some(automations) = &this.automations {
                        let connections = inbox
                            .read(cx)
                            .connection_statuses()
                            .into_iter()
                            .map(|(provider, (connected, _))| (provider, connected))
                            .collect();
                        automations
                            .update(cx, |view, cx| view.set_trigger_connections(connections, cx));
                    }
                    this.invalidate_chrome(cx);
                    this.sync_settings(cx);
                    cx.notify();
                }));
            self.inbox = Some(view);
        }
        if let Some(inbox) = &self.inbox {
            let projects = self.workspace.projects.iter().map(Into::into).collect();
            let cwd = self.workspace.project.clone().into();
            inbox.update(cx, |inbox, cx| {
                inbox.set_appearances(self.workspace.project_appearance.clone(), cx);
                inbox.set_context(projects, cwd, cx);
            });
        }
    }

    pub(super) fn open_automations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_app_view(window, cx);
        if self.automations.is_none() {
            let cwd = self.workspace.project.clone();
            let projects = self.workspace.projects.clone();
            let engine = self.engine.clone();
            let view = cx.new(|cx| automations::Automations::new(engine, cwd, projects, cx));
            self.app_view_events.push(cx.subscribe_in(
                &view,
                window,
                |this, _, event: &automations::Event, window, cx| match event {
                    automations::Event::Close => this.close_app_view(window, cx),
                    automations::Event::Launch(run) => {
                        this.launch_automation(run.as_ref().clone(), true, cx)
                    }
                    automations::Event::OpenSession(key) => {
                        if let Some(id) = this.mirror.row_for_key(key) {
                            this.open(id, window, cx);
                        }
                    }
                },
            ));
            self.automations = Some(view);
        }
        if let Some(view) = &self.automations {
            let cwd = self.workspace.project.clone();
            let projects = self.workspace.projects.clone();
            view.update(cx, |view, cx| {
                view.set_appearances(self.workspace.project_appearance.clone(), cx);
                let connections = self
                    .inbox
                    .as_ref()
                    .map(|inbox| inbox.read(cx).connection_statuses())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(provider, (connected, _))| (provider, connected))
                    .collect();
                view.set_trigger_connections(connections, cx);
                view.set_context(cwd, projects, cx);
                view.refresh(cx);
            });
        }
        self.show_app_view(AppView::Automations, window, cx);
    }

    fn note_to_chat(
        &mut self,
        note: onicode_protocol::notes::Note,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = note
            .source_cwd
            .clone()
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| self.workspace.project.clone());
        let choice = self.new_chat_choice(&cwd);
        let kind = choice.kind;
        let settings = onicode_protocol::agent::Settings {
            model: choice.model,
            effort: crate::conversation::preferred_effort(kind.id(), cx),
            access: self
                .workspace
                .preferences
                .engine
                .default_access
                .as_deref()
                .map(onicode_protocol::agent::AccessMode::from_id)
                .unwrap_or_default(),
            account: crate::accounts::get(cx)
                .map(|accounts| accounts.read(cx).selected(kind, &cwd))
                .unwrap_or_default(),
            ..Default::default()
        };
        self.leave_app_views(cx);
        self.navigation_revision += 1;
        let revision = self.navigation_revision;
        let title = (!note.title.trim().is_empty()).then(|| note.title.clone());
        let work = self.create_in_engine(
            kind.id(),
            &cwd,
            title,
            onicode_protocol::workspace::NewSession::Settings(Box::new(settings.clone())),
            Vec::new(),
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((summary, _)) => {
                    let id = summary.id;
                    this.add(summary, cx);
                    this.sessions[&id].update(cx, |view, cx| {
                        view.seed_new_settings(settings, cx);
                        view.input.update(cx, |input, cx| {
                            input.set_text(format!("@note/{} ", note.slug), cx)
                        });
                    });
                    if this.navigation_revision == revision {
                        this.activate_session(id, window, cx);
                    }
                    this.persist(cx);
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
        self.update_chat_visibility(cx);
        self.focus_active(window, cx);
    }

    fn start_inbox_session(
        &mut self,
        item: onicode_protocol::inbox::Item,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        let cwd = if item.project_path.is_empty() {
            self.workspace.project.clone()
        } else {
            item.project_path.clone()
        };
        let choice = self.new_chat_choice(&cwd);
        let kind = choice.kind;
        let settings = onicode_protocol::agent::Settings {
            model: choice.model,
            effort: crate::conversation::preferred_effort(kind.id(), cx),
            access: self
                .workspace
                .preferences
                .engine
                .default_access
                .as_deref()
                .map(onicode_protocol::agent::AccessMode::from_id)
                .unwrap_or_default(),
            account: crate::accounts::get(cx)
                .map(|accounts| accounts.read(cx).selected(kind, &cwd))
                .unwrap_or_default(),
            ..Default::default()
        };
        self.navigation_revision += 1;
        let revision = self.navigation_revision;
        // The folder is looked for on the engine that holds it: another machine's for a peer
        // project, where the session is created too.
        let (engine, folder) = self.engine.for_path(&cwd);
        let found = engine.call::<onicode_protocol::files::Stat>(
            None,
            onicode_protocol::files::Stat { path: folder },
        );
        let work_item = item.work_item();
        let title = format!("{} · {}", item.reference(), item.title);
        cx.spawn(async move |this, cx| {
            let refused = match cx.background_spawn(found).await {
                Ok(stat) if stat.kind == onicode_protocol::files::PathKind::Dir => None,
                Ok(_) => Some("Choose a project folder before starting work on this item".into()),
                Err(error) => Some(error),
            };
            if let Some(error) = refused {
                let _ = this.update(cx, |this, cx| {
                    this.error = Some(error);
                    cx.notify();
                });
                return;
            }
            let Ok(work) = this.update(cx, |this, cx| {
                this.create_in_engine(
                    kind.id(),
                    &cwd,
                    Some(title),
                    onicode_protocol::workspace::NewSession::Settings(Box::new(settings.clone())),
                    Vec::new(),
                    cx,
                )
            }) else {
                return;
            };
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((mut summary, _)) => {
                    let id = summary.id;
                    summary.work_item = Some(work_item.clone());
                    let engine = this.session_engine(id);
                    this.workspace_write_on::<onicode_protocol::workspace::SetWorkItem>(
                        &engine,
                        "link the session",
                        onicode_protocol::workspace::SetWorkItem {
                            session: engine.row(id),
                            item: Some(work_item),
                            if_unlinked: false,
                        },
                        None,
                        cx,
                    );
                    this.add(summary, cx);
                    this.sessions[&id]
                        .update(cx, |view, cx| view.quick_submit(prompt, settings, cx));
                    if this.navigation_revision == revision {
                        this.activate_session(id, window, cx);
                    }
                    this.persist(cx);
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    /// Starting an inbox item in a project on another machine looks for its folder on that
    /// machine, where its session is created, and says what that machine answered; a folder this
    /// Mac lacks is still refused for a local project.
    #[gpui::test]
    fn an_inbox_item_in_a_peer_project_starts_on_its_engine(cx: &mut TestAppContext) {
        let (window, _, _) = crate::shell::tests::window(cx);
        let item = |project: &str| onicode_protocol::inbox::Item {
            title: "Fix the build".into(),
            project_path: project.into(),
            ..Default::default()
        };
        window
            .update(cx, |shell, _, cx| {
                shell.start_inbox_session(item("peer://nStudio/Users/me/app"), "Fix".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        let calls = window
            .update(cx, |shell, _, cx| {
                crate::engine_client::calls_for_test(&shell.engine, cx)
            })
            .unwrap();
        let (_, stat) = calls
            .iter()
            .find(|(name, _)| *name == "Stat")
            .expect("the folder is looked for on the studio");
        assert_eq!(stat["engine"], "nStudio");
        assert_eq!(stat["body"]["path"], "/Users/me/app");
        assert!(
            calls.iter().all(|(name, _)| *name != "CreateSession"),
            "nothing is created while the studio is away"
        );
        window
            .update(cx, |shell, _, cx| {
                let error = shell.error.take().unwrap_or_default();
                assert!(error.contains("offline"), "{error}");
                shell.start_inbox_session(item("/no/such/onicode/folder"), "Fix".into(), cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                assert_eq!(
                    shell.error.as_deref(),
                    Some("Choose a project folder before starting work on this item")
                );
            })
            .unwrap();
    }
}
