//! Where a transfer writes: the destination's filesystem, behind a trait so a test can watch or
//! break it. Every call blocks; the puller makes them on the blocking pool.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt as _, PermissionsExt as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use crate::manifest::{Entry, Kind};

/// What is already at a destination path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Existing {
    /// Bytes; 0 for a folder.
    pub size: u64,
    /// Modified, epoch milliseconds.
    pub modified_ms: i64,
    /// Whether it is a folder (not a link to one).
    pub dir: bool,
}

/// A file being written at any offset.
pub trait Partial: Send + Sync {
    /// Writes `data` at `offset`.
    ///
    /// # Errors
    ///
    /// The write's.
    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<()>;
}

/// The destination's filesystem.
pub trait Sink: Send + Sync + 'static {
    /// What is at `path`, not following a link; `None` when nothing is.
    fn existing(&self, path: &Path) -> Option<Existing>;
    /// Makes the folder `path` and its parents.
    ///
    /// # Errors
    ///
    /// The filesystem's.
    fn make_dir(&self, path: &Path) -> io::Result<()>;
    /// Makes `path` a symlink to `target`, replacing what is there.
    ///
    /// # Errors
    ///
    /// The filesystem's.
    fn symlink(&self, target: &str, path: &Path) -> io::Result<()>;
    /// Opens the partial file `path` (made, with its parents, if missing; kept if there) at
    /// `size` bytes, its space reserved up front.
    ///
    /// # Errors
    ///
    /// The filesystem's.
    fn create(&self, path: &Path, size: u64) -> io::Result<Arc<dyn Partial>>;
    /// Gives `path` the metadata, mtime and mode of `entry`.
    ///
    /// # Errors
    ///
    /// The filesystem's.
    fn finish(&self, path: &Path, entry: &Entry) -> io::Result<()>;
    /// Moves `from` to `to`; with `replace` whatever is at `to` goes first, else `to` must be
    /// free.
    ///
    /// # Errors
    ///
    /// The filesystem's; [`io::ErrorKind::AlreadyExists`] when `to` is taken.
    fn place(&self, from: &Path, to: &Path, replace: bool) -> io::Result<()>;
    /// Removes `path`, a file or a whole tree; nothing there is fine.
    ///
    /// # Errors
    ///
    /// The filesystem's.
    fn remove(&self, path: &Path) -> io::Result<()>;
}

/// The local filesystem.
#[derive(Clone, Copy, Debug, Default)]
pub struct Disk;

struct DiskFile(File);

impl Partial for DiskFile {
    fn write_at(&self, data: &[u8], offset: u64) -> io::Result<()> {
        self.0.write_all_at(data, offset)
    }
}

impl Sink for Disk {
    fn existing(&self, path: &Path) -> Option<Existing> {
        let meta = fs::symlink_metadata(path).ok()?;
        let dir = meta.is_dir();
        Some(Existing {
            size: if dir { 0 } else { meta.len() },
            modified_ms: crate::manifest::mtime_ms(&meta),
            dir,
        })
    }

    fn make_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    fn symlink(&self, target: &str, path: &Path) -> io::Result<()> {
        if fs::symlink_metadata(path).is_ok() {
            self.remove(path)?;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(target, path)
    }

    fn create(&self, path: &Path, size: u64) -> io::Result<Arc<dyn Partial>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let open = || {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
        };
        let file = match open() {
            // A file finished in an earlier run took its read-only mode; it is fetched anew.
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                fs::remove_file(path)?;
                open()?
            }
            other => other?,
        };
        if file.metadata()?.len() != size {
            preallocate(&file, size);
            file.set_len(size)?;
        }
        Ok(Arc::new(DiskFile(file)))
    }

    fn finish(&self, path: &Path, entry: &Entry) -> io::Result<()> {
        let symlink = entry.kind == Kind::Symlink;
        if let Some(meta) = entry
            .meta_bytes()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?
            && let Err(error) = crate::meta::unpack(path, &meta, symlink)
        {
            // Metadata is best kept, never worth failing the copy over.
            tracing::debug!(path = %path.display(), %error, "metadata not restored");
        }
        if symlink {
            return Ok(());
        }
        // Times before the mode: the owner sets explicit times through a read-only
        // descriptor, which a mode-000 file would refuse to open.
        let millis = u64::try_from(entry.mtime_ms.max(0)).unwrap_or(0);
        let modified = UNIX_EPOCH + Duration::from_millis(millis);
        File::open(path)?.set_times(fs::FileTimes::new().set_modified(modified))?;
        fs::set_permissions(path, fs::Permissions::from_mode(entry.mode & 0o7777))
    }

    fn place(&self, from: &Path, to: &Path, replace: bool) -> io::Result<()> {
        if let Some(there) = self.existing(to) {
            if !replace {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is taken", to.display()),
                ));
            }
            // A file replaces a file in one rename; anything else goes first.
            let from_dir = fs::symlink_metadata(from)?.is_dir();
            if there.dir || from_dir {
                self.remove(to)?;
            }
        }
        fs::rename(from, to)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => {
                // A folder left read-only by its mode would refuse its children's removal.
                make_writable(path);
                fs::remove_dir_all(path)
            }
            Ok(_) => fs::remove_file(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Makes every folder under `path` writable by its owner, so the tree can be removed.
fn make_writable(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o700));
    }
    if let Ok(children) = fs::read_dir(path) {
        for child in children.flatten() {
            make_writable(&child.path());
        }
    }
}

/// Reserves `size` bytes for `file` up front, contiguous if the volume can, so a big copy
/// neither fragments nor finds the disk full near its end. Best effort.
#[cfg(target_os = "macos")]
fn preallocate(file: &File, size: u64) {
    use std::os::fd::AsRawFd as _;
    let Ok(length) = libc::off_t::try_from(size) else {
        return;
    };
    let mut store = libc::fstore_t {
        fst_flags: libc::F_ALLOCATECONTIG | libc::F_ALLOCATEALL,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: length,
        fst_bytesalloc: 0,
    };
    // SAFETY: the descriptor is open for writing and `store` lives across the call.
    let mut rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &raw mut store) };
    if rc == -1 {
        // No contiguous run that long: any space will do.
        store.fst_flags = libc::F_ALLOCATEALL;
        // SAFETY: as above.
        rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &raw mut store) };
    }
    if rc == -1 {
        tracing::debug!(error = %io::Error::last_os_error(), "preallocation refused");
    }
}

#[cfg(not(target_os = "macos"))]
fn preallocate(file: &File, size: u64) {
    let _ = (file, size);
}
