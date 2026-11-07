//! Runs use the same conversation lifecycle as an ordinary native submission. The engine claims
//! due runs and Inbox-triggered ones and records how each ended; the shell starts each in a
//! conversation (`PrepareAutomationRun`, then the prompt) and reports its end
//! (`FinishAutomationRun`).
use onicode_protocol::automations::DueAutomationRun;
use onicode_protocol::work::HasEventAutomations;
use onicode_protocol::workspace::{ClaimInboxEvents, FinishAutomationRun, PrepareAutomationRun};
use onicode_protocol::{Outcome, Rejection};

use super::*;

pub(super) struct Running {
    run_id: String,
    submitting: bool,
    generation_before: u64,
}

#[derive(Default)]
pub(super) struct State {
    polling: bool,
    pending: HashSet<String>,
    running: HashMap<i64, Running>,
    reserved: HashSet<i64>,
    pub session_automations: HashMap<i64, String>,
}

impl Shell {
    /// Starts the automations' background work once the window is up: the Inbox polls when an
    /// automation waits on its events, and a saved app tab opens again.
    pub(super) fn initialize_automations(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                if !cfg!(test) {
                    this.ensure_background_inbox(window, cx);
                }
                this.poll_automations(cx);
                if this.navigation_revision == 0
                    && this.active_app_view.is_none()
                    && this.settings.is_none()
                {
                    let saved = this.workspace.scope_focus.get(&this.tab_scope()).cloned();
                    if let Some(tab @ TitleTab::App(_)) = saved {
                        this.open_title_tab(&tab, window, cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Settles the runs whose conversations finished, and keeps the Inbox polling in the
    /// background while an automation waits on its events.
    pub(super) fn poll_automations(&mut self, cx: &mut Context<Self>) {
        // A rejected or deferred submission still needs a terminal receipt.
        let sessions: Vec<_> = self.automation_state.running.keys().copied().collect();
        for id in sessions {
            self.automation_session_changed(id, cx);
        }
        if self.automation_state.polling || cfg!(test) {
            return;
        }
        self.automation_state.polling = true;
        let work = crate::engine_client::call::<HasEventAutomations>(
            &self.engine,
            None,
            HasEventAutomations {},
            cx,
        );
        cx.spawn(async move |this, cx| {
            let has_events = work.await.unwrap_or(false);
            let _ = this.update_in(cx, |this, window, cx| {
                this.automation_state.polling = false;
                if has_events {
                    this.ensure_background_inbox(window, cx);
                }
            });
        })
        .detach();
    }

    /// Inbox items that appeared: the engine claims the event automations they trigger, and
    /// retries a failed claim on its own.
    pub(super) fn inbox_automation_events(
        &mut self,
        items: Vec<onicode_protocol::inbox::Item>,
        cx: &mut Context<Self>,
    ) {
        if items.is_empty() {
            return;
        }
        self.workspace_write::<ClaimInboxEvents>(
            "evaluate Inbox automations",
            ClaimInboxEvents { items },
            None,
            cx,
        );
    }

    pub(super) fn launch_automation(
        &mut self,
        due: DueAutomationRun,
        reveal: bool,
        cx: &mut Context<Self>,
    ) {
        if self.automation_state.pending.contains(&due.run.id)
            || self
                .automation_state
                .running
                .values()
                .any(|run| run.run_id == due.run.id)
        {
            return;
        }
        self.automation_state.pending.insert(due.run.id.clone());
        let automation = &due.automation;
        let kind = Kind::from_id(&automation.harness);
        let reuse = automation
            .reuse_session
            .then_some(automation.last_session_id.as_deref())
            .flatten()
            .and_then(|key| self.mirror.row_for_key(key))
            .filter(|id| {
                !self.automation_state.running.contains_key(id)
                    && !self.automation_state.reserved.contains(id)
            })
            .filter(|id| {
                self.session_summaries.get(id).is_some_and(|summary| {
                    summary.provider == automation.harness
                        && !summary.archived
                        && !self.draft_sessions.contains(id)
                        && self
                            .sessions
                            .get(id)
                            .is_none_or(|view| view.read(cx).available_for_managed_run(cx))
                        && match automation.workspace_mode.as_str() {
                            "current" => {
                                self.session_worktrees
                                    .get(id)
                                    .and_then(Option::as_deref)
                                    .is_none()
                                    && onicode_protocol::files::path_key(&summary.cwd)
                                        == onicode_protocol::files::path_key(&automation.cwd)
                            }
                            "existing" => self
                                .session_worktrees
                                .get(id)
                                .and_then(Option::as_deref)
                                .is_some_and(|cwd| {
                                    onicode_protocol::files::path_key(cwd)
                                        == onicode_protocol::files::path_key(
                                            &automation.worktree_cwd,
                                        )
                                }),
                            _ => false,
                        }
                })
            });
        if let Some(id) = reuse {
            self.ensure_session(id, cx);
        }
        let account = crate::accounts::get(cx)
            .map(|accounts| accounts.read(cx).selected(kind, &automation.cwd))
            .unwrap_or_default();
        if let Some(id) = reuse {
            self.automation_state.reserved.insert(id);
        }
        let mut settings = reuse
            .and_then(|id| {
                self.sessions
                    .get(&id)
                    .map(|view| view.read(cx).settings.clone())
            })
            .unwrap_or_default();
        settings.account = account;
        settings.model = onicode_protocol::sessions::native_model(&automation.model, kind.id());
        settings.model_settings = automation.model_settings.clone();
        settings.effort = [
            "effort",
            "reasoning",
            "reasoningEffort",
            "reasoning_effort",
            "variant",
        ]
        .into_iter()
        .find_map(|key| automation.model_settings.get(key).cloned());
        settings.fast = automation
            .model_settings
            .get("fast")
            .is_some_and(|value| value == "true");
        settings.service_tier = automation
            .model_settings
            .get("serviceTier")
            .or_else(|| automation.model_settings.get("service_tier"))
            .cloned();
        settings.thinking = automation
            .model_settings
            .get("thinking")
            .and_then(|value| value.parse().ok());
        settings.access = onicode_protocol::agent::AccessMode::from_id(&automation.runtime_mode);
        settings.new_worktree = automation.workspace_mode == "worktree";
        settings.worktree_base = (automation.workspace_mode == "worktree").then(|| "HEAD".into());
        settings.worktree_cwd =
            (automation.workspace_mode == "existing").then(|| automation.worktree_cwd.clone());
        settings.plan = false;
        settings.prepared_prompt = None;
        let revision = self.navigation_revision;
        let body = PrepareAutomationRun {
            due: Box::new(due.clone()),
            reuse,
            settings: Box::new(settings),
        };
        self.workspace_call::<PrepareAutomationRun>(body, None, cx, move |this, reply, cx| {
            this.automation_state.pending.remove(&due.run.id);
            if let Some(id) = reuse {
                this.automation_state.reserved.remove(&id);
            }
            let created = match reply {
                Ok(created) => match created.outcome {
                    Outcome::Applied => created.session.zip(created.settings),
                    // The engine recorded why the run could not start.
                    Outcome::Rejected(Rejection::Denied(error)) => {
                        if reveal {
                            this.error = Some(error);
                        }
                        return;
                    }
                    Outcome::Rejected(_) => return,
                },
                Err(error) => {
                    this.error = Some(format!("Could not start the automation: {error}"));
                    return;
                }
            };
            let Some((summary, settings)) = created else {
                return;
            };
            let summary = summary.session;
            let settings = *settings;
            let id = summary.id;
            if reuse.is_some()
                && this
                    .sessions
                    .get(&id)
                    .is_none_or(|view| !view.read(cx).available_for_managed_run(cx))
            {
                let mut retry = due.clone();
                retry.automation.reuse_session = false;
                this.launch_automation(retry, reveal, cx);
                return;
            }
            let linked = summary.work_item.clone();
            this.add(summary, cx);
            if let Some(linked) = linked {
                this.sessions[&id].update(cx, |view, _| view.summary.work_item = Some(linked));
            }
            this.automation_state
                .session_automations
                .insert(id, due.automation.id.clone());
            this.automation_state.running.insert(
                id,
                Running {
                    run_id: due.run.id.clone(),
                    submitting: true,
                    generation_before: this.sessions[&id].read(cx).submission_generation,
                },
            );
            if !due.automation.session_folder_id.is_empty() {
                // The engine filed it; the sidebar shows it there now.
                this.groups
                    .entry(due.automation.cwd.clone())
                    .or_default()
                    .add_to_folder(&due.automation.session_folder_id, id);
            }
            // Background runs append a tab without changing the focused project.
            if !this.workspace.tabs.contains(&id) {
                this.workspace.tabs.push(id);
                this.workspace.title_order.push(TitleTab::Session(id));
            }
            let prompt = due
                .run
                .prompt
                .clone()
                .unwrap_or_else(|| due.automation.prompt.clone());
            this.sessions[&id].update(cx, |view, cx| view.quick_submit(prompt, settings, cx));
            if let Some(run) = this.automation_state.running.get_mut(&id) {
                run.submitting = false;
            }
            this.automation_session_changed(id, cx);
            if reveal && this.navigation_revision == revision {
                let reveal = cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.activate_session(id, window, cx);
                    });
                });
                reveal.detach();
            }
            this.persist(cx);
        });
    }

