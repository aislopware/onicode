//! Full Disk Access and the protected folders, by opening directories: `opendir` alone, never a
//! read of what is in them, and never TCC.db itself.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::Status;

/// Folders only Full Disk Access opens, under the home folder.
const FULL_DISK_HOME: [&str; 2] = ["Library/Safari", "Library/Mail"];
/// The same, outside it.
const FULL_DISK_SYSTEM: &str = "/Library/Application Support/com.apple.TCC";
/// The folders that prompt on first open, under the home folder.
const FOLDERS: [&str; 3] = ["Desktop", "Documents", "Downloads"];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// What opening `dir` says: on, refused, or nothing (it is not there, or failed otherwise).
fn opens(dir: &Path) -> Option<bool> {
    // `read_dir` is `opendir`; entries are read only when iterated, and this never iterates.
    match std::fs::read_dir(dir) {
        Ok(_dir) => Some(true),
        Err(err) if err.kind() == ErrorKind::PermissionDenied => Some(false),
        Err(_) => None,
    }
}

/// Sums several folders: on when every one that exists opens, off when one is refused.
fn all_open(dirs: impl IntoIterator<Item = PathBuf>) -> Status {
    let mut seen = false;
    for dir in dirs {
        match opens(&dir) {
            Some(true) => seen = true,
            Some(false) => return Status::Denied,
            None => {}
        }
    }
    if seen {
        Status::Granted
    } else {
        Status::Unknown
    }
}

/// Full Disk Access, by a spot check of folders only it opens. Never prompts: these folders fail
/// with `EPERM` rather than ask.
#[must_use]
pub(super) fn full_disk() -> Status {
    let home = home();
    let dirs = FULL_DISK_HOME
        .iter()
        .filter_map(|dir| home.as_ref().map(|home| home.join(dir)))
        .chain(std::iter::once(PathBuf::from(FULL_DISK_SYSTEM)));
    let mut seen = false;
    for dir in dirs {
        match opens(&dir) {
            // One protected folder that opens is enough.
            Some(true) => return Status::Granted,
            Some(false) => seen = true,
            None => {}
        }
    }
    if seen {
        Status::Denied
    } else {
        Status::Unknown
    }
}

/// Desktop, Documents and Downloads, once they were asked for: opening them again prompts no
/// more.
#[must_use]
pub(super) fn folders() -> Status {
    let Some(home) = home() else {
        return Status::Unknown;
    };
    all_open(FOLDERS.iter().map(|dir| home.join(dir)))
}

/// Opens Desktop, Documents and Downloads, and every network or removable volume mounted now, so
/// their prompts come now rather than in the middle of an agent's run. Each open waits while its
/// prompt is up. Only from the person's click.
pub(super) fn open_folders() {
    if let Some(home) = home() {
        for dir in FOLDERS {
            let _open = opens(&home.join(dir));
        }
    }
    for volume in volumes() {
        let _open = opens(&volume);
    }
}

/// The volumes under `/Volumes` other than the startup disk, which is a link to `/`.
fn volumes() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/Volumes") else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}
