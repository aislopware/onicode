//! `onicode`'s command line, for every binary that answers to it: the Onicode
//! app (`apps/onicode`), its engine helper (`onicode-engine`) and the zgx
//! interface (`apps/zgx`). One parser, one help text, one set of exit codes,
//! with no engine and no interface linked in.
//!
//! [`run`] parses this process's arguments and runs what needs neither
//! (`update`, `daemon`, `--noop-browser`, help and version). What's left is
//! the caller's:
//!   - no subcommand: open the window ([`Launch::Window`]);
//!   - a subcommand that runs the engine's code here ([`EngineCommand`]: `headless`, `engine`,
//!     `login`, `logout`, `status`, `sync`, `quiesce`, `mcp`). A binary that links the engine runs
//!     it itself ([`Engines::InProcess`], `onicode_app::run_engine_command`); an interface that
//!     doesn't hands the whole command line to the engine helper beside it ([`Engines::Helper`]:
//!     `Contents/MacOS/Onicode Engine` in a macOS bundle), which replaces this process and runs it
//!     there.

pub mod daemon;
mod env;
pub mod logging;
pub mod paths;
pub mod update_cli;

use clap::{CommandFactory as _, FromArgMatches as _, Parser, Subcommand};
pub use env::{
    DEFAULT_EDGE_URL, DEFAULT_IPC_PORT, DEFAULT_WORKOS_CLIENT_ID, edge_url_from_env, ipc_port_from,
    ipc_port_from_env, workos_client_id_from_env,
};
pub use logging::{RotatingLog, init_logging, long_running_setup, open_log_file};

#[derive(Parser)]
#[command(
    name = "onicode",
    version,
    about = "Multi-device controller for coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Open a Onicode conversation URL.
    #[arg(value_name = "URL")]
    open_url: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the engine without a UI (local-only unless a saved session enables sync).
    Headless,
    /// The desktop app's background engine (`onicode-engine` where it ships
    /// separately): started by the app, it keeps sessions running with the
    /// app closed.
    #[command(hide = true)]
    Engine,
    /// Sign in and enable sync on the next engine start.
    Login,
    /// Remove the saved session and return to local-only on the next start.
    Logout,
    /// Show workspace mode, optional auth, and engine status.
    Status,
    /// Live sync introspection from the running engine: per-room connection
    /// state, last pushed-frame/ack ages, rejoin/probe/resync counters.
    Sync,
    /// Cloud sleep handshake: ask the running engine to start no new work.
    /// Exits 0 when quiesced, 75 when busy (a turn in flight, a message
    /// waiting) — the edge then keeps the machine awake.
    #[command(hide = true)]
    Quiesce,
    #[cfg(target_os = "linux")]
    /// Trigger an Appshot in the running headed instance (desktop shortcut fallback).
    Appshot,
    /// Serve the Onicode MCP (Model Context Protocol) server on stdin/stdout,
    /// proxying to the running engine's IPC. Agents use it to create, read,
    /// and message chats. Logs go to stderr; stdout is the protocol.
    Mcp,
    /// Manage `onicode headless` as a background service (launchd / systemd --user).
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Check for a newer release and apply it (download → verify → swap →
    /// service restart). `--check` only reports (exits 1 when one is available).
    Update {
        #[arg(long)]
        check: bool,
    },
    /// Watches a new version while it proves itself after an update: started by
    /// the old version just before it hands over.
    #[command(name = "update-watchdog", hide = true)]
    UpdateWatchdog,
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Install, enable, and start the service (captures ONICODE_* env).
    Install,
    /// Stop and remove the service.
    Uninstall,
    /// Start the installed service.
    Start,
    /// Stop the service.
    Stop,
    /// Restart the service.
    Restart,
    /// Show the service manager's view of the daemon.
    Status,
}

/// A subcommand that runs the engine's own code in this process: what the
/// engine helper (or a binary linking the engine) does with it.
pub enum EngineCommand {
    Headless,
    Engine,
    Login,
    Logout,
    Status,
    Sync,
    Quiesce,
    Mcp,
}

/// Where this binary runs [`EngineCommand`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engines {
    /// Here: the binary links the engine (`onicode`, `onicode-engine`).
    InProcess,
    /// In the engine helper beside this binary, which replaces this process
    /// with the same arguments (an interface that doesn't link the engine).
    Helper,
}

/// What's left for the binary once [`run`] has parsed its arguments.
pub enum Launch {
    /// A command ran here; exit with its result.
    Done(anyhow::Result<()>),
    /// Run this in-process ([`Engines::InProcess`] only); logging is set up.
    Engine(EngineCommand),
    /// No subcommand: open the window, on a conversation URL if one was given.
    Window { open_url: Option<String> },
    /// Ask the running headed instance for an Appshot (the interface's own).
    #[cfg(target_os = "linux")]
    Appshot,
}

/// The parser, as `binary`'s `--version` names it: `version` is the build's
/// (the workspace's, or a beta interface's own, `0.2.136-beta.1`).
pub fn command(version: &'static str) -> clap::Command {
    Cli::command().version(version)
}

