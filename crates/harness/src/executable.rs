//! Cross-platform executable discovery shared by native and ACP harnesses.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub(crate) fn home_dir() -> Option<PathBuf> {
    home_dir_with(&|key| std::env::var_os(key))
}

/// The user's home, or the current directory when HOME is unset.
pub(crate) fn home_or_current_dir() -> PathBuf {
    home_or_current_dir_with(&|key| std::env::var_os(key), &std::env::current_dir)
}

fn home_or_current_dir_with(
    env: &impl Fn(&str) -> Option<OsString>,
    current_dir: &impl Fn() -> std::io::Result<PathBuf>,
) -> PathBuf {
    home_dir_with(env)
        .or_else(|| current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Accept an executable override. The path must exist, so `installed()` and
/// the launch path agree on what an override means.
pub(crate) fn validate_native_override(path: &Path) -> Result<PathBuf, crate::HarnessError> {
    if !path.is_file() {
        return Err(crate::HarnessError::NotInstalled(format!(
            "{} does not exist",
            path.display()
        )));
    }
    Ok(path.to_path_buf())
}

pub(crate) fn find_on_paths(exe: &str, extra: Vec<PathBuf>) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    find_on_paths_matching_with(
        exe,
        extra,
        &|key| std::env::var_os(key),
        crate::shell_env::user_path(),
        |path| {
            if runnable(path) {
                candidates.push(path.to_path_buf());
            }
            false
        },
    );
    newest_candidate(candidates)
}

pub(crate) fn binary_hint(path: &Path) -> String {
    format!(
        "{} (version {})",
        path.display(),
        binary_version(path)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into())
    )
}

/// What a failed agent launch reports. A working directory that is gone
/// fails the spawn with `NotFound` on Unix too, which must not read as a
/// missing binary; `binary` names the binary when it really is missing.
/// Everything else stays `HarnessError::Io`, kind intact (the engine
/// retries the transient kinds).
pub(crate) fn spawn_error(
    error: std::io::Error,
    cwd: Option<&Path>,
    binary: impl FnOnce() -> String,
) -> crate::HarnessError {
    if let Some(cwd) = cwd
        && matches!(cwd.try_exists(), Ok(false))
    {
        return crate::HarnessError::Io(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            format!("working directory {} does not exist", cwd.display()),
        ));
    }
    if error.kind() == std::io::ErrorKind::NotFound {
        crate::HarnessError::NotInstalled(binary())
    } else {
        crate::HarnessError::Io(error)
    }
}

fn runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

fn newest_candidate(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    let mut seen = std::collections::HashSet::new();
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|p| seen.insert(p.canonicalize().unwrap_or_else(|_| p.clone())))
        .collect();
    let mut best = candidates.first()?.clone();
    if candidates.len() > 1 {
        let mut version = binary_version(&best);
        for path in candidates.iter().skip(1) {
            let next = binary_version(path);
            if next.as_ref().is_some_and(|next| {
                version
                    .as_ref()
                    .is_none_or(|current| next.cmp_precedence(current).is_gt())
            }) {
                best = path.clone();
                version = next;
            }
        }
    }
    Some(best)
}

type VersionKey = (PathBuf, Option<std::time::SystemTime>, u64);

struct VersionEntry {
    version: Option<semver::Version>,
    aliases: std::collections::HashSet<String>,
    /// Set for anything but a version: a probe that could not finish
    /// (timeout, transient spawn failure) is retried soon, a definite
    /// failure a little later; only an answer is kept for the binary's life.
    expires: Option<std::time::Instant>,
}

#[derive(Default)]
struct VersionCache {
    entries: std::collections::HashMap<VersionKey, VersionEntry>,
    /// One lock per executable being probed, so concurrent callers for the
    /// same binary share one `--version` while other binaries probe freely.
    probing: std::collections::HashMap<VersionKey, std::sync::Arc<std::sync::Mutex<()>>>,
    /// Per name, how often [`invalidate_versions`] cleared it: a probe whose
    /// names were cleared while it ran may have run the replaced wrapper, so
    /// its answer is not cached.
    invalidated: std::collections::HashMap<String, u64>,
}

impl VersionCache {
    /// A stamp over `names` that changes whenever any of them is invalidated.
    fn invalidation_stamp<'a>(&self, names: impl IntoIterator<Item = &'a String>) -> u64 {
        names
            .into_iter()
            .map(|name| self.invalidated.get(name).copied().unwrap_or(0))
            .sum()
    }
}

