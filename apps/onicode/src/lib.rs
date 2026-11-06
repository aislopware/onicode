//! What the `onicode` app/CLI and the `onicode-engine` background engine share
//! that needs the engine: its env-resolved configuration, the allocator, the
//! MCP bridge agents run, the background engine's entry point, and the
//! subcommands that run the engine's code here (`onicode_cli::EngineCommand`).
//! The command line itself, paths, logging and the rest are `onicode_cli`'s.

pub mod background;
pub mod engine_commands;
pub mod ipc_cli;

pub use engine_commands::run_engine_command;
pub use onicode_cli::{
    init_logging, long_running_setup, open_log_file, paths, update_feed_from_env,
};

/// mimalloc, macOS only: libmalloc never returns the streaming churn's
/// high-water pages, so transient allocation became permanent RSS
/// (docs/memory-plan.md §1). Pinned to mimalloc v2 in the workspace manifest —
/// the crate's default v3 has the same pathology (churn retained as permanent
/// RSS, ~6x glibc's growth on identical workloads, no idle recovery). Linux
/// measured flat on glibc, so it keeps the system allocator.
#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// The env-resolved engine configuration shared by `headless`, `status`, `sync` and the
/// background engine, so each reads the data dir and port the daemon uses.
pub fn engine_config_from_env() -> onicode_engine::EngineConfig {
    onicode_engine::EngineConfig {
        data_dir: paths::data_dir(),
        ipc_port: onicode_cli::ipc_port_from_env(),
        default_harness: harness_from_env(),
        update_feed: update_feed_from_env(),
        peers: peers_from_env(),
    }
}

/// `ONICODE_PEERS=0` keeps the engine off the tailnet: tests that run the binary set it, as no
/// test engine may join the real tailnet. On by default.
pub fn peers_from_env() -> bool {
    std::env::var("ONICODE_PEERS").as_deref().map(str::trim) != Ok("0")
}

/// `ONICODE_HARNESS` (kebab-case id) picks the default harness for chats without a
/// config row — `mock` powers the e2e smoke; default `claude-code`.
pub fn harness_from_env() -> onicode_engine::HarnessId {
    match std::env::var("ONICODE_HARNESS").as_deref().map(str::trim) {
        Ok("mock") => onicode_engine::HarnessId::Mock,
        Ok("codex") => onicode_engine::HarnessId::Codex,
        Ok("cursor") => onicode_engine::HarnessId::Cursor,
        Ok("devin") => onicode_engine::HarnessId::Devin,
        Ok("grok") => onicode_engine::HarnessId::Grok,
        Ok("hermes") => onicode_engine::HarnessId::Hermes,
        Ok("pi") => onicode_engine::HarnessId::Pi,
        Ok("antigravity") => onicode_engine::HarnessId::Antigravity,
        Ok("muse") => onicode_engine::HarnessId::Muse,
        _ => onicode_engine::HarnessId::ClaudeCode,
    }
}

/// `onicode mcp`: one bridge per agent session, and it only relays — stdio
/// requests out to the engine over IPC and replies back. Two workers keep a
/// burst of parallel tool calls (large replies to encode) close to per-core
/// speed without a worker per core in every agent's bridge.
pub fn run_mcp() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let mut config = onicode_mcp::McpConfig::from_env();
    // The engine's IPC bearer lives in its data dir (the injected
    // ONICODE_IPC_TOKEN_FILE wins when present).
    config.data_dir.get_or_insert_with(paths::data_dir);
    runtime.block_on(onicode_mcp::run(config))
}
