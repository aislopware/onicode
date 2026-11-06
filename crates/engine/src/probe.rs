//! The reads behind the Files and Git watches: a directory's listing and a checkout's status. A
//! seam, so the watch hubs' model tests run against a disk they script, with no `git`.
use std::collections::BTreeMap;
use std::path::Path;

use onicode_core::files::DirEntry;
use onicode_core::scm::{DiffIndex, FileDiffVersion};

/// A checkout's status as one read finds it: the index and each change's version.
pub type Status = (DiffIndex, BTreeMap<String, FileDiffVersion>);

pub trait Probe: Send + Sync + 'static {
    /// The children of `path`, folders first, with their ignore marks.
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, String>;

    /// The status of the checkout at `checkout`.
    fn status(&self, checkout: &Path) -> Result<Status, String>;
}

/// The real disk and `git`.
pub struct DiskProbe;

impl Probe for DiskProbe {
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, String> {
        onicode_core::files::list_dir(path)
    }

    fn status(&self, checkout: &Path) -> Result<Status, String> {
        let index = onicode_core::scm::diff_index(checkout)?;
        let versions = onicode_core::scm::file_diff_versions(checkout, &index)
            .map(|versions| versions.into_iter().collect())
            .unwrap_or_default();
        Ok((index, versions))
    }
}

/// A disk the Files and Git model tests script: listings and statuses they set, read back as
/// set, with a count of reads.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Scripted {
    pub(crate) dirs: std::sync::Mutex<BTreeMap<String, Result<Vec<DirEntry>, String>>>,
    pub(crate) statuses: std::sync::Mutex<BTreeMap<String, Result<Status, String>>>,
    pub(crate) reads: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl Probe for Scripted {
    fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, String> {
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.dirs
            .lock()
            .unwrap()
            .get(path.to_string_lossy().as_ref())
            .cloned()
            .unwrap_or_else(|| Err(format!("{}: No such file or directory", path.display())))
    }

    fn status(&self, checkout: &Path) -> Result<Status, String> {
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.statuses
            .lock()
            .unwrap()
            .get(checkout.to_string_lossy().as_ref())
            .cloned()
            .unwrap_or_else(|| Err(format!("{}: not a git repository", checkout.display())))
    }
}
