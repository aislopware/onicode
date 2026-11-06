//! Stable device-local browser storage, independent of chats and repositories.
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BrowserProfile {
    pub root: PathBuf,
}

impl BrowserProfile {
    /// This device's browser store. `None` until the engine names the device.
    pub fn for_device(data_dir: &Path, local_device_id: Option<&str>) -> Option<Self> {
        Some(Self::new(data_dir, &["local", local_device_id?]))
    }

    fn new(data_dir: &Path, identity: &[&str]) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"onicode-browser-profile-v1");
        for part in identity {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        // Resolve aliases so windows using the same installation share a store.
        let data_dir = data_dir.canonicalize().unwrap_or_else(|_| {
            std::path::absolute(data_dir).unwrap_or_else(|_| data_dir.to_path_buf())
        });
        Self {
            root: data_dir
                .join("browser")
                .join(format!("{:x}", hash.finalize())),
        }
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn data_store_identifier(&self) -> [u8; 16] {
        // Include the installation directory: fixtures and alternative data roots
        // must never open a real user's WKWebsiteDataStore.
        let mut hash = Sha256::new();
        hash.update(b"onicode-wkwebsite-data-store-v1");
        hash.update(self.root.as_os_str().as_encoded_bytes());
        let mut id: [u8; 16] = hash.finalize()[..16].try_into().unwrap();
        id[6] = (id[6] & 0x0f) | 0x80; // UUID v8, application-defined hash.
        id[8] = (id[8] & 0x3f) | 0x80;
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_stable_and_scoped_to_device_and_installation() {
        let root = tempfile::tempdir().unwrap();
        let make = |parts: &[&str]| BrowserProfile::new(root.path(), parts);
        let a = make(&["local", "device-a"]);
        assert_eq!(a, make(&["local", "device-a"]));
        let b = make(&["local", "device-b"]);
        assert_ne!(a.root, b.root);
        assert_ne!(a.data_store_identifier(), b.data_store_identifier());
        let other = tempfile::tempdir().unwrap();
        let elsewhere = BrowserProfile::new(other.path(), &["local", "device-a"]);
        assert_ne!(a.data_store_identifier(), elsewhere.data_store_identifier());
        assert_ne!(make(&["ab", "c"]), make(&["a", "bc"]));
        assert!(
            make(&["../../outside"])
                .root
                .starts_with(root.path().canonicalize().unwrap().join("browser"))
        );
    }

    #[test]
    fn an_unnamed_device_selects_no_persistent_profile() {
        let root = Path::new("/unused");
        assert!(BrowserProfile::for_device(root, None).is_none());
        let local = |device| BrowserProfile::for_device(root, Some(device));
        assert_eq!(local("one"), local("one"));
        assert_ne!(local("one"), local("two"));
    }
}
