//! The shell a new terminal runs and what it is told about its terminal.
//!
//! A login shell in the terminal's directory (`~` expanded, the home directory when it is gone)
//! with TerminalView's environment: `TERM=xterm-ghostty` with ghostty's terminfo entry, which the
//! host's libghostty implements, written where `TERMINFO` names; `xterm-256color` when the files
//! cannot be written. zsh, bash and fish also load Onicode's integration script, which emits
//! OSC 133 prompt marks and OSC 7 for the directory. The scripts are injected the way ghostty
//! does it and hand straight back to the user's own startup files:
//!
//! - zsh: `ZDOTDIR` names a bootstrap `.zshenv` that restores the user's `ZDOTDIR`.
//! - bash: `bash --posix` reads the file `ENV` names, which leaves posix mode and runs the login
//!   files. Apple's `/bin/bash` 3.2 ignores `ENV` in posix mode, so it runs without.
//! - fish: a vendor snippet in a directory put first on `XDG_DATA_DIRS`.
use std::io;
use std::path::{Path, PathBuf};

use crate::pty::Spec;

/// The terminfo name the shell runs as when the entry is installed.
pub const TERM: &str = "xterm-ghostty";
/// The terminfo name every system has.
pub const FALLBACK_TERM: &str = "xterm-256color";

/// ghostty's entry compiled by `tic -x` (source and license beside it in `assets/terminal`).
const TERMINFO: &[u8] = include_bytes!("../assets/terminal/xterm-ghostty");
const ZSH_ZSHENV: &[u8] = include_bytes!("../assets/terminal/zsh/.zshenv");
const ZSH_INTEGRATION: &[u8] = include_bytes!("../assets/terminal/zsh/onicode-integration.zsh");
const BASH_INTEGRATION: &[u8] = include_bytes!("../assets/terminal/bash/onicode.bash");
const FISH_INTEGRATION: &[u8] =
    include_bytes!("../assets/terminal/fish/vendor_conf.d/onicode.fish");

const BASH_SCRIPT: &str = "bash/onicode.bash";

/// Every installed file, relative to the install directory. ncurses looks an entry up under its
/// first letter (`x/`) on Linux and under its hex code (`78/`) on macOS.
const FILES: [(&str, &[u8]); 6] = [
    ("terminfo/x/xterm-ghostty", TERMINFO),
    ("terminfo/78/xterm-ghostty", TERMINFO),
    ("zsh/.zshenv", ZSH_ZSHENV),
    ("zsh/onicode-integration.zsh", ZSH_INTEGRATION),
    (BASH_SCRIPT, BASH_INTEGRATION),
    ("fish/vendor_conf.d/onicode.fish", FISH_INTEGRATION),
];

/// Where the scripts find what Onicode replaced, so they can put it back.
const ZSH_ZDOTDIR: &str = "ONICODE_ZSH_ZDOTDIR";
const BASH_INJECT: &str = "ONICODE_BASH_INJECT";
const BASH_USER_ENV: &str = "ONICODE_BASH_USER_ENV";
const BASH_LOGIN: &str = "ONICODE_BASH_LOGIN";
const FISH_XDG_DIR: &str = "ONICODE_SHELL_INTEGRATION_XDG_DIR";
/// fish's own default when `XDG_DATA_DIRS` is unset.
const XDG_DATA_DIRS_DEFAULT: &str = "/usr/local/share:/usr/share";

/// The login shell for a terminal in `cwd`, with `search_path` as its `PATH`, and the terminfo
/// name it runs as.
#[must_use]
pub fn spec(cwd: &str, search_path: String) -> (Spec, &'static str) {
    let dir = cache_dir().filter(|dir| install(dir).is_ok());
    build(cwd, search_path, dir.as_deref())
}