static VERSION_CACHE: std::sync::OnceLock<std::sync::Mutex<VersionCache>> =
    std::sync::OnceLock::new();

/// A freshly installed or updated CLI's first exec waits on the OS malware
/// scan.
const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long an inconclusive probe is trusted before the binary is asked again.
const INCONCLUSIVE_VERSION_TTL: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a definite failure (non-zero exit, no version in the output) is
/// trusted: a busy machine or a half-finished update heals.
const VERSION_FAILURE_TTL: std::time::Duration = std::time::Duration::from_secs(60);

enum Probe {
    /// The binary answered: a version, or a definite failure (non-zero exit,
    /// unparsable output). Cached for the executable's identity.
    Answered(Option<semver::Version>),
    /// The probe could not finish; cached only briefly.
    Inconclusive,
}

fn version_cache() -> std::sync::MutexGuard<'static, VersionCache> {
    VERSION_CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// An explicit install may replace wrappers without changing their metadata.
/// Clear all candidates, including canonical paths behind vendor symlinks.
pub(crate) fn invalidate_versions(names: &[&str]) {
    if VERSION_CACHE.get().is_some() {
        let mut cache = version_cache();
        for name in names {
            *cache.invalidated.entry((*name).to_owned()).or_default() += 1;
        }
        cache
            .entries
            .retain(|_, entry| !names.iter().any(|name| entry.aliases.contains(*name)));
    }
}

/// Probe once per executable identity. A version is cached until the file
/// changes; a failure is retried after [`VERSION_FAILURE_TTL`], and a probe
/// that timed out after [`INCONCLUSIVE_VERSION_TTL`], so one cold first exec
/// cannot pin an older install for the life of the process.
pub fn binary_version(path: &Path) -> Option<semver::Version> {
    binary_version_with(path, VERSION_PROBE_TIMEOUT)
}

fn binary_version_with(path: &Path, timeout: std::time::Duration) -> Option<semver::Version> {
    let canonical = path.canonicalize().ok()?;
    let metadata = canonical.metadata().ok()?;
    let key = (canonical, metadata.modified().ok(), metadata.len());
    let alias = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let cached = |cache: &mut VersionCache| {
        let entry = cache.entries.get_mut(&key)?;
        if entry
            .expires
            .is_some_and(|at| std::time::Instant::now() >= at)
        {
            return None;
        }
        entry.aliases.insert(alias.clone());
        Some(entry.version.clone())
    };
    let slot = {
        let mut cache = version_cache();
        if let Some(version) = cached(&mut cache) {
            return version;
        }
        cache.probing.entry(key.clone()).or_default().clone()
    };
    // The global lock is released while probing: a slow binary must not
    // stall every other harness's resolution behind it.
    let _probing = slot.lock().unwrap_or_else(|e| e.into_inner());
    // The names this binary is known by: what an invalidation would match.
    let (names, stamp) = {
        let mut cache = version_cache();
        if let Some(version) = cached(&mut cache) {
            return version;
        }
        let mut names: Vec<String> = vec![alias.clone()];
        if let Some(entry) = cache.entries.get(&key) {
            names.extend(entry.aliases.iter().cloned());
        }
        let stamp = cache.invalidation_stamp(&names);
        (names, stamp)
    };
    let (version, expires) = match probe_version(path, timeout) {
        Probe::Answered(Some(version)) => (Some(version), None),
        Probe::Answered(None) => (None, Some(std::time::Instant::now() + VERSION_FAILURE_TTL)),
        Probe::Inconclusive => (
            None,
            Some(std::time::Instant::now() + INCONCLUSIVE_VERSION_TTL),
        ),
    };
    let mut cache = version_cache();
    if cache.invalidation_stamp(&names) != stamp {
        // An install replaced wrappers while this probe ran: its answer may
        // be the old one's, so the next caller asks again.
        if cache
            .probing
            .get(&key)
            .is_some_and(|current| std::sync::Arc::ptr_eq(current, &slot))
        {
            cache.probing.remove(&key);
        }
        return version;
    }
    let mut aliases: std::collections::HashSet<String> = [alias].into_iter().collect();
    // An inconclusive retry keeps the aliases earlier callers registered.
    if let Some(previous) = cache.entries.get(&key) {
        aliases.extend(previous.aliases.iter().cloned());
    }
    cache.entries.retain(|(p, _, _), _| p != &key.0);
    cache.entries.insert(
        key.clone(),
        VersionEntry {
            version: version.clone(),
            aliases,
            expires,
        },
    );
    if cache
        .probing
        .get(&key)
        .is_some_and(|current| std::sync::Arc::ptr_eq(current, &slot))
    {
        cache.probing.remove(&key);
    }
    version
}