    pub(super) fn automation_session_changed(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(run) = self.automation_state.running.get(&id) else {
            return;
        };
        if run.submitting {
            return;
        }
        let Some(view) = self.sessions.get(&id) else {
            return;
        };
        let view = view.read(cx);
        if view.external_submission_pending() {
            return;
        }
        let cancelled = view.notification_was_cancelled();
        let (mut ok, mut message) = view.external_outcome(cx);
        if view.submission_generation == run.generation_before {
            ok = false;
            message = "The selected agent session could not start this run.".into();
        }
        let status = if cancelled {
            "cancelled"
        } else if ok {
            "succeeded"
        } else {
            "failed"
        };
        let run = self.automation_state.running.remove(&id).unwrap();
        self.finish_automation_run(
            run.run_id,
            status,
            (!ok && !cancelled).then_some(message),
            cx,
        );
    }

    fn finish_automation_run(
        &mut self,
        run: String,
        status: &str,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let body = FinishAutomationRun {
            run,
            status: status.to_string(),
            error,
        };
        self.workspace_call::<FinishAutomationRun>(body, None, cx, |this, reply, cx| {
            if let Err(error) = reply {
                this.error = Some(format!("Could not save automation result: {error}"));
            }
            if let Some(view) = &this.automations {
                view.update(cx, |view, cx| view.refresh(cx));
            }
        });
    }
}
