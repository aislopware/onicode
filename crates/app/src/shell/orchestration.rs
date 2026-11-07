//! Orchestrator's reviewed plans create real native sessions. Dependencies and
//! overlapping file scopes gate dispatch (the engine's `orchestration::Run`); approvals stay in
//! each worker's chat. Workers are created with `CreateSession`, and the lead's summary is kept
//! with `SaveOrchestration`.
use onicode_engine::workspace::orchestration::{Run, check_scope};
use onicode_protocol::orchestration;
use onicode_protocol::orchestration::Plan;
use onicode_protocol::workspace::{NewSession, SaveOrchestration};

use super::*;

impl Shell {
    pub(super) fn observe_orchestration(
        &mut self,
        view: &Entity<Conversation>,
        id: i64,
        cx: &mut Context<Self>,
    ) {
        use crate::conversation::composer_features::{StartOrchestration, StopOrchestration};
        self.observations.push(cx.subscribe(
            view,
            move |this, _, event: &StartOrchestration, cx| {
                this.start_orchestration(id, event.0.clone(), cx)
            },
        ));
        self.observations.push(
            cx.subscribe(view, move |this, _, _: &StopOrchestration, cx| {
                this.stop_orchestration(id, cx)
            }),
        );
    }

    fn start_orchestration(&mut self, lead: i64, plan: Plan, cx: &mut Context<Self>) {
        if self.orchestrations.contains_key(&lead) {
            return;
        }
        if let Err(error) = orchestration::validate(&plan) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let Some(view) = self.sessions.get(&lead) else {
            return;
        };
        let conversation = view.read(cx);
        if conversation.busy {
            return;
        }
        let cwd = conversation
            .settings
            .worktree_cwd
            .clone()
            .unwrap_or_else(|| conversation.summary.cwd.clone());
        if self.orchestrations.values().any(|run| run.cwd == cwd) {
            self.error = Some("An orchestration is already using this checkout.".into());
            cx.notify();
            return;
        }
        let run = Run::new(
            plan,
            cwd,
            conversation.summary.provider.clone(),
            conversation.settings.clone(),
        );
        self.orchestrations.insert(lead, run);
        self.schedule_orchestration(lead, cx);
    }

    fn stop_orchestration(&mut self, lead: i64, cx: &mut Context<Self>) {
        if let Some(run) = self.orchestrations.get_mut(&lead) {
            for session in run.stop() {
                if let Some(view) = self.sessions.get(&session) {
                    view.update(cx, |v, cx| v.stop_if_busy(cx));
                }
            }
        }
        self.schedule_orchestration(lead, cx);
    }

    pub(super) fn orchestration_session_changed(&mut self, id: i64, cx: &mut Context<Self>) {
        let leads: Vec<_> = self
            .orchestrations
            .iter()
            .filter(|(lead, run)| **lead == id || run.tasks.iter().any(|t| t.session == Some(id)))
            .map(|(lead, _)| *lead)
            .collect();
        let finished = self
            .sessions
            .get(&id)
            .map(|view| view.read(cx))
            .filter(|view| !view.busy)
            .map(|view| view.external_outcome(cx));
        for lead in leads {
            if let Some(run) = self.orchestrations.get_mut(&lead)
                && let Some((success, report)) = finished.clone()
            {
                run.worker_finished(id, success, report);
            }
            self.schedule_orchestration(lead, cx);
        }
    }

