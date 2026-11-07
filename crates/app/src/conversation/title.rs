//! Background session names never join or rewrite the user's provider history.
use onicode_protocol::session_title::{TextModel, display_title};
use onicode_protocol::workspace::GenerateTitle;

use super::*;

impl Conversation {
    /// The background model picked in Settings, else this session's harness.
    /// Only the session's own provider reuses the session's account.
    pub(super) fn background_text_model(&self, cx: &gpui::App) -> TextModel {
        let kind = Kind::from_id(&self.summary.provider);
        let account = self.settings.account.clone();
        let chosen = cx
            .try_global::<crate::settings::Preferences>()
            .and_then(|p| p.background_text_model());
        match chosen {
            Some((chosen, model)) => TextModel {
                account: if chosen == kind {
                    account
                } else {
                    crate::accounts::get(cx)
                        .map(|center| center.read(cx).selected(chosen, &self.summary.cwd))
                        .unwrap_or_default()
                },
                kind: chosen,
                model,
            },
            None => TextModel::automatic(kind, account),
        }
    }

    /// Tauri's sessionDisplayTitle: the harness prefix belongs to persistence,
    /// while every conversation headline shows only the user's title.
    pub(crate) fn display_title(&self) -> String {
        display_title(&self.summary.title, &self.summary.provider).replace('\n', " ")
    }

    pub(crate) fn rename(&mut self, title: String, cx: &mut Context<Self>) {
        self.summary.title = title;
        cx.emit(SessionChanged);
        cx.notify();
    }

    /// Names a blank session after its first prompt: the prompt's start stands in at once, and
    /// the engine's generated title and any pasted work item arrive through the workspace.
    pub(super) fn generate_title(&mut self, message: &str, cx: &mut Context<Self>) {
        if self.summary.has_messages
            || self.summary.title
                != format!(
                    "New {} session",
                    Kind::from_id(&self.summary.provider).label()
                )
        {
            return;
        }
        let placeholder = self.summary.title.clone();
        let seed = message
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(42)
            .collect::<String>();
        if seed.is_empty() {
            return;
        }
        self.summary.title.clone_from(&seed);
        let body = GenerateTitle {
            session: self.summary.id,
            message: message.to_owned(),
            placeholder,
            seed,
            cwd: self
                .settings
                .worktree_cwd
                .clone()
                .unwrap_or_else(|| self.summary.cwd.clone()),
            model: self.background_text_model(cx),
        };
        // Applied or not, the workspace says what the title became.
        crate::engine_client::call::<GenerateTitle>(&self.engine, Some(OpId::new()), body, cx)
            .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_removes_only_the_sessions_own_canonical_provider_prefix() {
        assert_eq!(display_title("codex · Fix sidebar", "codex"), "Fix sidebar");
        assert_eq!(
            display_title("claude · Fix sidebar", "claude"),
            "Fix sidebar"
        );
        assert_eq!(
            display_title("claude · Notes about Claude", "codex"),
            "claude · Notes about Claude"
        );
        assert_eq!(
            display_title("My codex · notes", "codex"),
            "My codex · notes"
        );
        assert_eq!(
            display_title("New feature proposal", "codex"),
            "New feature proposal"
        );
        for title in ["codex", "Codex", "New Codex session"] {
            assert_eq!(display_title(title, "codex"), "New session");
        }
    }
}