fn probe_version(path: &Path, timeout: std::time::Duration) -> Probe {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let Ok(mut child) = command.spawn() else {
        // ETXTBSY while an updater is still writing the file, EAGAIN, …
        return Probe::Inconclusive;
    };
    fn reader(pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.take(65536).read_to_end(&mut bytes);
            bytes
        })
    }
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Probe::Inconclusive;
    };
    let readers = [reader(stdout), reader(stderr)];
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // SAFETY: kill(2) targets the probe's own private process group.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let Some(status) = status else {
        return Probe::Inconclusive;
    };
    if !status.success() {
        return Probe::Answered(None);
    }
    let mut bytes = Vec::new();
    for reader in readers {
        while !reader.is_finished() && start.elapsed() < timeout {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if !reader.is_finished() {
            return Probe::Inconclusive;
        }
        bytes.extend(reader.join().unwrap_or_default());
        bytes.push(b' ');
    }
    Probe::Answered(parse_version(&bytes))
}

pub(crate) fn parse_version(bytes: &[u8]) -> Option<semver::Version> {
    String::from_utf8_lossy(bytes)
        .split_whitespace()
        .find_map(|word| {
            semver::Version::parse(
                word.trim_matches(|c: char| matches!(c, '(' | ')' | ','))
                    .trim_start_matches('v'),
            )
            .ok()
        })
}

/// The user's home as the agent CLIs see it.
fn home_dir_with(env: &impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    env_path(env, "HOME")
}

fn node_version_manager_bins_with(env: &impl Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let home = home_dir_with(env);
    let mut dirs = Vec::new();

    let mut fnm_roots: Vec<PathBuf> = env("FNM_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(home) = &home {
        fnm_roots.push(home.join(".local").join("share").join("fnm"));
        fnm_roots.push(home.join("Library").join("Application Support").join("fnm"));
        fnm_roots.push(home.join(".fnm"));
    }
    for root in fnm_roots {
        dirs.push(root.join("aliases").join("default").join("bin"));
    }

    if let Some(home) = &home {
        dirs.push(home.join(".volta").join("bin"));
        dirs.push(home.join(".bun").join("bin"));
        dirs.push(home.join("Library").join("pnpm"));
        dirs.push(home.join(".local").join("share").join("pnpm"));

        let nvm = home.join(".nvm").join("versions").join("node");
        if let Ok(entries) = std::fs::read_dir(&nvm) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path().join("bin"))
                .collect();
            versions.sort();
            versions.reverse();
            dirs.append(&mut versions);
        }
    }
    dirs
}

