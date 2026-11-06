//! `onicode-engine` — the desktop app's background engine, as a process of its
//! own ("Onicode Engine" in Activity Monitor): the app starts it and attaches;
//! sessions keep running with the app closed. Without the UI linked in.
//!
//! It also answers what the engine runs its own executable for: `mcp` (the
//! Onicode MCP server injected into agents) and `--noop-browser` (browser
//! suppression for agents' sign-in flows). And it is the engine helper of an
//! interface that doesn't link the engine (the zgx app): every `onicode`
//! subcommand that runs the engine's code (`headless`, `login`, `status`,
//! `plugin`, …) is handed here with the same arguments, and parsed by the
//! same command line (`onicode_cli`).

#![cfg_attr(windows, windows_subsystem = "windows")]

use onicode_cli::{Engines, Launch};

fn main() -> anyhow::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("--noop-browser") => Ok(()),
        Some("--version" | "-V") => {
            println!("onicode-engine {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        // Which build this binary is (`EngineInfo.build` of an engine it
        // starts): a client compares it with the one running.
        Some("--build-id") => {
            println!("{}", onicode_engine::build_id());
            Ok(())
        }
        // How the app starts it: the background engine.
        None => onicode_app::background::run(),
        Some(_) => match onicode_cli::run(env!("CARGO_PKG_VERSION"), Engines::InProcess) {
            Launch::Done(result) => result,
            Launch::Engine(command) => onicode_app::run_engine_command(command),
            Launch::Window { .. } => {
                anyhow::bail!("onicode-engine has no window: open the Onicode app")
            }
            #[cfg(target_os = "linux")]
            Launch::Appshot => anyhow::bail!("onicode-engine has no window: open the Onicode app"),
        },
    }
}