    fn schedule_orchestration(&mut self, lead: i64, cx: &mut Context<Self>) {
        let Some(run) = self.orchestrations.get_mut(&lead) else {
            return;
        };
        let jobs = run.schedule();
        let done = run.done();
        let progress = run.progress();
        let summary = run.summary();
        let stopped = run.stopped;
        let reports = run.reports();
        // The sidebar lists the lead's agents from this, during and after the run.
        if self.orchestration_summaries.get(&lead) != Some(&summary) {
            self.orchestration_summaries.insert(lead, summary.clone());
            self.workspace_write::<SaveOrchestration>(
                "save the orchestration",
                SaveOrchestration { lead, summary },
                None,
                cx,
            );
            self.invalidate_chrome(cx);
        }
        if let Some(view) = self.sessions.get(&lead) {
            view.update(cx, |v, cx| v.set_orchestration_progress(Some(progress), cx));
        }
        if done {
            let ready = self
                .sessions
                .get(&lead)
                .is_some_and(|v| v.read(cx).ready_for_external(cx));
            if stopped || ready {
                self.orchestrations.remove(&lead);
                if let Some(view) = self.sessions.get(&lead) {
                    view.update(cx, |v, cx| {
                        v.orchestration_plan = None;
                        v.set_orchestration_progress(None, cx);
                        if !stopped {
                            v.send_coordinator_report(format!("The orchestration workers have finished. Summarize and verify the combined results, identify incomplete work and report the outcome to the user. Preserve their changes.\n\n{reports}"), cx);
                        }
                    });
                }
            }
        }
        let Some(project) = self
            .sessions
            .get(&lead)
            .map(|view| view.read(cx).summary.cwd.clone())
        else {
            return;
        };
        for job in jobs {
            let index = job.index;
            let mut settings = job.settings;
            settings.worktree_cwd = Some(job.cwd.clone());
            let prompt = orchestration::worker_prompt(&job.task, &job.reports);
            let files = job.task.files.clone();
            let cwd = job.cwd.clone();
            let provider = job.provider.clone();
            let title = format!("Agent · {}", job.task.title);
            let project = project.clone();
            cx.spawn(async move |this, cx| {
                // Existing symlink parents must not redirect an assignment outside the checkout.
                let scope = cx
                    .background_spawn(async move { check_scope(&cwd, &files) })
                    .await;
                let created = match scope {
                    Ok(()) => {
                        let Ok(work) = this.update(cx, |this, cx| {
                            this.create_in_engine(
                                &provider,
                                &project,
                                Some(title),
                                NewSession::Settings(Box::new(settings.clone())),
                                cx,
                            )
                        }) else {
                            return;
                        };
                        work.await
                    }
                    Err(error) => Err(error),
                };
                let _ = this.update(cx, |this, cx| {
                    match created {
                        Ok((summary, _)) => {
                            let id = summary.id;
                            this.add(summary, cx);
                            let start = this
                                .orchestrations
                                .get_mut(&lead)
                                .is_some_and(|run| run.worker_created(index, id));
                            if start {
                                this.sessions[&id].update(cx, |v, cx| {
                                    v.install_transfer(settings, Some(prompt), cx)
                                });
                            }
                        }
                        Err(error) => {
                            if let Some(run) = this.orchestrations.get_mut(&lead) {
                                run.worker_failed(index, error);
                            }
                        }
                    }
                    this.invalidate_chrome(cx);
                    this.schedule_orchestration(lead, cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use onicode_engine::workspace::orchestration::TaskState;
    use onicode_protocol::agent::Settings;
    use onicode_protocol::orchestration::Task;

    use super::*;
    #[gpui::test]
    fn reviewed_dispatch_caps_workers_serializes_scopes_and_stops_before_launch(
        cx: &mut TestAppContext,
    ) {
        let (window, _, lead) = crate::shell::tests::window(cx);
        window
            .update(cx, |shell, _, cx| {
                shell.sessions[&lead].update(cx, |v, cx| {
                    v.install_transfer(Settings::default(), None, cx)
                });
                let tasks = ["src", "src/main.rs", "tests", "docs", "examples", "scripts"]
                    .into_iter()
                    .enumerate()
                    .map(|(ix, file)| Task {
                        id: format!("task-{ix}"),
                        title: format!("Assignment {ix}"),
                        prompt: "Inspect and report".into(),
                        files: vec![file.into()],
                        depends_on: Vec::new(),
                    })
                    .collect();
                shell.start_orchestration(
                    lead,
                    Plan {
                        title: "Review".into(),
                        summary: String::new(),
                        tasks,
                    },
                    cx,
                );
                let run = shell.orchestrations.get(&lead).unwrap();
                assert_eq!(
                    run.tasks
                        .iter()
                        .filter(|t| t.state == TaskState::Starting)
                        .count(),
                    4
                );
                assert!(
                    run.tasks[1].state == TaskState::Pending,
                    "overlapping src/main.rs waits for src"
                );
                shell.stop_orchestration(lead, cx);
                assert!(!shell.orchestrations.contains_key(&lead));
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert!(shell.sessions.values().all(|v| !v.read(cx).busy))
            })
            .unwrap();
    }
}
