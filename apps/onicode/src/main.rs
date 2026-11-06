//! onicode — headed by default; `onicode headless` runs the engine alone. Either serves this
//! device's profile and joins the user's other engines on the tailnet.
//!
//! The command line is `onicode_cli`'s (shared with the engine helper and the
//! zgx interface). This binary links the engine, so it runs the engine's
//! subcommands itself; with no subcommand it opens the window.

use onicode_app::{paths, update_feed_from_env};
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
            // Headed: the UI probes ONICODE_IPC_PORT and connects to a running
            // daemon, or embeds the engine in-process (ARCHITECTURE §1).
            onicode_ui::run_app(onicode_ui::UiConfig {
                data_dir: paths::data_dir(),
                ipc_port: onicode_cli::ipc_port_from_env(),
                update_feed: update_feed_from_env(),
                default_harness: onicode_ui::HarnessId::ClaudeCode,
                initial_url: open_url,
            });
            Ok(())
        }
    }
}