/// Parse this process's arguments and run what needs neither the engine nor
/// the interface. Help, `--version` and argument errors print and exit here,
/// as clap does.
pub fn run(version: &'static str, engines: Engines) -> Launch {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--noop-browser")) {
        return Launch::Done(Ok(()));
    }
    let cli = match Cli::from_arg_matches(&command(version).get_matches()) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    if engines == Engines::Helper && cli.command.as_ref().is_some_and(Command::runs_engine) {
        // The helper parses the same arguments and does all of the below
        // itself (logging, the command).
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(error) => return Launch::Done(Err(error.into())),
        };
        let error = onicode_launch::exec_helper(&exe, std::env::args_os().skip(1));
        return Launch::Done(Err(anyhow::anyhow!("couldn't start the engine: {error}")));
    }
    let long_running = matches!(&cli.command, None | Some(Command::Headless));
    let mode = if cli.command.is_some() {
        "headless"
    } else {
        "headed"
    };
    // `onicode mcp` owns stdout for the protocol: a single log line on it
    // would corrupt the JSON-RPC stream, so its diagnostics go to stderr.
    // The background engine logs on its own (`onicode_app::background`).
    if !matches!(&cli.command, Some(Command::Engine)) {
        init_logging(
            long_running,
            mode,
            matches!(&cli.command, Some(Command::Mcp)),
        );
    }
    if long_running {
        long_running_setup();
    }

    let engine = |command| match engines {
        Engines::InProcess => Launch::Engine(command),
        // (Handed to the helper above.)
        Engines::Helper => unreachable!("engine commands run in the helper"),
    };
    match cli.command {
        Some(Command::Headless) => engine(EngineCommand::Headless),
        Some(Command::Engine) => engine(EngineCommand::Engine),
        Some(Command::Login) => engine(EngineCommand::Login),
        Some(Command::Logout) => engine(EngineCommand::Logout),
        Some(Command::Status) => engine(EngineCommand::Status),
        Some(Command::Sync) => engine(EngineCommand::Sync),
        Some(Command::Quiesce) => engine(EngineCommand::Quiesce),
        Some(Command::Mcp) => engine(EngineCommand::Mcp),
        #[cfg(target_os = "linux")]
        Some(Command::Appshot) => Launch::Appshot,
        Some(Command::Update { check }) => Launch::Done(
            tokio::runtime::Runtime::new()
                .map_err(anyhow::Error::from)
                .and_then(|runtime| {
                    runtime.block_on(update_cli::update(&edge_url_from_env(), check))
                }),
        ),
        Some(Command::UpdateWatchdog) => Launch::Done(onicode_update::transaction::run_watchdog()),
        Some(Command::Daemon { command }) => Launch::Done(match command {
            DaemonCommand::Install => daemon::install(&paths::data_dir()),
            DaemonCommand::Uninstall => daemon::uninstall(),
            DaemonCommand::Start => daemon::start(),
            DaemonCommand::Stop => daemon::stop(),
            DaemonCommand::Restart => daemon::restart(),
            DaemonCommand::Status => daemon::status(),
        }),
        None => Launch::Window {
            open_url: cli.open_url,
        },
    }
}

impl Command {
    /// Whether it runs the engine's code ([`EngineCommand`]).
    fn runs_engine(&self) -> bool {
        match self {
            Self::Headless
            | Self::Engine
            | Self::Login
            | Self::Logout
            | Self::Status
            | Self::Sync
            | Self::Quiesce
            | Self::Mcp => true,
            #[cfg(target_os = "linux")]
            Self::Appshot => false,
            Self::Update { .. } | Self::UpdateWatchdog | Self::Daemon { .. } => false,
        }
    }
}

/// Every subcommand path of the command line (`[]`, `["daemon"]`,
/// `["daemon", "install"]`, …), hidden ones included: what the parity checks of
/// each binary that runs it walk (`apps/onicode/tests/cli.rs`,
/// `apps/zgx/tests/cli.rs`).
pub fn subcommand_paths(version: &'static str) -> Vec<Vec<String>> {
    fn walk(command: &clap::Command, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        out.push(path.clone());
        for sub in command.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            path.push(sub.get_name().to_owned());
            walk(sub, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    walk(&command(version), &mut Vec::new(), &mut out);
    out
}

/// `onicode <path…> --help` as a binary named `onicode` prints it: clap's own
/// answer to those arguments.
pub fn help_for(version: &'static str, path: &[String]) -> String {
    let argv = std::iter::once("onicode".to_owned())
        .chain(path.iter().cloned())
        .chain(std::iter::once("--help".to_owned()));
    match command(version).try_get_matches_from(argv) {
        Err(help) if help.kind() == clap::error::ErrorKind::DisplayHelp => {
            help.render().to_string()
        }
        other => panic!(
            "onicode {} --help didn't print help: {other:?}",
            path.join(" ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_well_formed() {
        command("0.0.0").debug_assert();
    }

    #[test]
    fn every_subcommand_is_reachable() {
        let paths = subcommand_paths("0.0.0");
        for expected in [
            &["headless"][..],
            &["engine"],
            &["login"],
            &["logout"],
            &["status"],
            &["sync"],
            &["quiesce"],
            &["mcp"],
            &["daemon", "install"],
            &["update"],
        ] {
            assert!(
                paths.iter().any(|path| path == expected),
                "{expected:?} missing from {paths:?}"
            );
        }
    }

    /// What runs where: the engine's commands go to the engine (in-process
    /// or the helper), the rest run here.
    #[test]
    fn engine_commands_are_the_ones_that_run_the_engine() {
        let parse = |args: &[&str]| {
            let argv = std::iter::once("onicode").chain(args.iter().copied());
            Cli::from_arg_matches(&command("0.0.0").try_get_matches_from(argv).unwrap())
                .unwrap()
                .command
        };
        for args in [
            &["headless"][..],
            &["engine"],
            &["login"],
            &["logout"],
            &["status"],
            &["sync"],
            &["quiesce"],
            &["mcp"],
        ] {
            assert!(parse(args).unwrap().runs_engine(), "{args:?}");
        }
        for args in [&["update", "--check"][..], &["daemon", "status"]] {
            assert!(!parse(args).unwrap().runs_engine(), "{args:?}");
        }
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn the_version_is_the_build_s() {
        let version = command("0.2.136-beta.1").render_version();
        assert_eq!(version.trim(), "onicode 0.2.136-beta.1");
    }
}