/// [`spec`] with the files at `dir`, or without them.
fn build(cwd: &str, search_path: String, dir: Option<&Path>) -> (Spec, &'static str) {
    let workdir = working_dir(cwd);
    let (program, args) = default_shell(&search_path);
    let mut spec = Spec {
        program,
        args,
        env: vec![
            ("COLORTERM".into(), "truecolor".into()),
            ("COLORFGBG".into(), "15;0".into()),
            ("TERM_PROGRAM".into(), "Onicode".into()),
            ("PATH".into(), search_path),
            ("PWD".into(), workdir.to_string_lossy().into_owned()),
        ],
        cwd: workdir,
        unset: Vec::new(),
    };
    let term = configure(&mut spec, dir, &|key| std::env::var(key).ok());
    (spec, term)
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `cwd` with `~` expanded, or the home directory when it is not a directory.
#[must_use]
pub fn working_dir(cwd: &str) -> PathBuf {
    let path = match cwd.strip_prefix('~') {
        Some(rest) => home().map_or_else(|| cwd.into(), |h| h.join(rest.trim_start_matches('/'))),
        None => PathBuf::from(cwd),
    };
    if path.is_dir() {
        path
    } else {
        home().unwrap_or(path)
    }
}

/// A per-user cache directory for the terminfo entry and the scripts.
fn cache_dir() -> Option<PathBuf> {
    let home = home()?;
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Caches")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".cache"))
    };
    Some(base.join("onicode/terminal"))
}

/// Writes every file that is missing or differs. Each goes through a temporary file and a rename,
/// so a shell starting alongside never reads half of one.
fn install(dir: &Path) -> io::Result<()> {
    for (name, bytes) in FILES {
        let path = dir.join(name);
        if std::fs::read(&path).is_ok_and(|have| have == bytes) {
            continue;
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("file without a directory"))?;
        std::fs::create_dir_all(parent)?;
        let staged = parent.join(format!(
            ".{}.{}",
            std::process::id(),
            name.replace('/', "_")
        ));
        std::fs::write(&staged, bytes)?;
        // Readable by others, so `sudo -u` keeps a working TERMINFO.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o644))?;
        }
        std::fs::rename(&staged, &path)?;
    }
    Ok(())
}

/// Sets the terminal variables and the shell's integration. `dir` is where [`install`] wrote the
/// files, or `None` to run without them; `current` reads the engine's environment. Returns the
/// `TERM` set.
fn configure(
    spec: &mut Spec,
    dir: Option<&Path>,
    current: &dyn Fn(&str) -> Option<String>,
) -> &'static str {
    let name = Path::new(&spec.program)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_owned();
    let mut set = |key: &str, value: String| spec.env.push((key.to_owned(), value));
    let Some(dir) = dir else {
        set("TERM", FALLBACK_TERM.to_owned());
        spec.unset.push("TERMINFO".to_owned());
        return FALLBACK_TERM;
    };
    let path = |sub: &str| dir.join(sub).to_string_lossy().into_owned();
    set("TERM", TERM.to_owned());
    set("TERMINFO", path("terminfo"));
    match name.as_str() {
        "zsh" => {
            match current("ZDOTDIR") {
                Some(user) => set(ZSH_ZDOTDIR, user),
                None => spec.unset.push(ZSH_ZDOTDIR.to_owned()),
            }
            set("ZDOTDIR", path("zsh"));
        }
        "bash" if !ignores_env_in_posix(&spec.program) => {
            // Login is the script's to do: a login bash would read ~/.profile, not ENV.
            spec.args = vec!["--posix".to_owned()];
            match current("ENV") {
                Some(user) => set(BASH_USER_ENV, user),
                None => spec.unset.push(BASH_USER_ENV.to_owned()),
            }
            set("ENV", path(BASH_SCRIPT));
            set(BASH_INJECT, "1".to_owned());
            set(BASH_LOGIN, "1".to_owned());
        }
        "fish" => {
            let rest = current("XDG_DATA_DIRS")
                .filter(|dirs| !dirs.is_empty())
                .unwrap_or_else(|| XDG_DATA_DIRS_DEFAULT.to_owned());
            let dirs = format!("{}:{rest}", dir.to_string_lossy());
            set("XDG_DATA_DIRS", dirs);
            set(FISH_XDG_DIR, dir.to_string_lossy().into_owned());
        }
        _ => {}
    }
    TERM
}

