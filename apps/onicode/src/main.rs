//! onicode — headed by default; `onicode headless` runs the engine alone. Both start
//! local-only without credentials. `onicode login` and `onicode logout` select the
//! profile used by the next engine start without mutating a live runtime.
//!
//! The command line is `onicode_cli`'s (shared with the engine helper and the
//! zgx interface). This binary links the engine, so it runs the engine's
//! subcommands itself; with no subcommand it opens the window.

#![cfg_attr(windows, windows_subsystem = "windows")]

use onicode_app::{edge_url_from_env, paths, workos_client_id_from_env};
use onicode_cli::{Engines, Launch};

fn main() -> anyhow::Result<()> {
    match onicode_cli::run(env!("CARGO_PKG_VERSION"), Engines::InProcess) {
        Launch::Done(result) => result,
        Launch::Engine(command) => onicode_app::run_engine_command(command),
        #[cfg(target_os = "linux")]
        Launch::Appshot => onicode_ui::appshots::request_running_appshot(
            &onicode_app::engine_config_from_env().data_dir,
        )
        .map_err(anyhow::Error::msg),
        Launch::Window { open_url } => {
            let edge_token = std::env::var("ONICODE_EDGE_TOKEN").ok();
            // Headed: the UI probes ONICODE_IPC_PORT and connects to a running
            // daemon, or embeds the engine in-process (ARCHITECTURE §1).
            onicode_ui::run_app(onicode_ui::UiConfig {
                data_dir: paths::data_dir(),
                ipc_port: onicode_cli::ipc_port_from_env(),
                edge_url: edge_url_from_env(),
                workos_client_id: workos_client_id_from_env(&edge_token),
                edge_token,
                org_id: std::env::var("ONICODE_ORG_ID").ok(),
                default_harness: onicode_ui::HarnessId::ClaudeCode,
                initial_url: open_url,
            });
            Ok(())
        }
    }
}
