//! `onicode-engine --data-dir PATH`: the machine's engine (engine.md, "Process"). The app starts
//! it detached from beside itself (`onicode-launch`) and reaches it on `PATH/engine.sock`; it runs
//! until a viewer stops it or it is signalled, and its agents stop with it.
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
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
