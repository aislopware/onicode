//! Operator's project actions.
use onicode_core::app_access as app;
use onicode_core::app_access::bridge::{Call, Server};
use onicode_protocol::OpId;
use onicode_protocol::agent::Settings;
use onicode_protocol::app_access::{
    AppModels, AppNotes, ReadAppConversation, ReadAppReceipt, SaveAppReceipt, StartAppSession,
};
use onicode_protocol::git::{CreateWorktree, NewWorktree, Refs};
use serde_json::{Value, json};

use super::*;

#[cfg(test)]
mod tests;

type Placement = Option<(i64, Edge)>;
enum Effect {
    None,
    Start(Box<(Session, Settings, bool, bool, Placement)>),
    Groups(String, onicode_protocol::session_groups::Groups),
    Archive(i64),
}
impl Shell {
    pub(super) fn init_app_bridge(&mut self, cx: &mut Context<Self>) {
        if cx.is_test() {
            return;
        }
        match Server::start() {
            Ok((server, calls)) => {
                self.app_bridge = Some(server);
                cx.spawn(async move |this, cx| {
                    while let Ok(call) = calls.recv().await {
                        if this
                            .update_in(cx, |this, window, cx| {
                                this.handle_app_call(call, window, cx)
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })
                .detach();
            }
            Err(error) => self.error = Some(format!("Operator app access: {error}")),
        }
    }

    fn finish_app_call(
        &mut self,
        call: Call,
        result: Result<Value, String>,
        cx: &mut Context<Self>,
    ) {
        if !mutating(&call.action) {
            self.app_pending
                .remove(&(call.session, call.request_id.clone()));
            let _ = call.reply.send(result);
            return;
        }
        let fingerprint = app::fingerprint(&call.action, &call.input);
        if self.app_results.len() >= 256
            && let Some(key) = self.app_results.keys().next().cloned()
        {
            self.app_results.remove(&key);
        }
        self.app_results.insert(
            (call.session, call.request_id.clone()),
            (fingerprint.clone(), result.clone()),
        );
        let receipt = json!([fingerprint, result]).to_string();
        // A successful reply is sent only after its receipt reaches durable storage.
        let saved = crate::engine_client::call::<SaveAppReceipt>(
            &self.engine,
            Some(OpId::new()),
            SaveAppReceipt {
                session: call.session,
                request: call.request_id.clone(),
                receipt,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let saved = saved.await.map(|_| ());
            let _ = this.update(cx, |this, _| {
                this.app_pending
                    .remove(&(call.session, call.request_id.clone()));
            });
            let _ = call.reply.send(saved.and(result));
        })
        .detach();
    }

    /// Answers `call` with the engine's JSON reply, which has no effect to show.
    fn app_engine_reply(
        &mut self,
        call: Call,
        reply: gpui::Task<Result<String, String>>,
        cx: &mut Context<Self>,
    ) {
        let work = cx.spawn(async move |_, _| Ok((json_reply(reply.await?)?, Effect::None)));
        self.app_settle(call, work, cx);
    }

    /// Answers `call` with what `work` did, after showing its effect.
    fn app_settle(
        &mut self,
        call: Call,
        work: gpui::Task<Result<(Value, Effect), String>>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let result = result.map(|(value, effect)| {
                    match effect {
                        Effect::None => (),
                        Effect::Start(start) => {
                            let (summary, settings, draft, reveal, placement) = *start;
                            let id = summary.id;
                            this.draft_sessions.insert(id);
                            this.sidebar_hidden_sessions.remove(&id);
                            this.session_worktrees
                                .insert(id, settings.worktree_cwd.clone());
                            this.add(summary, cx);
                            this.started_by.insert(id, call.session);
                            if let Some(view) = this.sessions.get(&id) {
                                view.update(cx, |v, cx| v.install_app_launch(settings, !draft, cx));
                            }
                            this.app_place_launch(id, placement, reveal, window, cx);
                        }
                        Effect::Groups(project, groups) => {
                            this.groups.insert(project, groups);
                        }
                        Effect::Archive(id) => {
                            if let Some(summary) = this.session_summaries.get_mut(&id) {
                                summary.archived = true;
                            }
                            if let Some(view) = this.sessions.get(&id) {
                                view.update(cx, |v, _| v.summary.archived = true);
                            }
                            this.refresh_session_mark(id, cx);
                            if this.workspace.tab_of(id).is_some() {
                                this.close_pane(id, window, cx);
                            }
                        }
                    }
                    this.invalidate_chrome(cx);
                    cx.notify();
                    value
                });
                this.finish_app_call(call, result, cx);
            });
        })
        .detach();
    }

    fn authorize_app_call(&self, call: &Call, cx: &App) -> Result<(), String> {
        app::validate_operator(&call.action, &call.input)?;
        if call.request_id.is_empty()
            || call.request_id.len() > 128
            || !call
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("request ID must use letters, digits, underscores or hyphens".into());
        }
        let caller = self
            .sessions
            .get(&call.session)
            .map(|v| v.read(cx))
            .filter(|v| v.settings.operator_enabled)
            .ok_or("Operator is not enabled for this thread.")?;
        if call
            .input
            .get("project")
            .and_then(Value::as_str)
            .is_some_and(|p| {
                onicode_protocol::files::path_key(p)
                    != onicode_protocol::files::path_key(&caller.summary.cwd)
            })
        {
            return Err("Operator can only access this thread's project.".into());
        }
        if (caller.app_read_only
            || caller.settings.plan
            || caller.settings.access == onicode_protocol::agent::AccessMode::ReadOnly)
            && !matches!(
                call.action.as_str(),
                "models.list"
                    | "sessions.list"
                    | "sessions.read"
                    | "folders.list"
                    | "notes.list"
                    | "notes.read"
                    | "worktrees.list"
            )
        {
            return Err("This thread's access policy allows read-only app actions.".into());
        }
        Ok(())
    }

    fn handle_app_call(&mut self, call: Call, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.authorize_app_call(&call, cx) {
            let _ = call.reply.send(Err(error));
            return;
        }
        let key = (call.session, call.request_id.clone());
        let fingerprint = app::fingerprint(&call.action, &call.input);
        if let Some((hash, result)) = self.app_results.get(&key) {
            let _ = call.reply.send(if *hash == fingerprint {
                result.clone()
            } else {
                Err("Request ID was already used with different input".into())
            });
            return;
        }
        if !self.app_pending.insert(key) {
            let _ = call.reply.send(Err(
                "Request is still running; retry with the same request ID.".into(),
            ));
            return;
        }
        if !mutating(&call.action) {
            self.dispatch_app_call(call, window, cx);
            return;
        }
        let work = crate::engine_client::call::<ReadAppReceipt>(
            &self.engine,
            None,
            ReadAppReceipt {
                session: call.session,
                request: call.request_id.clone(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let saved = work.await.and_then(|receipt| {
                receipt
                    .map(|raw| {
                        serde_json::from_str::<(String, Result<Value, String>)>(&raw)
                            .map_err(|e| e.to_string())
                    })
                    .transpose()
            });
            let _ = this.update_in(cx, |this, window, cx| match saved {
                Ok(Some((hash, result))) => {
                    this.app_pending
                        .remove(&(call.session, call.request_id.clone()));
                    let _ = call.reply.send(if hash == fingerprint {
                        result
                    } else {
                        Err("Request ID was already used with different input".into())
                    });
                }
                Ok(None) => this.dispatch_app_call(call, window, cx),
                Err(error) => this.finish_app_call(call, Err(error), cx),
            });
        })
        .detach();
        let _ = window;
    }

    fn app_target(
        &self,
        input: &Value,
        caller: i64,
        project: &str,
        manage: bool,
    ) -> Result<i64, String> {
        let id = session_id(input, "sessionId", &self.mirror)?;
        let summary = self
            .session_summaries
            .get(&id)
            .ok_or("Session was not found in this project")?;
        if onicode_protocol::files::path_key(&summary.cwd)
            != onicode_protocol::files::path_key(project)
        {
            return Err("Session is outside this project.".into());
        }
        if manage && (id == caller || self.orchestration_worker(id)) {
            return Err("Only other regular project sessions can be managed here".into());
        }
        Ok(id)
    }

    pub(crate) fn app_placement(
        &self,
        input: &Value,
        caller: i64,
        project: &str,
        cx: &App,
    ) -> Result<Placement, String> {
        let edge = match input["placement"].as_str().unwrap_or("tab") {
            "tab" => return Ok(None),
            "right" => Edge::Right,
            "down" => Edge::Bottom,
            _ => return Err("placement must be tab, right or down".into()),
        };
        let id = if input.get("besideSessionId").is_some() {
            session_id(input, "besideSessionId", &self.mirror)?
        } else {
            caller
        };
        if self.workspace.tab_of(id).is_none()
            || self.session_metadata(id, cx).summary.cwd != project
        {
            return Err("besideSessionId must be an open session pane in this project".into());
        }
        Ok(Some((id, edge)))
    }

    pub(crate) fn app_place_launch(
        &mut self,
        id: i64,
        placement: Placement,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some((target, edge)) =
            placement.filter(|(target, _)| self.workspace.tab_of(*target).is_some())
        {
            self.open(target, window, cx);
            let before = self.active_leaves();
            self.workspace.place(id, target, edge);
            self.begin_pane_motion(before, cx);
            self.fit_sidebar(cx);
            self.activate_session(id, window, cx);
        } else if reveal {
            self.open(id, window, cx);
        }
        self.persist(cx);
    }

    fn dispatch_app_call(&mut self, call: Call, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.authorize_app_call(&call, cx) {
            self.finish_app_call(call, Err(error), cx);
            return;
        }
        let caller = self.sessions[&call.session].read(cx);
        let project = caller.summary.cwd.clone();
        let provider = caller.summary.provider.clone();
        let inherited = caller.settings.clone();
        let input = call.input.clone();
        let action = call.action.clone();
        let result: Result<Value, String> = match action.as_str() {
            "sessions.list" => Ok(
                json!({"cwd":project,"sessions":self.order.iter().filter_map(|id|{
                let metadata=self.session_metadata(*id,cx);
                if metadata.summary.cwd!=project || self.orchestration_worker(*id){return None;}
                let live=self.sessions.get(id).map(|v|v.read(cx));
                Some(json!({"id":id,"sessionId":id,"title":metadata.summary.title,"harness":metadata.summary.provider,"model":live.and_then(|v|v.settings.model.clone()).or_else(||self.session_details.get(id).and_then(|m|m.model.clone())),"busy":metadata.busy,"hasDraft":live.is_some_and(|v|v.has_saved_draft() || !v.input.read(cx).content.is_empty()) || self.draft_sessions.contains(id),"archived":metadata.summary.archived}))
            }).collect::<Vec<_>>()}),
            ),
            "models.list" => {
                let profiles: Vec<_> = Kind::ALL
                    .into_iter()
                    .map(|kind| {
                        let profile = if kind.id() == provider {
                            inherited.account.clone()
                        } else {
                            crate::accounts::get(cx)
                                .map(|center| center.read(cx).selected(kind, &project))
                                .unwrap_or_default()
                        };
                        (kind, profile)
                    })
                    .collect();
                let reply = crate::engine_client::call::<AppModels>(
                    &self.engine,
                    None,
                    AppModels { profiles },
                    cx,
                );
                self.app_engine_reply(call, reply, cx);
                return;
            }
            "sessions.start" => {
                let parsed = (|| {
                    let prompt = app::prompt(&input)?;
                    let kind = Kind::parse(input["harness"].as_str().unwrap_or(&provider))
                        .ok_or("Unknown harness")?;
                    let placement = self.app_placement(&input, call.session, &project, cx)?;
                    let mut settings = inherited;
                    if kind.id() != provider {
                        settings.account = crate::accounts::get(cx)
                            .map(|center| center.read(cx).selected(kind, &project))
                            .unwrap_or_default();
                    }
                    Ok::<_, String>((prompt, kind, placement, settings))
                })();
                let (prompt, kind, placement, settings) = match parsed {
                    Ok(p) => p,
                    Err(e) => {
                        self.finish_app_call(call, Err(e), cx);
                        return;
                    }
                };
                let started = crate::engine_client::call::<StartAppSession>(
                    &self.engine,
                    Some(OpId::new()),
                    StartAppSession {
                        caller: call.session,
                        request: call.request_id.clone(),
                        fingerprint: app::fingerprint(&action, &input),
                        input: input.to_string(),
                        provider,
                        kind,
                        project,
                        prompt,
                        settings,
                    },
                    cx,
                );
                let draft = input["draft"].as_bool().unwrap_or(false);
                let reveal = input["reveal"].as_bool().unwrap_or(false);
                let work = cx.spawn(async move |_, _| {
                    let started = started.await?;
                    let effect = match started.started {
                        Some((summary, settings)) => {
                            Effect::Start(Box::new((summary, settings, draft, reveal, placement)))
                        }
                        None => Effect::None,
                    };
                    Ok((json_reply(started.value)?, effect))
                });
                self.app_settle(call, work, cx);
                return;
            }
            "sessions.read" => match self.app_target(&input, call.session, &project, false) {
                Ok(id) => {
                    let busy = self.session_metadata(id, cx).busy;
                    let draft = self
                        .sessions
                        .get(&id)
                        .is_some_and(|v| v.read(cx).has_saved_draft())
                        || self.draft_sessions.contains(&id);
                    let reply = crate::engine_client::call::<ReadAppConversation>(
                        &self.engine,
                        None,
                        ReadAppConversation {
                            session: id,
                            input: input.to_string(),
                            busy,
                            draft,
                        },
                        cx,
                    );
                    self.app_engine_reply(call, reply, cx);
                    return;
                }
                Err(e) => Err(e),
            },
            "sessions.send" | "sessions.draft" | "sessions.stop" | "sessions.archive"
            | "sessions.delete" => {
                match self
                    .app_target(&input, call.session, &project, true)
                    .and_then(|id| {
                        self.ensure_session(id, cx)
                            .map(|view| (id, view))
                            .ok_or("Session not found".into())
                    }) {
                    Err(e) => Err(e),
                    Ok((id, view)) => {
                        if action == "sessions.delete" {
                            self.delete_session(id, window, cx);
                            if !self.deleting.contains(&id) {
                                self.finish_app_call(
                                    call,
                                    Err(self
                                        .error
                                        .clone()
                                        .unwrap_or("Could not delete session".into())),
                                    cx,
                                );
                                return;
                            }
                            let deleting = self.group_writes.take();
                            self.group_writes = Some(cx.spawn(async move |this, cx| {
                                if let Some(deleting) = deleting {
                                    deleting.await;
                                }
                                let _ = this.update(cx, |this, cx| {
                                    let result = if this.session_summaries.contains_key(&id) {
                                        Err("Could not delete session; its data was preserved"
                                            .into())
                                    } else {
                                        Ok(json!({"sessionId":id,"deleted":true}))
                                    };
                                    this.finish_app_call(call, result, cx)
                                });
                            }));
                            return;
                        } else if action == "sessions.archive" {
                            view.update(cx, |v, cx| v.stop_app_session(cx));
                            let archived =
                                crate::engine_client::call::<onicode_protocol::workspace::Archive>(
                                    &self.engine,
                                    Some(onicode_protocol::OpId::new()),
                                    onicode_protocol::workspace::Archive {
                                        session: id,
                                        archived: true,
                                    },
                                    cx,
                                );
                            let work = cx.spawn(async move |_, _| match archived.await? {
                                onicode_protocol::Outcome::Applied => Ok((
                                    json!({"sessionId":id,"archived":true}),
                                    Effect::Archive(id),
                                )),
                                onicode_protocol::Outcome::Rejected(rejection) => {
                                    Err(format!("Could not archive the session: {rejection:?}"))
                                }
                            });
                            self.app_settle(call, work, cx);
                            return;
                        } else if action == "sessions.stop" {
                            view.update(cx, |v, cx| v.stop_app_session(cx));
                            Ok(json!({"sessionId":id,"stopped":true}))
                        } else {
                            view.update(cx, |v, cx| v.load(cx));
                            if !view.read(cx).ready_for_external(cx)
                                || view.read(cx).has_saved_draft()
                            {
                                Err("Session is busy, loading, or has an unsent draft. Its draft was preserved.".into())
                            } else {
                                match app::prompt(&input) {
                                    Err(e) => Err(e),
                                    Ok(prompt) => {
                                        view.update(cx, |v, cx| {
                                            if action == "sessions.draft" {
                                                v.save_app_draft(prompt, cx)
                                            } else {
                                                v.install_transfer(
                                                    v.settings.clone(),
                                                    Some(prompt),
                                                    cx,
                                                )
                                            }
                                        });
                                        Ok(
                                            json!({"sessionId":id,"submitted":action=="sessions.send","saved":action=="sessions.draft"}),
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
            }
            "notes.list" | "notes.read" | "notes.write" => {
                let reply = crate::engine_client::call::<AppNotes>(
                    &self.engine,
                    mutating(&action).then(OpId::new),
                    AppNotes {
                        caller: call.session,
                        request: call.request_id.clone(),
                        action,
                        input: input.to_string(),
                        projects: vec![project],
                    },
                    cx,
                );
                self.app_engine_reply(call, reply, cx);
                return;
            }
            "folders.list" => Ok(
                json!({"cwd":project,"folders":self.groups.get(&project).cloned().unwrap_or_default().folders.iter().map(|f|json!({"id":f.id,"name":f.name,"sessionIds":f.session_ids})).collect::<Vec<_>>()}),
            ),
            "folders.move" => match self.app_target(&input, call.session, &project, false) {
                Err(e) => Err(e),
                Ok(id) => {
                    let mut groups = self.groups.get(&project).cloned().unwrap_or_default();
                    let folder = app::optional_text(&input, "folderId", 256);
                    let name = app::optional_text(&input, "newFolderName", 100);
                    match folder.and_then(|f| name.map(|n| (f, n))) {
                        Err(e) => Err(e),
                        Ok((folder, name)) => {
                            if folder.is_some() == name.is_some() {
                                Err("Supply exactly one of folderId or newFolderName".into())
                            } else {
                                let folder = if let Some(name) = name {
                                    let folder = groups
                                        .create_folder(&[id], onicode_protocol::new_id())
                                        .unwrap();
                                    groups.rename_folder(&folder, &name);
                                    folder
                                } else {
                                    folder.unwrap()
                                };
                                if groups.folder(&folder).is_none() {
                                    Err("Folder not found".into())
                                } else {
                                    groups.add_to_folder(&folder, id);
                                    let name = groups.folder(&folder).unwrap().name.clone();
                                    let saved = crate::engine_client::call::<
                                        onicode_protocol::workspace::SaveGroups,
                                    >(
                                        &self.engine,
                                        Some(onicode_protocol::OpId::new()),
                                        onicode_protocol::workspace::SaveGroups {
                                            project: project.clone(),
                                            groups: groups.clone(),
                                        },
                                        cx,
                                    );
                                    let work = cx.spawn(async move |_, _| {
                                        saved.await?;
                                        Ok::<_, String>((
                                            json!({"sessionId":id,"folderId":folder,"folderName":name}),
                                            Effect::Groups(project, groups),
                                        ))
                                    });
                                    self.app_settle(call, work, cx);
                                    return;
                                }
                            }
                        }
                    }
                }
            },
            "worktrees.list" | "worktrees.create" => {
                let (engine, project) = crate::engine_client::for_project(&project, cx);
                let work = cx.background_executor().spawn(async move {
                    let value = if action == "worktrees.list" {
                        let snapshot = engine.call::<Refs>(None, Refs { checkout: project }).await?;
                        let snapshot = crate::git_cache::respell(&engine, snapshot);
                        json!({"worktrees":snapshot.worktrees.iter().map(worktree_json).collect::<Vec<_>>(),"branches":snapshot.branches.iter().map(|b|json!({"name":b.name,"remote":b.remote})).collect::<Vec<_>>()})
                    } else {
                        let branch = app::text(&input, "branch", 400)?;
                        let base = app::optional_text(&input, "base", 400)?;
                        let existing = input["existing"].as_bool().unwrap_or(false);
                        if existing && base.is_some() {
                            return Err("base cannot be set for an existing branch".into());
                        }
                        let tree = engine
                            .call::<CreateWorktree>(
                                Some(OpId::new()),
                                CreateWorktree {
                                    checkout: project,
                                    worktree: NewWorktree::Project {
                                        branch,
                                        base,
                                        existing,
                                    },
                                },
                            )
                            .await?;
                        let tree = onicode_protocol::git::Worktree {
                            path: engine.app_path(&tree.path),
                            ..tree
                        };
                        worktree_json(&tree)
                    };
                    Ok((value, Effect::None))
                });
                self.app_settle(call, work, cx);
                return;
            }
            _ => Err("Unknown Operator action".into()),
        };
        self.finish_app_call(call, result, cx);
    }
}
fn mutating(action: &str) -> bool {
    !matches!(
        action,
        "models.list"
            | "sessions.list"
            | "sessions.read"
            | "folders.list"
            | "notes.list"
            | "notes.read"
            | "worktrees.list"
    )
}
fn worktree_json(tree: &onicode_protocol::git::Worktree) -> Value {
    json!({"path":tree.path,"branch":tree.branch,"head":tree.head,"main":tree.main,"missing":tree.missing})
}
/// An engine reply's JSON.
fn json_reply(text: String) -> Result<Value, String> {
    serde_json::from_str(&text).map_err(|error| error.to_string())
}
/// The session an action names, by the id the store keeps it under for other tools or by row.
fn session_id(
    input: &Value,
    key: &str,
    mirror: &super::workspace_mirror::Mirror,
) -> Result<i64, String> {
    match input.get(key) {
        Some(Value::String(id)) => mirror.row_for_key(id).map_or_else(
            || id.parse::<i64>().map_err(|_| "Unknown session ID".into()),
            Ok,
        ),
        Some(value) => value.as_i64().ok_or_else(|| format!("Provide {key}")),
        None => Err(format!("Provide {key}")),
    }
}
