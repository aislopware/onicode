//! Last-turn editing recalls first, and rewinds only when the user resubmits.
use std::path::PathBuf;

use onicode_protocol::provider;
use onicode_protocol::session::{EditLastTurn, EditedTurn, RecallLastTurn, RecalledTurn};
use onicode_protocol::transcript::Row;

use super::*;

#[derive(Default)]
pub(super) struct State {
    active: bool,
    /// The turn the engine recalled and holds for the edit.
    prepared: Option<RecalledTurn>,
    /// A confirmed provider mutation followed by a storage conflict requires a
    /// history reload, rather than silently appending to inconsistent context.
    blocked: bool,
}
impl State {
    pub(super) fn pending(&self) -> bool {
        self.active || self.prepared.is_some() || self.blocked
    }
}

fn latest_user(rows: &[Row]) -> Option<usize> {
    rows.iter().rposition(|row| row.first && row.role == "user")
}
impl Conversation {
    pub(super) fn cancel_edit_last_turn(&mut self, cx: &mut Context<Self>) -> bool {
        if self.editing_last_turn.active || self.editing_last_turn.blocked {
            return false;
        }
        if self.editing_last_turn.prepared.take().is_none() {
            return false;
        }
        self.status = "Ready".into();
        self.run_history_actions(cx);
        cx.notify();
        true
    }

    pub(super) fn defer_edit_transfer(
        &mut self,
        settings: Settings,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.history_actions.push_back(Box::new(move |this, cx| {
            if let Some(prompt) = prompt {
                if this.preserve_new_composer(cx) {
                    this.install_transfer(settings, None, cx);
                    this.enqueue_restored_prompt(prompt, cx);
                } else {
                    this.install_transfer(settings, Some(prompt), cx);
                }
            } else {
                this.install_transfer(settings, None, cx);
            }
        }));
        self.status = "Finish or cancel the current message edit to continue".into();
        cx.notify();
    }

    fn can_prepare_edit(&self, cx: &gpui::App) -> bool {
        self.history_ready()
            && !self.busy
            && !self.stopping
            && !self.editing_last_turn.pending()
            && !self.settings.worktree_removed
            && provider::supports_rewind(Kind::from_id(&self.summary.provider))
            && !self.account_switching
            && !self.preparing_worktree
            && !self.attaching
            && !self.planning_orchestration
            && self.orchestration_progress.is_none()
            && !self.needs_approval()
            && self.questions.is_empty()
            && self.controls.is_none()
            && self.follow_ups.state.messages.is_empty()
            && self.follow_ups.state.draft.is_none()
            && self.follow_ups.edit.is_none()
            && self.follow_ups.sending.is_none()
            && self.follow_ups.requested.is_none()
            && self.follow_ups.state.revision == self.follow_ups.persisted
            && self.input.read(cx).content.is_empty()
            && !self.input.read(cx).is_composing()
            && self.attachments.is_empty()
    }

