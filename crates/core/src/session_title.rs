//! Generated sidebar titles: the provider run that makes one. The prompt and its parsing live in
//! `onicode-protocol`.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub use onicode_protocol::session_title::*;

use crate::agent::Settings;
use crate::provider::{self, Kind, Request};

/// Runs on a worker, outside the user's provider thread and native transcript.
pub fn generate(
    kind: Kind,
    cwd: &Path,
    message: &str,
    cancel: Arc<AtomicBool>,
) -> Option<GeneratedTitle> {
    generate_for(
        TextModel::automatic(kind, Default::default()),
        cwd,
        message,
        cancel,
    )
}
pub fn generate_for(
    text: TextModel,
    cwd: &Path,
    message: &str,
    cancel: Arc<AtomicBool>,
) -> Option<GeneratedTitle> {
    if !supports(text.kind) {
        return None;
    }
    let binary = provider::resolve(text.kind)?;
    let output = provider::run_text(
        Request {
            kind: text.kind,
            binary,
            cwd: cwd.into(),
            prompt: prompt(message),
            provider_id: None,
            model: text.model,
            settings: Settings {
                account: text.account,
                ..Default::default()
            },
        },
        cancel,
    )
    .ok()?;
    parse(&output, message)
}
