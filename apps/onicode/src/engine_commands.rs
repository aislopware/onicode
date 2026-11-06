//! The subcommands that run the engine's own code ([`onicode_cli::EngineCommand`]),
//! in a binary that links it: `onicode` itself, and `onicode-engine`, the helper
//! an interface without the engine hands them to. Logging is set up by
//! [`onicode_cli::run`] before these run.

use onicode_cli::EngineCommand;

use crate::{engine_config_from_env, ipc_cli};

pub fn run_engine_command(command: EngineCommand) -> anyhow::Result<()> {
    match command {
        EngineCommand::Headless => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async {
                let engine = onicode_engine::Engine::new(engine_config_from_env());
                engine.run().await
            })
        }
        EngineCommand::Status => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(ipc_cli::status_cli(&engine_config_from_env()))
        }
        EngineCommand::Sync => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(ipc_cli::sync_cli(config.ipc_port, &config.data_dir))
        }
        EngineCommand::Mcp => crate::run_mcp(),
        EngineCommand::Engine => crate::background::run(),
    }
}