fn env_path(env: &impl Fn(&str) -> Option<OsString>, key: &str) -> Option<PathBuf> {
    env(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Directories of a bare system PATH: launchd's for a Dock/Finder launch,
/// systemd's default for a service, and macOS's path_helper additions.
const SYSTEM_PATH_DIRS: &[&str] = &[
    "/usr/local/sbin",
    "/usr/local/bin",
    "/usr/sbin",
    "/usr/bin",
    "/sbin",
    "/bin",
    "/snap/bin",
    "/usr/games",
    "/usr/local/games",
    "/System/Cryptexes/App/usr/bin",
    "/Library/Apple/usr/bin",
];

/// Whether a Unix process PATH is only the system's: what a GUI launch or a
/// service without its own PATH inherits. Only then does the login shell's
/// PATH (the user's Homebrew, nvm, …) go first; a PATH the user or a unit
/// set up — a venv, nix, direnv, nvm in the launching terminal — keeps
/// precedence, for the child's PATH and for resolving the agent binary alike.
pub(crate) fn is_bare_system_path(path: Option<&OsStr>) -> bool {
    path.is_none_or(|path| {
        std::env::split_paths(path).all(|dir| {
            dir.as_os_str().is_empty()
                || SYSTEM_PATH_DIRS
                    .iter()
                    .any(|system| dir == Path::new(system))
                || dir.starts_with("/var/run/com.apple.security.cryptexd")
        })
    })
}

pub(crate) fn find_on_paths_with(
    exe: &str,
    extra: Vec<PathBuf>,
    env: &impl Fn(&str) -> Option<OsString>,
    login_shell_path: Option<OsString>,
) -> Option<PathBuf> {
    find_on_paths_matching_with(exe, extra, env, login_shell_path, |_| true)
}

pub(crate) fn find_on_paths_matching_with(
    exe: &str,
    extra: Vec<PathBuf>,
    env: &impl Fn(&str) -> Option<OsString>,
    login_shell_path: Option<OsString>,
    mut predicate: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let from_path = |path: OsString| {
        std::env::split_paths(&path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join(exe))
            .collect::<Vec<_>>()
    };
    let own_path = env("PATH");
    let login_first = is_bare_system_path(own_path.as_deref());
    let own = own_path.map(from_path).unwrap_or_default();
    let shell = login_shell_path.map(from_path).unwrap_or_default();
    // The same precedence as the child's PATH (see `child_path_entries`).
    let mut candidates = if login_first {
        [shell, own].concat()
    } else {
        [own, shell].concat()
    };
    candidates.extend(extra);
    candidates.extend(
        node_version_manager_bins_with(env)
            .into_iter()
            .map(|dir| dir.join(exe)),
    );
    candidates
        .into_iter()
        .find(|path| path.is_file() && predicate(path))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Resolution walks the PATHs in the child PATH's order: the login shell
    /// first only behind a bare system PATH.
    #[test]
    fn resolution_prefers_a_deliberate_process_path_like_the_child_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let (venv, brew) = (dir.path().join("venv"), dir.path().join("brew"));
        for bin in [&venv, &brew] {
            std::fs::create_dir_all(bin).unwrap();
            let exe = bin.join("agent");
            std::fs::write(&exe, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let shell = Some(brew.clone().into_os_string());
        let process = std::env::join_paths([venv.as_path(), Path::new("/usr/bin")]).unwrap();
        let lookup = env(&[("PATH", process)]);
        assert_eq!(
            find_on_paths_with("agent", Vec::new(), &lookup, shell.clone()),
            Some(venv.join("agent"))
        );
        let bare = env(&[("PATH", OsString::from("/usr/bin:/bin:/usr/sbin:/sbin"))]);
        assert_eq!(
            find_on_paths_with("agent", Vec::new(), &bare, shell),
            Some(brew.join("agent"))
        );
        assert!(is_bare_system_path(None));
        assert!(!is_bare_system_path(Some(OsStr::new(
            "/nix/store/x/bin:/usr/bin"
        ))));
    }

    #[test]
    fn a_missing_working_directory_is_not_a_missing_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("deleted-project");
        let not_found = || std::io::Error::from(std::io::ErrorKind::NotFound);
        match spawn_error(not_found(), Some(&gone), || "claude".into()) {
            crate::HarnessError::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::NotADirectory);
                assert_eq!(
                    error.to_string(),
                    format!("working directory {} does not exist", gone.display())
                );
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            spawn_error(not_found(), Some(tmp.path()), || "claude".into()),
            crate::HarnessError::NotInstalled(hint) if hint == "claude"
        ));
        assert!(matches!(
            spawn_error(not_found(), None, || "claude".into()),
            crate::HarnessError::NotInstalled(_)
        ));
        let busy = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(
            spawn_error(busy, Some(tmp.path()), || unreachable!()),
            crate::HarnessError::Io(error) if error.kind() == std::io::ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn explicit_install_invalidates_symlinked_wrapper_versions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let version = dir.path().join("version");
        let entry = dir.path().join("entry.js");
        let cli = dir.path().join("install-cache-test");
        std::fs::write(
            &entry,
            format!("#!/bin/sh\n/bin/cat '{}'\n", version.display()),
        )
        .unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&entry, &cli).unwrap();
        std::fs::write(&version, "1.0.0").unwrap();
        assert_eq!(binary_version(&cli).unwrap().to_string(), "1.0.0");
        std::fs::write(&version, "2.0.0").unwrap();
        assert_eq!(binary_version(&cli).unwrap().to_string(), "1.0.0");
        invalidate_versions(&["install-cache-test"]);
        assert_eq!(binary_version(&cli).unwrap().to_string(), "2.0.0");
    }

    #[test]
    fn newest_binary_deduplicates_caches_and_invalidates() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("one/codex");
        let second = dir.path().join("two/codex");
        let script = |path: &Path, version: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    "#!/bin/sh\necho codex-cli {version}\necho x >> '{}.calls'\n",
                    path.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        script(&first, "0.99.0");
        script(&second, "0.110.0");
        let alias = dir.path().join("alias");
        symlink(&second, &alias).unwrap();
        for _ in 0..3 {
            assert_eq!(
                newest_candidate(vec![first.clone(), second.clone(), alias.clone()]),
                Some(second.clone())
            );
        }
        assert_eq!(
            std::fs::read_to_string(second.with_file_name("codex.calls")).unwrap(),
            "x\n"
        );
        script(&first, "1.200.0");
        assert_eq!(
            newest_candidate(vec![first.clone(), second.clone()]),
            Some(first.clone())
        );
        script(&second, "1.200.0");
        assert_eq!(newest_candidate(vec![first.clone(), second]), Some(first));
    }

    #[test]
    fn stderr_versions_are_read_and_build_metadata_does_not_break_ties() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        for (path, version) in [(&first, "1.0.0+aaa"), (&second, "1.0.0+zzz")] {
            std::fs::write(path, format!("#!/bin/sh\necho codex-cli {version} >&2\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(binary_version(&first).unwrap().to_string(), "1.0.0+aaa");
        assert_eq!(newest_candidate(vec![first.clone(), second]), Some(first));
    }

    #[test]
    fn version_probe_bounds_hangs_and_rejects_nonzero() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in [("failed", "echo 9.0.0; exit 1"), ("hung", "sleep 30")] {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let started = std::time::Instant::now();
            assert_eq!(
                binary_version_with(&path, std::time::Duration::from_secs(1)),
                None
            );
            assert!(started.elapsed() < std::time::Duration::from_secs(3));
            assert_eq!(newest_candidate(vec![path.clone()]), Some(path));
        }
    }

    /// A first exec slower than the probe budget (an AV scan of a fresh
    /// install) must not pin "unknown version" for the process lifetime.
    #[test]
    fn timed_out_probe_is_retried_after_its_ttl_but_answers_are_kept() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slow-until-warm");
        let warm = dir.path().join("warm");
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\n[ -e '{}' ] || sleep 30\necho cli 3.1.0\n",
                warm.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let short = std::time::Duration::from_millis(500);
        assert_eq!(binary_version_with(&path, short), None);
        std::fs::write(&warm, "").unwrap();
        // Still inside the TTL: the inconclusive result is reused, not re-run.
        assert_eq!(binary_version_with(&path, short), None);
        // Age the entry past its TTL.
        let canonical = path.canonicalize().unwrap();
        for entry in version_cache()
            .entries
            .iter_mut()
            .filter(|((p, _, _), _)| p == &canonical)
            .map(|(_, entry)| entry)
        {
            assert!(entry.expires.is_some(), "a timeout is cached only briefly");
            entry.expires = Some(std::time::Instant::now());
        }
        assert_eq!(
            binary_version_with(&path, std::time::Duration::from_secs(10)),
            Some(semver::Version::new(3, 1, 0))
        );
        let entries = version_cache();
        let entry = entries
            .entries
            .iter()
            .find(|((p, _, _), _)| p == &canonical)
            .unwrap()
            .1;
        assert!(entry.expires.is_none(), "an answer is kept");
    }

    #[test]
    fn a_failed_probe_is_retried_once_its_failure_ages_out() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let answer = dir.path().join("answer");
        let path = dir.path().join("flaky-cli");
        std::fs::write(
            &path,
            format!("#!/bin/sh\ncat '{}' || exit 1\n", answer.display()),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(binary_version(&path), None);
        // The binary itself is unchanged, so its identity (and cache key) is too.
        std::fs::write(&answer, "flaky-cli 1.2.3\n").unwrap();
        assert_eq!(binary_version(&path), None, "a fresh failure is cached");
        let canonical = path.canonicalize().unwrap();
        for entry in version_cache()
            .entries
            .iter_mut()
            .filter(|((p, _, _), _)| p == &canonical)
            .map(|(_, entry)| entry)
        {
            assert!(entry.expires.is_some(), "a failure is cached only briefly");
            entry.expires = Some(std::time::Instant::now());
        }
        assert_eq!(binary_version(&path), Some(semver::Version::new(1, 2, 3)));
    }

    /// An install that replaces a wrapper in place (same metadata) while it
    /// is being probed: the probe's answer, from the old wrapper, is not
    /// cached over the invalidation.
    #[test]
    fn a_probe_racing_an_invalidation_does_not_cache_its_answer() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("racing-wrapper");
        let (calls, go) = (dir.path().join("calls"), dir.path().join("go"));
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\necho x >> '{}'\nwhile [ ! -e '{}' ]; do sleep 0.02; done\necho cli 1.0.0\n",
                calls.display(),
                go.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let budget = std::time::Duration::from_secs(30);
        let probe = {
            let path = path.clone();
            std::thread::spawn(move || binary_version_with(&path, budget))
        };
        while !calls.exists() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        invalidate_versions(&["racing-wrapper"]);
        std::fs::write(&go, "").unwrap();
        assert_eq!(probe.join().unwrap(), Some(semver::Version::new(1, 0, 0)));
        let canonical = path.canonicalize().unwrap();
        assert!(
            !version_cache()
                .entries
                .keys()
                .any(|(p, _, _)| p == &canonical),
            "the stale answer was cached"
        );
        // The next caller probes again.
        assert_eq!(
            binary_version_with(&path, budget),
            Some(semver::Version::new(1, 0, 0))
        );
        assert_eq!(std::fs::read_to_string(&calls).unwrap(), "x\nx\n");
    }

    /// Concurrent callers for one binary share a probe, and a slow binary
    /// does not hold up probes of another.
    #[test]
    fn concurrent_probes_coalesce_per_binary_without_a_global_wait() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let slow = dir.path().join("slow");
        let fast = dir.path().join("fast");
        std::fs::write(
            &slow,
            format!(
                "#!/bin/sh\necho x >> '{}'\nsleep 3\necho cli 1.0.0\n",
                dir.path().join("slow.calls").display()
            ),
        )
        .unwrap();
        std::fs::write(&fast, "#!/bin/sh\necho cli 2.0.0\n").unwrap();
        for path in [&slow, &fast] {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // A budget far past the slow binary's 3s: on a loaded machine the
        // default one could time it out, which is not what this tests.
        let budget = std::time::Duration::from_secs(30);
        let slow_callers: Vec<_> = (0..4)
            .map(|_| {
                let slow = slow.clone();
                std::thread::spawn(move || binary_version_with(&slow, budget))
            })
            .collect();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            binary_version_with(&fast, budget),
            Some(semver::Version::new(2, 0, 0))
        );
        assert!(
            slow_callers.iter().all(|caller| !caller.is_finished()),
            "the fast probe waited on the slow one"
        );
        for caller in slow_callers {
            assert_eq!(caller.join().unwrap(), Some(semver::Version::new(1, 0, 0)));
        }
        assert_eq!(
            std::fs::read_to_string(dir.path().join("slow.calls")).unwrap(),
            "x\n"
        );
    }

    fn env(values: &[(&str, OsString)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let values: HashMap<String, OsString> = values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect();
        move |key| values.get(key).cloned()
    }

    fn joined(paths: &[&Path]) -> OsString {
        std::env::join_paths(paths).unwrap()
    }

    #[test]
    fn unix_pi_discovery_finds_node_manager_defaults_without_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let lookup = env(&[("HOME", home.as_os_str().to_owned())]);
        for relative in [
            ".volta/bin",
            ".bun/bin",
            ".local/share/pnpm",
            "Library/pnpm",
            ".nvm/versions/node/v22/bin",
            ".fnm/aliases/default/bin",
            ".local/share/fnm/aliases/default/bin",
            ".local/bin",
        ] {
            let bin = home.join(relative);
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("pi"), "fixture").unwrap();
            // Pi supplies ~/.local/bin as an explicit npm fallback.
            let extra = vec![home.join(".local/bin/pi")];
            assert_eq!(
                find_on_paths_with("pi", extra, &lookup, None),
                Some(bin.join("pi")),
                "{relative}"
            );
            std::fs::remove_file(bin.join("pi")).unwrap();
        }
    }

    #[test]
    fn unix_discovery_preserves_exact_name_and_source_order() {
        let temp = tempfile::tempdir().unwrap();
        let path_dir = temp.path().join("path");
        let shell_dir = temp.path().join("shell");
        let extra_dir = temp.path().join("extra");
        for dir in [&path_dir, &shell_dir, &extra_dir] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("agent"), b"shim").unwrap();
        }
        let found = find_on_paths_with(
            "agent",
            vec![extra_dir.join("agent")],
            &env(&[("PATH", joined(&[&path_dir]))]),
            Some(joined(&[&shell_dir])),
        );
        assert_eq!(found, Some(path_dir.join("agent")));
    }

    #[test]
    fn portable_base_dir_uses_current_dir_when_home_is_unset() {
        let current = PathBuf::from("/work/comet");
        assert_eq!(
            home_or_current_dir_with(&env(&[]), &|| Ok(current.clone())),
            current
        );
    }
}
