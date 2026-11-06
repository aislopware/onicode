//! Jobs on disk, so a transfer outlives a restarted engine: `<id>.json` for the job and its
//! progress, rewritten as it moves, and `<id>.manifest.json` beside it, written once a run.
//! Each file is written whole to a temporary name and renamed over the old one.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs, io};

use crate::job::Job;
use crate::manifest::Entry;

/// Where jobs are kept; `None` keeps nothing (tests).
#[derive(Clone, Debug, Default)]
pub struct Store {
    dir: Option<PathBuf>,
}

impl Store {
    /// Jobs kept in `dir`.
    #[must_use]
    pub const fn at(dir: PathBuf) -> Self {
        Self { dir: Some(dir) }
    }

    /// Nothing kept.
    #[must_use]
    pub const fn none() -> Self {
        Self { dir: None }
    }

    /// Writes `job`'s state.
    ///
    /// # Errors
    ///
    /// The filesystem's.
    pub fn save(&self, job: &Job) -> io::Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let json = serde_json::to_vec(job).map_err(io::Error::other)?;
        write_whole(dir, &format!("{}.json", job.id), &json)
    }

    /// Writes `job`'s manifest.
    ///
    /// # Errors
    ///
    /// The filesystem's.
    pub fn save_manifest(&self, id: &str, entries: &[Entry]) -> io::Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let json = serde_json::to_vec(entries).map_err(io::Error::other)?;
        write_whole(dir, &format!("{id}.manifest.json"), &json)
    }

    /// Forgets `id`.
    pub fn remove(&self, id: &str) {
        if let Some(dir) = &self.dir {
            let _ = fs::remove_file(dir.join(format!("{id}.json")));
            let _ = fs::remove_file(dir.join(format!("{id}.manifest.json")));
        }
    }

    /// Every job kept, with its manifest. A file that does not parse is left alone and skipped.
    #[must_use]
    pub fn load(&self) -> Vec<Job> {
        let Some(dir) = &self.dir else {
            return Vec::new();
        };
        let Ok(listing) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut jobs = Vec::new();
        for file in listing.flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if id.ends_with(".manifest") || id.contains(".tmp") {
                continue;
            }
            let job = fs::read(file.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Job>(&bytes).ok());
            let Some(mut job) = job else {
                tracing::warn!(file = %name, "a kept transfer does not parse");
                continue;
            };
            let manifest = fs::read(dir.join(format!("{id}.manifest.json")))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Vec<Entry>>(&bytes).ok())
                .unwrap_or_default();
            if manifest.is_empty() {
                // Progress means nothing without the manifest it indexes.
                job.done = crate::spans::Spans::default();
                job.partial.clear();
            }
            job.manifest = Arc::new(manifest);
            jobs.push(job);
        }
        jobs.sort_by_key(|job| job.created_ms);
        jobs
    }
}

fn write_whole(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{name}.tmp"));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, dir.join(name))
}
