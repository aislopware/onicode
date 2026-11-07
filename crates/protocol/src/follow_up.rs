//! Durable composer messages: the queue typed while a turn runs, and the draft.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    pub id: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<u64>,
    /// Snapshot transport context without exposing it as the editable message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prepared_prompt: Option<String>,
    pub attachments: Vec<PathBuf>,
    pub mode: Option<String>,
    /// Insertion point for a saved draft, independent of provider history.
    pub at: usize,
    pub error: Option<String>,
    pub steer: bool,
}
impl Message {
    pub fn label(&self) -> String {
        if self.text.trim().is_empty() {
            format!(
                "{} attachment{}",
                self.attachments.len(),
                if self.attachments.len() == 1 { "" } else { "s" }
            )
        } else {
            self.text.trim().to_owned()
        }
    }

    /// Editing ordinary text keeps snapshotted MCP context while its selected
    /// tokens remain present, including after restoring a saved queue.
    pub fn replace_text(&mut self, text: String, prepared: String) {
        let context = self
            .prepared_prompt
            .as_deref()
            .and_then(|prepared| prepared.strip_suffix(self.text.as_str()))
            .filter(|prefix| prefix.starts_with("MCP context:"))
            .map(str::to_owned);
        let tokens = |text: &str| {
            text.lines()
                .filter(|line| !line.trim_start().starts_with('>'))
                .flat_map(str::split_whitespace)
                .map(|word| word.trim_matches(|c: char| "()[]{}.!?;,".contains(c)))
                .filter(|word| word.starts_with("@mcp/"))
                .map(str::to_owned)
                .collect::<std::collections::HashSet<_>>()
        };
        let previous = tokens(&self.text);
        self.prepared_prompt = if prepared != text {
            Some(prepared)
        } else if !previous.is_empty() && previous == tokens(&text) {
            context.map(|prefix| format!("{prefix}{text}"))
        } else {
            None
        };
        self.text = text;
    }

    pub fn can_steer(&self) -> bool {
        self.mode.is_none()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub revision: u64,
    pub messages: Vec<Message>,
    pub paused: bool,
    pub draft: Option<Message>,
}
