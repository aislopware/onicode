//! A transcript's rows live in `onicode-protocol`; what reads the disk for them stays here.
use std::path::PathBuf;

pub use onicode_protocol::transcript::*;

/// [`Transcript::attach_files_with`] with each file's size read from the disk, off the UI thread.
pub trait AttachFiles {
    fn attach_files(&mut self, paths: &[PathBuf]);
}

impl AttachFiles for Transcript {
    fn attach_files(&mut self, paths: &[PathBuf]) {
        self.attach_files_with(paths, |path| {
            path.metadata().ok().map(|metadata| metadata.len())
        });
    }
}