/// Apple's bash 3.2 reads no `ENV` in posix mode.
fn ignores_env_in_posix(shell: &str) -> bool {
    cfg!(target_os = "macos") && shell == "/bin/bash"
}

/// The user's shell and the arguments that make it a login shell.
#[cfg(not(windows))]
#[must_use]
pub fn default_shell(_search_path: &str) -> (String, Vec<String>) {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".into()
            } else {
                "/bin/bash".into()
            }
        });
    let stem = Path::new(&shell)
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let args = match stem {
        "zsh" | "bash" | "sh" | "fish" => vec!["-l".to_owned()],
        _ => Vec::new(),
    };
    (shell, args)
}

/// The user's shell and the arguments that make it a login shell.
#[cfg(windows)]
#[must_use]
pub fn default_shell(search_path: &str) -> (String, Vec<String>) {
    windows_shell(
        std::env::var("SHELL").ok().as_deref(),
        std::env::var("COMSPEC").ok().as_deref(),
        |program| resolve_windows_shell(program, search_path),
    )
}

#[cfg(windows)]
fn windows_shell(
    preferred: Option<&str>,
    comspec: Option<&str>,
    resolve: impl Fn(&str) -> Option<String>,
) -> (String, Vec<String>) {
    // Honor SHELL only if it resolves to a native executable. Git/MSYS can
    // otherwise supply a Unix path such as /bin/bash that Windows cannot run.
    let shell = preferred
        .and_then(&resolve)
        .or_else(|| resolve("pwsh.exe"))
        .or_else(|| resolve("powershell.exe"))
        .or_else(|| comspec.and_then(&resolve))
        .or_else(|| resolve("cmd.exe"))
        .unwrap_or_else(|| "cmd.exe".into());
    let stem = Path::new(&shell)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let args = match stem.as_str() {
        "pwsh" | "powershell" => vec!["-NoLogo".into()],
        "bash" | "sh" | "zsh" | "fish" => vec!["-l".into()],
        _ => Vec::new(),
    };
    (shell, args)
}

