//! `onicode-engine --data-dir PATH`: the machine's engine (engine.md, "Process"). The app starts
//! it detached from beside itself (`onicode-launch`) and reaches it on `PATH/engine.sock`; it runs
//! until a viewer stops it or it is signalled, and its agents stop with it.
//!
//! The agents' bridges are this executable's too, since a turn names its own executable to its
//! agent: `visual-mcp` serves the session visual tools over standard input and output, and
//! `visual ...` and `app ...` are their command-line clients.
use std::path::PathBuf;
use std::process::ExitCode;

/// A bridge client's answer, printed as JSON; a refusal fails the process.
fn answer(reply: Result<serde_json::Value, String>) -> ExitCode {
    match reply {
        Ok(value) => {
            println!("{value}");
            if value.get("ok") == Some(&serde_json::Value::Bool(false)) {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            println!("{}", serde_json::json!({"ok": false, "error": error}));
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let words: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    let rest = words.get(1..).unwrap_or_default();
    match words.first().map(String::as_str) {
        Some("visual-mcp") => {
            return match onicode_core::visual_bridge::mcp_stdio() {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error}");
                    ExitCode::FAILURE
                }
            };
        }
        Some("visual") => return answer(onicode_core::visual_bridge::client(rest)),
        Some("app") => return answer(onicode_core::app_access::bridge::client(rest)),
        _ => {}
    }
    let mut args = std::env::args_os().skip(1);
    let mut data_dir: Option<PathBuf> = std::env::var_os("ONICODE_DATA_DIR").map(PathBuf::from);
    while let Some(arg) = args.next() {
        if arg == onicode_launch::DATA_DIR_ARG {
            data_dir = args.next().map(PathBuf::from);
        } else if arg == "--version" {
            println!("onicode-engine {}", onicode_engine::VERSION);
            return ExitCode::SUCCESS;
        } else {
            eprintln!("Unknown option: {}", arg.to_string_lossy());
            return ExitCode::from(2);
        }
    }
    let Some(data_dir) = data_dir else {
        eprintln!("onicode-engine {} PATH", onicode_launch::DATA_DIR_ARG);
        return ExitCode::from(2);
    };
    match onicode_engine::host::run(&data_dir) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
