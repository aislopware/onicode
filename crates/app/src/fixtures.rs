//! Local reference layout for the chat presentation.
use onicode_protocol::tool::{ToolCall, ToolState};
use onicode_protocol::transcript::Transcript;
pub(crate) fn chat() -> Transcript {
    let mut t = Transcript::default();
    t.begin("assistant");
    t.append("The new regression tests pass. They cover Markdown across storage chunks, repeated tool snapshots, and expanding reasoning while output changes. I also fixed a scroll issue where opening a disclosure could jump to the tail, and limited syntax highlighting to one background job at a time.");
    for (id, name, input, output, state) in [
        (
            "read",
            "Read",
            r#"{"file_path":"docs/native/migration.md"}"#,
            "",
            ToolState::Completed,
        ),
        (
            "edit1",
            "Edit",
            r#"{"file_path":"crates/monocode-core/src/provider.rs"}"#,
            "",
            ToolState::Completed,
        ),
        (
            "cmd1",
            "Bash",
            r#"{"command":"python3 — <<'PY'"}"#,
            "",
            ToolState::Completed,
        ),
        (
            "cmd2",
            "Bash",
            r#"{"command":"cargo fmt --all"}"#,
            "error: let chains are only allowed in Rust 2024 or later",
            ToolState::Failed,
        ),
        (
            "edit2",
            "Edit",
            r#"{"file_path":"crates/monocode-core/src/provider.rs"}"#,
            "",
            ToolState::Completed,
        ),
        (
            "find",
            "Grep",
            r#"{"pattern":"fn simulate_click|fn bounds_for|debug_bounds"}"#,
            "",
            ToolState::Running,
        ),
    ] {
        t.begin("tool");
        t.rows.last_mut().unwrap().text = serde_json::to_string(&ToolCall {
            id: id.into(),
            name: name.into(),
            input: input.into(),
            output: output.into(),
            state,
            panel: None,
            display: None,
        })
        .unwrap();
    }
    t
}
