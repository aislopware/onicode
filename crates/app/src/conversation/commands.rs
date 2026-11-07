//! Composer commands are handled before taking a draft or entering the turn queue.
use std::time::Duration;

use onicode_protocol::provider;
use onicode_protocol::session::{Compact, Compacted, Stop};
use onicode_protocol::tool::{ToolCall, ToolState};
use onicode_protocol::transcript::Row;

use super::*;

impl Conversation {
    /// Returns true when this input belongs to a local command, including failures.
    /// A rejected command keeps its draft and attachments intact.
    pub(super) fn handle_local_command(&mut self, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).content.trim().to_owned();
        if text.eq_ignore_ascii_case("/mcp") {
            if self.input.read(cx).is_composing() {
                return true;
            }
            self.completion
                .update(cx, |completion, cx| completion.open_mcp(cx));
            return true;
        }
        if self.git_action_command(&text, cx) {
            return true;
        }
        if let Some(request) = onicode_protocol::composer::consume_command(&text, "visualize") {
            self.visualize_command(&text, request, cx);
            return true;
        }
        if let Some(question) = onicode_protocol::composer::consume_command(&text, "btw") {
            let question = question.trim().to_owned();
            self.open_btw(question, false, cx);
            return true;
        }
        if !text.eq_ignore_ascii_case("/compact") {
            return false;
        }
        self.compact_context(cx);
        true
    }

    fn visualize_command(&mut self, text: &str, request: &str, cx: &mut Context<Self>) {
        if self.input.read(cx).is_composing() {
            return;
        }
        if self.busy || self.stopping || !self.follow_ups.state.messages.is_empty() {
            self.status = "Wait for the current turn and queued messages before /visualize.".into();
            cx.notify();
            return;
        }
        if !self.history_ready()
            || self.attaching
            || self.account_switching
            || self.preparing_worktree
            || self.settings.worktree_removed
        {
            self.status = "/visualize is not available in this session.".into();
            cx.notify();
            return;
        }
        let request = self.completion.read(cx).prepare_prompt(request);
        let attachments = std::mem::take(&mut self.attachments);
        let mut message = self.message(text.to_owned(), attachments, None, cx);
        message.prepared_prompt = Some(onicode_core::visual_bridge::requested_prompt(&request));
        self.input.update(cx, |input, cx| {
            input.take(cx);
        });
        self.completion
            .update(cx, |completion, cx| completion.dismiss(cx));
        self.launch_message(message, cx);
    }

    pub(super) fn compact_context(&mut self, cx: &mut Context<Self>) {
        if !self.history_ready() {
            self.defer_history_action(|this, cx| this.compact_context(cx), cx);
            return;
        }
        if self.editing_last_turn.pending()
            || self.stopping
            || self.attaching
            || self.input.read(cx).is_composing()
            || self.account_switching
            || self.controls.is_some()
            || self.needs_approval()
            || !self.questions.is_empty()
        {
            return;
        }
        if self.busy || !self.follow_ups.state.messages.is_empty() {
            self.status =
                "Wait for the current turn and queued messages before compacting context.".into();
            cx.notify();
            return;
        }
        let kind = Kind::from_id(&self.summary.provider);
        if !provider::supports_compact(kind) {
            self.status = format!(
                "Context compaction is not supported by the native {} transport.",
                kind.label()
            );
            cx.notify();
            return;
        }
        if self.summary.provider_id.is_none() {
            self.status = "Send a message before compacting this chat.".into();
            cx.notify();
            return;
        }
        if self.settings.worktree_removed || self.preparing_worktree || !self.loaded {
            return;
        }
        if let Some(reason) = self.edit_recovery.clone() {
            self.status = reason;
            cx.notify();
            return;
        }
        // The engine sets the account's home and the Claude hooks itself.
        let mut settings = self.settings.clone();
        settings.environment.extend(self.app_environment.clone());
        let model = self.model.clone();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.busy = true;
        self.submission_generation = self.submission_generation.wrapping_add(1);
        let generation = self.submission_generation;
        self.status = "Compacting context…".into();
        let compact_start = self.transcript.read(cx).source_rows().len();
        let mut compact_tool = ToolCall::new(
            &format!(
                "manual-compaction-{}-{generation}",
                onicode_protocol::time::now_ms()
            ),
            "contextCompaction",
        );
        let mut compact_row = Row {
            role: "tool".into(),
            text: serde_json::to_string(&compact_tool).unwrap(),
            first: true,
            sent_at: Some(onicode_protocol::time::now_ms()),
            id: None,
        };
        self.transcript.update(cx, |view, cx| {
            view.patch(compact_start, vec![compact_row.clone()], cx);
        });
        let compact_draft = self.input.read(cx).content.clone();
        let clear_command = compact_draft.trim().eq_ignore_ascii_case("/compact");
        let source_provider = self.summary.provider.clone();
        let source_account = self.settings.account.id.clone();
        self.completion
            .update(cx, |completion, cx| completion.dismiss(cx));
        let tool = compact_tool.id.clone();
        let work = self.session_call::<Compact>(
            Some(OpId::new()),
            move |session| Compact {
                session,
                at: compact_start,
                tool,
                model,
                settings,
            },
            cx,
        );
        // Stop sets the conversation's cancel flag; the engine hears it as `Stop`.
        let done = Arc::new(AtomicBool::new(false));
        let finished = done.clone();
        cx.spawn(async move |this, cx| {
            while !finished.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    let _ = this.update(cx, |this, cx| {
                        let stop = |session| Stop { session };
                        this.session_call::<Stop>(Some(OpId::new()), stop, cx)
                            .detach();
                    });
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let reply = work.await;
            done.store(true, Ordering::Relaxed);
            let (result, context, commands, compact_row) = match reply {
                Ok(Compacted {
                    row,
                    context,
                    commands,
                    error,
                }) => (error.map_or(Ok(()), Err), Some(context), commands, row),
                Err(error) => {
                    compact_tool.state = ToolState::Failed;
                    compact_tool.output.clone_from(&error);
                    compact_row.text = serde_json::to_string(&compact_tool).unwrap();
                    (Err(error), None, None, compact_row)
                }
            };
            let _ = this.update(cx, |this, cx| {
                if this.submission_generation != generation {
                    return;
                }
                this.busy = false;
                this.stopping = false;
                this.transcript.update(cx, |view, cx| {
                    view.patch(compact_start, vec![compact_row], cx);
                });
                let source_selected = this.summary.provider == source_provider
                    && this.settings.account.id == source_account;
                if let Some(commands) = commands.filter(|_| source_selected) {
                    this.completion.update(cx, |completion, cx| {
                        completion.set_native_commands(commands, cx)
                    });
                }
                if let Some(context) = context.filter(|_| source_selected) {
                    this.context = Some(context);
                }
                this.status = match result {
                    Ok(()) => {
                        if clear_command && this.input.read(cx).content == compact_draft {
                            this.input.update(cx, |input, cx| {
                                input.take(cx);
                            });
                        }
                        "Ready".into()
                    }
                    Err(error) if error == "Stopped" => "Ready".into(),
                    Err(error) => error,
                };
                if this.status == "Ready" {
                    this.run_history_actions(cx);
                    this.pump_follow_ups(cx);
                }
                cx.emit(SessionChanged);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn visualize_command_preserves_rejected_drafts_and_starts_an_explicit_turn(
        cx: &mut TestAppContext,
    ) {
        let engine = cx.update(crate::engine_client::Engine::memory);
        let mut summary = engine
            .seed()
            .create("codex", "/missing/visual-command")
            .unwrap();
        summary.provider = "fixture".into();
        let window = cx.update(|cx| {
            input::bind(cx);
            crate::popover::bind(cx);
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut view = Conversation::new(summary, engine, cx);
                    view.loaded = true;
                    view.summary.has_messages = true;
                    view
                })
            })
            .unwrap()
        });
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(visual, |view, _, cx| {
                for text in ["/visualizer", "Explain /visualize", "> /visualize"] {
                    view.input
                        .update(cx, |input, cx| input.set_text(text.into(), cx));
                    assert!(!view.handle_local_command(cx));
                }
                view.busy = true;
                view.attachments = vec![PathBuf::from("/tmp/visual-context.txt")];
                view.input
                    .update(cx, |input, cx| input.set_text("/visualize".into(), cx));
                view.send(cx);
                assert!(view.follow_ups.state.messages.is_empty());
                assert_eq!(view.input.read(cx).content, "/visualize");
                assert_eq!(view.attachments.len(), 1);
                assert!(view.status.contains("Wait for the current turn"));

                view.busy = false;
                view.attachments.clear();
                view.input.update(cx, |input, cx| {
                    input.set_text("/Visualize Compare the two approaches".into(), cx)
                });
                view.send(cx);
                assert!(view.busy);
                assert!(view.input.read(cx).content.is_empty());
                let rows = view.transcript.read(cx).source_rows();
                assert_eq!(
                    rows.iter()
                        .filter(|row| row.is_prompt())
                        .map(|row| row.text.as_str())
                        .collect::<String>(),
                    "/Visualize Compare the two approaches"
                );
            })
            .unwrap();
    }
}
