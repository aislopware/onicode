//! The instance lock: one engine per data dir, so one per machine. An advisory `flock` on
//! `{data_dir}/engine.lock`, which the kernel lets go when its holder exits however it exits, so a
//! crashed engine never wedges the next one. The file names its holder ([`Holder`]): the process
//! a viewer may stop, and the executable it runs, which a viewer compares with the one it would
//! launch.
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// The lock file inside the data dir.
pub const LOCK: &str = "engine.lock";

/// How often a viewer waiting for an engine to let go looks again.
const POLL: Duration = Duration::from_millis(20);

/// Who holds the lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    /// The holder's process.
    pub pid: u32,
    /// The executable the holder runs.
    pub exe: PathBuf,
    /// That executable's modification time, in nanoseconds since the Unix epoch: a rebuilt or
    /// updated engine on disk differs from the one running.
    pub built: Option<u64>,
}

impl Holder {
    /// This process, running `exe`.
    #[must_use]
    pub fn this(exe: &Path) -> Self {
        Self {
            pid: std::process::id(),
            exe: exe.to_path_buf(),
            built: build_stamp(exe),
        }
    }

    /// Whether the holder runs `exe` as it is on disk now.
    #[must_use]
    pub fn runs(&self, exe: &Path) -> bool {
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        same(&self.exe, exe) && self.built == build_stamp(exe)
    }
}

/// `exe`'s modification time in nanoseconds since the Unix epoch.
#[must_use]
pub fn build_stamp(exe: &Path) -> Option<u64> {
    let modified = std::fs::metadata(exe)
        .and_then(|meta| meta.modified())
        .ok()?;
    let since = modified.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(since.as_nanos()).ok()
}

/// The held lock. Dropping it lets it go.
pub struct InstanceLock {
    file: File,
}

impl InstanceLock {
    /// Takes the lock in `dir`; `None` when another process (or another open of it in this one)
    /// holds it.
    pub fn acquire(dir: &Path) -> std::io::Result<Option<Self>> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join(LOCK))?;
        // SAFETY: `flock` on a descriptor this function owns; it reads no memory.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if taken == 0 {
            return Ok(Some(Self { file }));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(None)
        } else {
            Err(error)
        }
    }

    /// Names `holder` in the lock file.
    pub fn record(&mut self, holder: &Holder) -> std::io::Result<()> {
        let json = serde_json::to_vec(holder).map_err(std::io::Error::other)?;
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&json)?;
        self.file.flush()
    }

    /// Whether a process holds the lock in `dir` now.
    #[must_use]
    pub fn held(dir: &Path) -> bool {
        matches!(Self::acquire(dir), Ok(None))
    }

    /// Who holds the lock in `dir`, while one does.
    #[must_use]
    pub fn holder(dir: &Path) -> Option<Holder> {
        if !Self::held(dir) {
            return None;
        }
        let bytes = std::fs::read(dir.join(LOCK)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Takes the lock in `dir`, waiting up to `wait` for its holder to let it go.
    pub fn wait(dir: &Path, wait: Duration) -> std::io::Result<Option<Self>> {
        let deadline = Instant::now() + wait;
        loop {
            if let Some(lock) = Self::acquire(dir)? {
                return Ok(Some(lock));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(POLL);
        }
    }
}

/// Stops the engine holding the lock in `dir`, and waits until it let go: `SIGTERM` first (it
/// stops its agents and removes its socket), `SIGKILL` once `grace` passes. Blocks; whether the
/// lock is free at the end.
pub fn stop_holder(dir: &Path, grace: Duration) -> bool {
    let Some(holder) = InstanceLock::holder(dir) else {
        return !InstanceLock::held(dir);
    };
    let Ok(pid) = libc::pid_t::try_from(holder.pid) else {
        return false;
    };
    let gone = |wait: Duration| {
        let deadline = Instant::now() + wait;
        while InstanceLock::held(dir) {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
        true
    };
    // SAFETY: `kill` only sends a signal. The lock is held, so `pid` is its live holder's.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    if gone(grace) {
        return true;
    }
    if InstanceLock::held(dir) {
        // SAFETY: as above, while the lock is still held.
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
    gone(Duration::from_secs(1))
}