    pub(super) fn begin_edit_last_turn(
        &mut self,
        turn: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The action is tied to a painted source row. A history restoration may
        // replace that row, so never replay this intent against an empty cache.
        if !self.can_prepare_edit(cx) {
            return;
        }
        let rows = self.transcript.read(cx).source_rows().to_vec();
        if latest_user(&rows) != Some(turn) {
            return;
        }
        let exact = self
            .settings
            .turns
            .iter()
            .max_by_key(|(row, _)| **row)
            .and_then(|(_, info)| info.provider_turn_id.clone());
        self.editing_last_turn.active = true;
        self.status = "Preparing edit…".into();
        self.response_menu = None;
        let binding = self.summary.provider_id.clone();
        let work = self.session_call::<RecallLastTurn>(
            None,
            move |session| RecallLastTurn {
                session,
                turn: exact,
            },
            cx,
        );
        window.focus(&self.input.focus_handle(cx), cx);
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.editing_last_turn.active = false;
                match result {
                    Ok(edit)
                        if this.summary.provider_id == binding
                            && edit.rows == rows
                            && edit.rows == this.transcript.read(cx).source_rows()
                            && this.input.read(cx).content.is_empty()
                            && this.attachments.is_empty()
                            && !this.busy
                            && !this.input.read(cx).is_composing() =>
                    {
                        this.input
                            .update(cx, |input, cx| input.set_text(edit.prompt.clone(), cx));
                        this.attachments = edit.attachments.clone();
                        this.editing_last_turn.prepared = Some(edit);
                        this.status = "Editing last message — send to replace its turn".into();
                    }
                    Ok(_) => {
                        this.status = "Composer or conversation changed; edit cancelled".into()
                    }
                    Err(error) => this.status = format!("Could not edit last message: {error}"),
                }
                if !this.editing_last_turn.pending() {
                    this.run_history_actions(cx);
                }
                cx.emit(SessionChanged);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Called before ordinary send consumes composer text or attachments.
    /// Returns true while this edit owns the submission (including rejection).
    pub(super) fn submit_edited(&mut self, cx: &mut Context<Self>) -> bool {
        if self.editing_last_turn.active || self.editing_last_turn.blocked {
            return true;
        }
        if let Some(reason) = self.edit_recovery.clone() {
            self.editing_last_turn.blocked = true;
            self.status = reason;
            cx.notify();
            return true;
        }
        let Some(edit) = self.editing_last_turn.prepared.clone() else {
            return false;
        };
        if self.input.read(cx).content.is_empty() && self.attachments.is_empty() {
            // Clearing the recalled prompt cancels the edit without touching
            // either provider or history. The next new prompt sends normally.
            self.editing_last_turn.prepared = None;
            self.status = "Ready".into();
            self.run_history_actions(cx);
            cx.notify();
            return true;
        }
        let text = self.input.read(cx).content.trim();
        if text.eq_ignore_ascii_case("/compact")
            || text.eq_ignore_ascii_case("/mcp")
            || ["btw", "draft", "add-to-folder"]
                .iter()
                .any(|command| onicode_protocol::composer::consume_command(text, command).is_some())
            || self.draft_active(cx)
        {
            self.status = "Cancel message editing before running a local command".into();
            cx.notify();
            return true;
        }
        if !self.input.read(cx).has_message_text() && self.attachments.is_empty() {
            return true;
        }
        if self.busy
            || self.stopping
            || self.attaching
            || self.account_switching
            || self.preparing_worktree
            || self.settings.worktree_removed
            || self.input.read(cx).is_composing()
            || !self.follow_ups.state.messages.is_empty()
            || self.follow_ups.state.draft.is_some()
            || self.follow_ups.edit.is_some()
            || self.follow_ups.sending.is_some()
            || self.follow_ups.state.revision != self.follow_ups.persisted
            || edit.rows != self.transcript.read(cx).source_rows()
            || self.summary.provider_id.as_deref() != Some(edit.provider_id.as_str())
            || self.summary.provider != edit.provider
            || self.settings.account.id != edit.account_id
            || self.engine.path_of(self.execution_cwd()) != edit.cwd
        {
            self.status = "Conversation changed; cancel the edit before sending".into();
            cx.notify();
            return true;
        }
        let prompt = self.input.read(cx).content.to_string();
        let attachments = self.attachments.clone();
        let checked_attachments = attachments.clone();
        let model = self.model.clone();
        self.editing_last_turn.active = true;
        // Keep the visible draft in place until confirmation, including on a
        // rejected call. IME composition was checked before making it read-only.
        self.input.update(cx, |input, _| input.set_read_only(true));
        self.status = "Rewinding last turn…".into();
        // The engine rewinds the provider, and only once it confirms drops the turn.
        let work = self.session_call::<EditLastTurn>(
            Some(OpId::new()),
            move |session| EditLastTurn {
                session,
                attachments: checked_attachments,
                model,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = match work.await {
                Ok(EditedTurn {
                    outcome: Outcome::Applied,
                    provider_id,
                }) => Ok(provider_id),
                Ok(EditedTurn {
                    outcome: Outcome::Rejected(Rejection::Denied(error)),
                    ..
                }) => Err(error),
                Ok(EditedTurn {
                    outcome: Outcome::Rejected(Rejection::Gone),
                    ..
                }) => Err("the edit is no longer held".into()),
                Ok(EditedTurn {
                    outcome: Outcome::Rejected(rejection),
                    ..
                }) => Err(format!("{rejection:?}")),
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                this.editing_last_turn.active = false;
                this.input.update(cx, |input, _| input.set_read_only(false));
                match result {
                    Ok(binding) => {
                        this.editing_last_turn.prepared = None;
                        this.summary.provider_id = binding;
                        this.settings.turns.retain(|&row, _| row < edit.start);
                        this.transcript.update(cx, |view, cx| {
                            view.replace_history(edit.rows[..edit.start].to_vec(), cx)
                        });
                        this.summary.has_messages = edit.start > 0;
                        this.context = None;
                        if this.input.read(cx).content.as_str() == prompt
                            && this.attachments == attachments
                        {
                            // Normal submit still owns mode commands, attachments,
                            // account checks, durable acceptance and failure recovery.
                            this.send(cx);
                        } else {
                            // Programmatic composer updates can bypass read-only.
                            // Preserve both the new composer and the accepted edit.
                            this.preserve_interrupted_edit(prompt, attachments, edit.start, cx);
                        }
                        this.run_history_actions(cx);
                    }
                    Err(error) => {
                        if error.starts_with("Provider recovery required:") {
                            this.editing_last_turn.blocked = true;
                            this.editing_last_turn.prepared = None;
                            this.edit_recovery = Some(error.clone());
                            this.status = format!(
                                "{error}. Start a new conversation; your edited draft is preserved here."
                            );
                        } else {
                            this.status = format!(
                                "Could not rewind last turn: {error}. Your edited prompt is preserved."
                            );
                        }
                    }
                }
                cx.emit(SessionChanged);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
        true
    }

    fn preserve_interrupted_edit(
        &mut self,
        text: String,
        attachments: Vec<PathBuf>,
        at: usize,
        cx: &mut Context<Self>,
    ) {
        let message = onicode_protocol::follow_up::Message {
            id: onicode_protocol::MessageId::new().to_string(),
            text,
            attachments,
            at,
            ..Default::default()
        };
        if self.follow_ups.state.draft.is_none() {
            self.follow_ups.state.draft = Some(message);
        } else {
            self.follow_ups.state.paused = true;
            self.follow_ups.state.messages.push(message);
        }
        self.save_follow_ups(cx);
        self.status = "Last turn rewound; the edited prompt was saved as a draft because the composer changed".into();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use onicode_protocol::transcript::Transcript;

    use super::*;

    /// The last turn as `RecallLastTurn` hands it to the conversation.
    fn recalled(session: &Session, rows: &[Row]) -> RecalledTurn {
        RecalledTurn {
            start: 0,
            prompt: "Original".into(),
            attachments: Vec::new(),
            provider: session.provider.clone(),
            provider_id: session.provider_id.clone().unwrap_or_default(),
            account_id: String::new(),
            cwd: session.cwd.clone(),
            rows: rows.to_vec(),
        }
    }

    fn answered() -> Transcript {
        let mut transcript = Transcript::default();
        transcript.begin("user");
        transcript.append("Original");
        transcript.begin("assistant");
        transcript.append("Answer");
        transcript
    }

    #[gpui::test]
    fn cancelled_recall_preserves_history_and_native_provider_binding(cx: &mut TestAppContext) {
        let engine = cx.update(crate::engine_client::Engine::memory);
        let mut session = engine.seed().create("codex", "/project").unwrap();
        engine.seed().provider_id(session.id, "thread").unwrap();
        session.provider_id = Some("thread".into());
        let transcript = answered();
        engine
            .seed()
            .write_rows(session.id, 0, &transcript.rows)
            .unwrap();
        let edit = recalled(&session, &transcript.rows);
        let chat = cx.new(|cx| Conversation::new(session, engine, cx));
        chat.update(cx, |chat, cx| {
            chat.loaded = true;
            chat.transcript
                .update(cx, |view, cx| view.patch(0, transcript.rows.clone(), cx));
            chat.editing_last_turn.prepared = Some(edit);
            assert!(chat.submit_edited(cx));
            assert!(!chat.editing_last_turn.pending());
            assert_eq!(chat.transcript.read(cx).source_rows(), transcript.rows);
            assert_eq!(chat.summary.provider_id.as_deref(), Some("thread"));
            assert_eq!(
                chat.engine.seed().rows(chat.summary.id).unwrap(),
                transcript.rows
            );
        });
    }
    #[gpui::test]
    fn newer_composer_and_saved_draft_survive_interrupted_edit_recovery(cx: &mut TestAppContext) {
        let engine = cx.update(crate::engine_client::Engine::memory);
        let session = engine.seed().create("codex", "/project").unwrap();
        let chat = cx.new(|cx| Conversation::new(session, engine, cx));
        chat.update(cx, |chat, cx| {
            chat.loaded = true;
            chat.input
                .update(cx, |input, cx| input.set_text("New draft".into(), cx));
            chat.attachments.push("/tmp/new.png".into());
            chat.follow_ups.state.draft = Some(onicode_protocol::follow_up::Message {
                id: "saved".into(),
                text: "Existing draft".into(),
                ..Default::default()
            });
            chat.preserve_interrupted_edit("Edited prompt".into(), Vec::new(), 0, cx);
            assert_eq!(chat.input.read(cx).content, "New draft");
            assert_eq!(chat.attachments, vec![PathBuf::from("/tmp/new.png")]);
            assert_eq!(
                chat.follow_ups.state.draft.as_ref().unwrap().text,
                "Existing draft"
            );
            assert_eq!(chat.follow_ups.state.messages[0].text, "Edited prompt");
            assert!(chat.follow_ups.state.paused);
        });
    }
    #[gpui::test]
    fn local_commands_and_cancel_never_remove_recalled_history(cx: &mut TestAppContext) {
        let engine = cx.update(crate::engine_client::Engine::memory);
        let mut session = engine.seed().create("codex", "/project").unwrap();
        engine.seed().provider_id(session.id, "thread").unwrap();
        session.provider_id = Some("thread".into());
        let transcript = answered();
        engine
            .seed()
            .write_rows(session.id, 0, &transcript.rows)
            .unwrap();
        let edit = recalled(&session, &transcript.rows);
        let chat = cx.new(|cx| Conversation::new(session, engine, cx));
        chat.update(cx, |chat, cx| {
            chat.loaded = true;
            chat.transcript
                .update(cx, |view, cx| view.patch(0, transcript.rows.clone(), cx));
            chat.editing_last_turn.prepared = Some(edit);
            chat.input
                .update(cx, |input, cx| input.set_text("/compact".into(), cx));
            assert!(chat.submit_edited(cx));
            assert!(chat.editing_last_turn.pending());
            assert_eq!(chat.input.read(cx).content, "/compact");
            assert!(chat.cancel_edit_last_turn(cx));
            assert!(!chat.editing_last_turn.pending());
            assert_eq!(chat.input.read(cx).content, "/compact");
            assert_eq!(
                chat.engine.seed().rows(chat.summary.id).unwrap(),
                transcript.rows
            );
        });
    }
    #[gpui::test]
    fn persisted_recovery_guard_preserves_a_new_composer_before_normal_send(
        cx: &mut TestAppContext,
    ) {
        let engine = cx.update(crate::engine_client::Engine::memory);
        let mut session = engine.seed().create("pi", "/project").unwrap();
        engine.seed().provider_id(session.id, "original").unwrap();
        session.provider_id = Some("original".into());
        let chat = cx.new(|cx| Conversation::new(session, engine, cx));
        chat.update(cx, |chat, cx| {
            chat.loaded = true;
            // As `ReadSession` loads it after an edit the engine could not reconcile.
            chat.edit_recovery = Some(
                "A previous edit changed provider context before local history could be \
                 reconciled. Start a new conversation; the saved history is preserved here."
                    .into(),
            );
            chat.input.update(cx, |input, cx| {
                input.set_text("Unsent edited prompt".into(), cx)
            });
            assert!(chat.submit_edited(cx));
            assert!(chat.editing_last_turn.blocked);
            assert_eq!(chat.input.read(cx).content, "Unsent edited prompt");
            assert!(!chat.cancel_edit_last_turn(cx));
        });
    }
}