#[cfg(windows)]
fn resolve_windows_shell(program: &str, search_path: &str) -> Option<String> {
    let program = program.trim().trim_matches('"');
    if program.is_empty() {
        return None;
    }
    let mut executable = PathBuf::from(program);
    if executable.extension().is_none() {
        executable.set_extension("exe");
    }
    if !executable.extension()?.eq_ignore_ascii_case("exe") {
        return None;
    }
    let mut candidates = Vec::new();
    if executable.is_absolute() {
        candidates.push(executable);
    } else if executable.components().count() == 1 {
        candidates.extend(std::env::split_paths(search_path).map(|dir| dir.join(&executable)));
        // GUI launches can inherit a PATH that lacks PowerShell's install dir.
        match program.to_ascii_lowercase().as_str() {
            "pwsh" | "pwsh.exe" => {
                if let Some(root) = std::env::var_os("ProgramFiles") {
                    candidates.push(PathBuf::from(root).join("PowerShell/7/pwsh.exe"));
                }
            }
            "powershell" | "powershell.exe" | "cmd" | "cmd.exe" => {
                if let Some(root) = std::env::var_os("SystemRoot") {
                    let system = PathBuf::from(root).join("System32");
                    candidates.push(if program.to_ascii_lowercase().starts_with("powershell") {
                        system.join("WindowsPowerShell/v1.0/powershell.exe")
                    } else {
                        system.join("cmd.exe")
                    });
                }
            }
            _ => {}
        }
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .map(|path| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(spec: &'a Spec, key: &str) -> Option<&'a str> {
        spec.env
            .iter()
            .rev()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn shell(program: &str) -> Spec {
        Spec {
            program: program.to_owned(),
            args: vec!["-l".to_owned()],
            ..Spec::default()
        }
    }

    #[test]
    fn install_writes_both_terminfo_paths_and_repairs_edits() {
        let dir = std::env::temp_dir().join(format!("onicode-term-install-{}", std::process::id()));
        install(&dir).unwrap();
        for name in ["terminfo/x/xterm-ghostty", "terminfo/78/xterm-ghostty"] {
            assert_eq!(std::fs::read(dir.join(name)).unwrap(), TERMINFO);
        }
        let script = dir.join(BASH_SCRIPT);
        std::fs::write(&script, "edited").unwrap();
        install(&dir).unwrap();
        assert_eq!(std::fs::read(&script).unwrap(), BASH_INTEGRATION);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_the_files_the_shell_runs_as_xterm_256color() {
        let mut spec = shell("/bin/zsh");
        assert_eq!(configure(&mut spec, None, &|_| None), FALLBACK_TERM);
        assert_eq!(env(&spec, "TERM"), Some(FALLBACK_TERM));
        assert_eq!(env(&spec, "ZDOTDIR"), None);
        assert!(spec.unset.contains(&"TERMINFO".to_owned()));
        assert_eq!(spec.args, ["-l"]);
    }

    #[test]
    fn zsh_keeps_the_users_zdotdir() {
        let mut spec = shell("/bin/zsh");
        let user = |key: &str| (key == "ZDOTDIR").then(|| "/u".to_owned());
        assert_eq!(configure(&mut spec, Some(Path::new("/d")), &user), TERM);
        assert_eq!(env(&spec, "ZDOTDIR"), Some("/d/zsh"));
        assert_eq!(env(&spec, ZSH_ZDOTDIR), Some("/u"));
        assert_eq!(env(&spec, "TERMINFO"), Some("/d/terminfo"));
        assert_eq!(spec.args, ["-l"]);
        let mut spec = shell("/bin/zsh");
        configure(&mut spec, Some(Path::new("/d")), &|_| None);
        assert!(spec.unset.contains(&ZSH_ZDOTDIR.to_owned()));
    }

    #[test]
    fn bash_runs_posix_with_env_and_keeps_the_users_env() {
        let mut spec = shell("/usr/local/bin/bash");
        let user = |key: &str| (key == "ENV").then(|| "/u/env".to_owned());
        configure(&mut spec, Some(Path::new("/d")), &user);
        assert_eq!(spec.args, ["--posix"]);
        assert_eq!(env(&spec, "ENV"), Some("/d/bash/onicode.bash"));
        assert_eq!(env(&spec, BASH_USER_ENV), Some("/u/env"));
        assert_eq!(env(&spec, BASH_INJECT), Some("1"));
        assert_eq!(env(&spec, BASH_LOGIN), Some("1"));
    }

    #[test]
    fn fish_finds_its_snippet_on_xdg_data_dirs() {
        let mut spec = shell("fish");
        let user = |key: &str| (key == "XDG_DATA_DIRS").then(|| "/a:/b".to_owned());
        configure(&mut spec, Some(Path::new("/d")), &user);
        assert_eq!(env(&spec, "XDG_DATA_DIRS"), Some("/d:/a:/b"));
        assert_eq!(env(&spec, FISH_XDG_DIR), Some("/d"));
        let mut spec = shell("fish");
        configure(&mut spec, Some(Path::new("/d")), &|_| None);
        let want = format!("/d:{XDG_DATA_DIRS_DEFAULT}");
        assert_eq!(env(&spec, "XDG_DATA_DIRS"), Some(want.as_str()));
    }

    #[test]
    fn the_spec_carries_terminal_views_environment() {
        let dir = std::env::temp_dir();
        let (spec, term) = build(&dir.to_string_lossy(), "/usr/bin:/bin".to_owned(), None);
        assert_eq!(term, FALLBACK_TERM);
        assert_eq!(spec.cwd, dir);
        assert_eq!(env(&spec, "PATH"), Some("/usr/bin:/bin"));
        assert_eq!(env(&spec, "TERM_PROGRAM"), Some("Onicode"));
        assert_eq!(env(&spec, "COLORTERM"), Some("truecolor"));
        assert_eq!(working_dir("/no/such/dir"), home().unwrap());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    fn select(preferred: Option<&str>, available: &[&str]) -> (String, Vec<String>) {
        windows_shell(preferred, Some("custom-cmd.exe"), |program| {
            available.contains(&program).then(|| program.to_owned())
        })
    }

    #[test]
    fn windows_prefers_powershell_7_then_windows_powershell_then_cmd() {
        assert_eq!(
            select(None, &["pwsh.exe", "powershell.exe", "custom-cmd.exe"]),
            ("pwsh.exe".into(), vec!["-NoLogo".into()])
        );
        assert_eq!(
            select(None, &["powershell.exe", "custom-cmd.exe"]),
            ("powershell.exe".into(), vec!["-NoLogo".into()])
        );
        assert_eq!(
            select(None, &["custom-cmd.exe"]),
            ("custom-cmd.exe".into(), vec![])
        );
        assert_eq!(select(None, &["cmd.exe"]), ("cmd.exe".into(), vec![]));
        assert_eq!(select(None, &[]), ("cmd.exe".into(), vec![]));
    }

    #[test]
    fn windows_honors_a_resolvable_user_shell() {
        assert_eq!(
            select(Some("bash.exe"), &["bash.exe", "pwsh.exe"]),
            ("bash.exe".into(), vec!["-l".into()])
        );
        assert_eq!(
            select(Some("cmd.exe"), &["cmd.exe", "pwsh.exe"]),
            ("cmd.exe".into(), vec![])
        );
    }

    #[test]
    fn windows_ignores_missing_or_unix_shells() {
        for preferred in [
            None,
            Some(""),
            Some(" "),
            Some("/bin/bash"),
            Some("missing.exe"),
        ] {
            assert_eq!(
                select(preferred, &["pwsh.exe"]),
                ("pwsh.exe".into(), vec!["-NoLogo".into()])
            );
        }
        assert!(resolve_windows_shell("/bin/bash", "").is_none());
        assert!(resolve_windows_shell("missing.exe", "").is_none());
        assert!(resolve_windows_shell("script.cmd", "").is_none());
        assert!(resolve_windows_shell("", "").is_none());
    }

    #[test]
    fn windows_uses_terminal_path_and_preserves_candidate_order() {
        use std::fs;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "onicode-shell-path-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let system = root.join("system");
        let scoop = root.join("scoop/shims");
        let later = root.join("later");
        for dir in [&system, &scoop, &later] {
            fs::create_dir_all(dir).unwrap();
        }
        fs::write(system.join("powershell.exe"), []).unwrap();
        fs::write(scoop.join("pwsh.exe"), []).unwrap();
        fs::write(later.join("pwsh.exe"), []).unwrap();
        let search_path = std::env::join_paths([&system, &scoop, &later]).unwrap();
        let search_path = search_path.to_str().unwrap();

        // PowerShell 7 in the terminal-only Scoop path wins over both Windows
        // PowerShell earlier in PATH and another pwsh later in PATH.
        let selected = windows_shell(None, None, |program| {
            resolve_windows_shell(program, search_path)
        });
        let resolved = resolve_windows_shell("pwsh.exe", search_path);
        fs::remove_dir_all(&root).unwrap();
        let expected = scoop.join("pwsh.exe").to_string_lossy().into_owned();
        assert_eq!(resolved, Some(expected.clone()));
        assert_eq!(selected, (expected, vec!["-NoLogo".into()]));
    }

    #[test]
    fn windows_default_shell_runs_a_command() {
        use std::os::windows::process::CommandExt;
        let (shell, args) =
            windows_shell(None, std::env::var("COMSPEC").ok().as_deref(), |program| {
                resolve_windows_shell(program, &std::env::var("PATH").unwrap_or_default())
            });
        let powershell = Path::new(&shell)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_ascii_lowercase();
        let command_args = if matches!(powershell.as_str(), "pwsh" | "powershell") {
            vec!["-NoProfile", "-Command", "Write-Output onicode-terminal-ok"]
        } else {
            vec!["/D", "/C", "echo onicode-terminal-ok"]
        };
        let output = std::process::Command::new(shell)
            .args(args)
            .args(command_args)
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .expect("the selected Windows shell must start");
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("onicode-terminal-ok"));
    }
}
