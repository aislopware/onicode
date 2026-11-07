//! How the app starts its engine (engine.md, "Process"): the `onicode-engine` executable beside
//! the app's own ("Onicode Engine" inside a macOS bundle), given the data dir, detached into a
//! session of its own so it outlives the window. The app spawns it itself, so macOS attributes
//! the engine's (and its agents') file access to the app: TCC stays the app's, and no Login
//! Items approval is needed. Its standard error goes to `{data_dir}/engine.log`.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// The engine executable's names beside the app's, best first.
pub const ENGINE_EXECUTABLES: &[&str] = &["Onicode Engine", "onicode-engine"];

/// The flag that names the engine's data dir.
pub const DATA_DIR_ARG: &str = "--data-dir";
/// Set on the engine's environment by the app that started it.
pub const MANAGED_ENV: &str = "ONICODE_MANAGED_ENGINE";
/// The app to open for a clicked banner: its bundle, or its executable outside one.
pub const APP_ENV: &str = "ONICODE_APP";
/// The engine's standard error, inside its data dir.
pub const LOG_FILE: &str = "engine.log";
/// A log past this size starts over at the next launch.
const LOG_LIMIT: u64 = 4 << 20;

/// The engine executable beside the app at `app_exe`, if there is one.
#[must_use]
pub fn engine_executable(app_exe: &Path) -> Option<PathBuf> {
    let dir = app_exe.parent().unwrap_or(Path::new("."));
    ENGINE_EXECUTABLES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// What a clicked banner opens: the app bundle (the executable's enclosing `.app`), or the
/// executable outside one.
#[must_use]
pub fn app_path(app_exe: &Path) -> PathBuf {
    app_exe
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .unwrap_or(app_exe)
        .to_path_buf()
}

/// Starts the engine at `engine` over `data_dir` for the app at `app_exe`, detached: a session
/// of its own, no inherited console or pipes, so the app exiting (or its terminal closing)
/// leaves it running. An engine already running there keeps its instance lock and the new one
/// exits at once, so a race of launches leaves one engine.
pub fn launch(data_dir: &Path, engine: &Path, app_exe: &Path) -> std::io::Result<Child> {
    let mut command = Command::new(engine);
    command
        .arg(DATA_DIR_ARG)
        .arg(data_dir)
        .env(MANAGED_ENV, "1")
        .env(APP_ENV, app_path(app_exe));
    spawn_detached(&mut command, log(data_dir).ok())
}

/// The engine's log, appended to; one past [`LOG_LIMIT`] starts over.
fn log(data_dir: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::create_dir_all(data_dir)?;
    let path = data_dir.join(LOG_FILE);
    let full = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_LIMIT);
    OpenOptions::new()
        .create(true)
        .append(!full)
        .write(true)
        .truncate(full)
        .mode(0o600)
        .open(path)
}

/// Spawns `command` so it survives its parent, its standard error into `log` when given.
pub fn spawn_detached(command: &mut Command, log: Option<File>) -> std::io::Result<Child> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.map_or_else(Stdio::null, Stdio::from));
    use std::os::unix::process::CommandExt as _;
    // A session of its own: no controlling terminal to hang up on it, and no process group the
    // app's teardown signals.
    // SAFETY: `setsid` is async-signal-safe and touches no memory of the parent.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn()
}

/// Reaps `child` whenever it exits, so an engine that stops while the app runs (or a launch that
/// lost the race for the lock) never lingers as a zombie.
pub fn reap(mut child: Child) {
    let _ = std::thread::Builder::new()
        .name("engine-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn the_engine_is_found_beside_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("onicode");
        std::fs::write(&app, "").unwrap();
        assert_eq!(engine_executable(&app), None);
        let engine = dir.path().join(ENGINE_EXECUTABLES[0]);
        std::fs::write(&engine, "").unwrap();
        assert_eq!(engine_executable(&app), Some(engine));
    }

    #[test]
    fn app_path_is_the_bundle_on_macos_layouts() {
        assert_eq!(
            app_path(Path::new(
                "/Applications/Onicode.app/Contents/MacOS/onicode"
            )),
            PathBuf::from("/Applications/Onicode.app")
        );
        assert_eq!(
            app_path(Path::new("/home/me/.onicode/app/current/onicode")),
            PathBuf::from("/home/me/.onicode/app/current/onicode")
        );
    }

    /// The engine gets the data dir and the app's mark, and writes its standard error to the log
    /// in the data dir.
    #[test]
    fn launch_hands_the_engine_its_data_dir_detached() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let engine = dir.path().join("onicode-engine");
        let out = dir.path().join("out");
        std::fs::write(
            &engine,
            format!(
                "#!/bin/sh\nprintf '%s %s %s' \"$1\" \"$2\" \"$ONICODE_MANAGED_ENGINE\" > '{}'\necho starting >&2\n",
                out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&engine, std::fs::Permissions::from_mode(0o700)).unwrap();
        let app = dir.path().join("onicode");
        let status = launch(&data, &engine, &app).unwrap().wait().unwrap();
        assert!(status.success());
        let written = std::fs::read_to_string(&out).unwrap();
        let fields: Vec<&str> = written.split(' ').collect();
        assert_eq!(fields.first(), Some(&DATA_DIR_ARG));
        assert_eq!(fields.get(1).copied(), data.to_str());
        assert_eq!(fields.get(2), Some(&"1"));
        let log = std::fs::read_to_string(data.join(LOG_FILE)).unwrap();
        assert_eq!(log, "starting\n");
    }
}
