//! What a provider is and offers, as every viewer names it: its identity, its own slash
//! commands and the work it leaves running. Running one is the engine's.
use serde::{Deserialize, Serialize};

mod kind;

pub use kind::Kind;

/// A command or subagent left running after the provider yielded its answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundTask {
    pub id: String,
    pub description: String,
    pub agent: bool,
    /// The shell command behind a backgrounded Bash call, when known.
    pub command: Option<String>,
    /// Unix milliseconds.
    pub started_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeCommand {
    pub name: String,
    pub description: String,
    pub invocation: String,
    pub source: Kind,
    pub origin: Option<String>,
    pub aliases: Vec<String>,
    pub input_hint: Option<String>,
    pub subcommands: Vec<NativeSubcommand>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeSubcommand {
    pub name: String,
    pub description: Option<String>,
    pub usage: Option<String>,
}

pub fn native_invocation(kind: Kind, name: &str) -> String {
    if matches!(name, "plan" | "compact" | "add-to-folder" | "visualize") {
        format!("{}:{name}", kind.id())
    } else {
        name.into()
    }
}

pub fn native_prompt(kind: Kind, text: &str) -> String {
    let leading = text.len() - text.trim_start().len();
    let body = &text[leading..];
    let end = body.find(char::is_whitespace).unwrap_or(body.len());
    let prefix = format!("/{}:", kind.id());
    if let Some(name) = body[..end]
        .strip_prefix(prefix.as_str())
        .filter(|name| matches!(*name, "plan" | "compact" | "add-to-folder" | "visualize"))
    {
        return format!("{}/{}{}", &text[..leading], name, &body[end..]);
    }
    text.into()
}

pub fn supports_compact(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Codex
            | Kind::Claude
            | Kind::Pi
            | Kind::Omp
            | Kind::Grok
            | Kind::Devin
            | Kind::OpenCode
    )
}

/// Whether the provider can drop its last turn, for editing it.
pub fn supports_rewind(kind: Kind) -> bool {
    matches!(kind, Kind::Codex | Kind::Pi | Kind::Omp | Kind::OpenCode)
}
