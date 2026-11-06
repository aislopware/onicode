//! The engine's one profile: this device's own data, under its data directory.
//!
//! Stores, journals, and uploads live under the profile's store root.
//! Repositories, worktrees, agent credentials, UI settings, and the device id
//! stay device-scoped under the engine data directory. Who the device is comes
//! from its device id and the tailnet node it runs on, never from an account.

use std::path::{Path, PathBuf};

use crate::EngineError;

/// The storage boundary of one engine runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineProfile {
    device_root: PathBuf,
    store_root: PathBuf,
    uploads_root: PathBuf,
}

impl EngineProfile {
    /// The profile under `data_dir`, its directories created.
    pub fn local(data_dir: &Path) -> Result<Self, EngineError> {
        let store_root = data_dir.join("profiles").join("local");
        std::fs::create_dir_all(&store_root)?;
        Ok(Self {
            device_root: data_dir.to_path_buf(),
            uploads_root: store_root.join("uploads"),
            store_root,
        })
    }

    pub fn device_root(&self) -> &Path {
        &self.device_root
    }

    pub fn store_root(&self) -> &Path {
        &self.store_root
    }

    pub fn uploads_root(&self) -> &Path {
        &self.uploads_root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_profile_lives_under_the_data_dir() {
        let dir = tempfile::tempdir().unwrap();
        let profile = EngineProfile::local(dir.path()).unwrap();
        let root = dir.path().join("profiles/local");

        assert_eq!(profile.device_root(), dir.path());
        assert_eq!(profile.store_root(), root);
        assert_eq!(profile.uploads_root(), root.join("uploads"));
        assert!(root.is_dir());
    }

    #[test]
    fn a_file_where_the_profile_goes_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/local"), b"not a directory").unwrap();

        assert!(EngineProfile::local(dir.path()).is_err());
    }

    #[tokio::test]
    async fn the_assembler_uses_the_profile_roots() {
        let dir = tempfile::tempdir().unwrap();
        let core = crate::EngineCore::assemble(
            dir.path(),
            std::sync::Arc::new(crate::default_registry()),
            onicode_proto::HarnessId::Mock,
        )
        .unwrap();

        assert_eq!(
            core.uploads.dir(),
            dir.path().join("profiles/local/uploads")
        );
        core.shutdown().await;
    }
}
