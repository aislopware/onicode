//! An item's Mac metadata (xattrs, ACL, resource fork, Finder info) as one AppleDouble blob,
//! through `copyfile(3)`: `COPYFILE_PACK` on the source, `COPYFILE_UNPACK` on the destination
//! before the rename. Elsewhere there is nothing to carry.
//!
//! An item with no extended attributes is sent with none, so an ACL alone is not carried.

use std::io;
use std::path::Path;

/// `path`'s metadata as AppleDouble, `None` when it has none; `symlink` packs the link itself.
///
/// # Errors
///
/// copyfile's, or the scratch file's.
pub fn pack(path: &Path, symlink: bool) -> io::Result<Option<Vec<u8>>> {
    #[cfg(target_os = "macos")]
    {
        mac::pack(path, symlink)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, symlink);
        Ok(None)
    }
}

/// Restores `meta` (from [`pack`]) onto `path`; `symlink` writes to the link itself.
///
/// # Errors
///
/// copyfile's, or the scratch file's.
pub fn unpack(path: &Path, meta: &[u8], symlink: bool) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::unpack(path, meta, symlink)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, meta, symlink);
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::CString;
    use std::io;
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Path, PathBuf};

    fn c_path(path: &Path) -> io::Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in the path"))
    }

    /// A scratch file for the AppleDouble, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!(".onicode-meta-{}", uuid::Uuid::new_v4().simple())),
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    const PROVENANCE: &[u8] = b"com.apple.provenance";

    pub(super) fn pack(path: &Path, symlink: bool) -> io::Result<Option<Vec<u8>>> {
        let from = c_path(path)?;
        let options = if symlink { libc::XATTR_NOFOLLOW } else { 0 };
        // SAFETY: `from` is a NUL-terminated path; a null buffer of size 0 asks for the length.
        let listed = unsafe { libc::listxattr(from.as_ptr(), std::ptr::null_mut(), 0, options) };
        let Ok(listed) = usize::try_from(listed) else {
            return Ok(None);
        };
        let mut names = vec![0_u8; listed];
        // SAFETY: as above, with a buffer of the stated length.
        let got =
            unsafe { libc::listxattr(from.as_ptr(), names.as_mut_ptr().cast(), listed, options) };
        names.truncate(usize::try_from(got).unwrap_or(0));
        // The kernel tags every file a process writes with `com.apple.provenance` and tags the
        // copy itself, so a file with nothing else has nothing worth sending.
        if names
            .split(|byte| *byte == 0)
            .all(|name| name.is_empty() || name == PROVENANCE)
        {
            return Ok(None);
        }
        let scratch = Scratch::new();
        let to = c_path(&scratch.0)?;
        let mut flags = libc::COPYFILE_PACK | libc::COPYFILE_ACL | libc::COPYFILE_XATTR;
        if symlink {
            flags |= libc::COPYFILE_NOFOLLOW_SRC;
        }
        // SAFETY: both paths are NUL-terminated and live across the call; no state is passed.
        let rc = unsafe { libc::copyfile(from.as_ptr(), to.as_ptr(), std::ptr::null_mut(), flags) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(std::fs::read(&scratch.0)?))
    }

    pub(super) fn unpack(path: &Path, meta: &[u8], symlink: bool) -> io::Result<()> {
        let scratch = Scratch::new();
        std::fs::write(&scratch.0, meta)?;
        let from = c_path(&scratch.0)?;
        let to = c_path(path)?;
        let mut flags = libc::COPYFILE_UNPACK | libc::COPYFILE_ACL | libc::COPYFILE_XATTR;
        if symlink {
            flags |= libc::COPYFILE_NOFOLLOW_DST;
        }
        // SAFETY: both paths are NUL-terminated and live across the call; no state is passed.
        let rc = unsafe { libc::copyfile(from.as_ptr(), to.as_ptr(), std::ptr::null_mut(), flags) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn xattrs_survive_pack_and_unpack() {
            let dir = tempfile::tempdir().unwrap();
            let from = dir.path().join("from");
            let to = dir.path().join("to");
            std::fs::write(&from, b"x").unwrap();
            std::fs::write(&to, b"x").unwrap();
            assert_eq!(
                pack(&from, false).unwrap(),
                None,
                "only the provenance tag, nothing packed"
            );
            let name = CString::new("com.onicode.test").unwrap();
            let value = b"kept";
            let path = c_path(&from).unwrap();
            // SAFETY: NUL-terminated name and path; the value's pointer and length agree.
            let rc = unsafe {
                libc::setxattr(
                    path.as_ptr(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            };
            assert_eq!(rc, 0);
            let packed = pack(&from, false).unwrap().expect("an xattr to pack");
            unpack(&to, &packed, false).unwrap();
            let mut read = [0_u8; 16];
            let to_path = c_path(&to).unwrap();
            // SAFETY: as above, with a buffer of the stated length.
            let got = unsafe {
                libc::getxattr(
                    to_path.as_ptr(),
                    name.as_ptr(),
                    read.as_mut_ptr().cast(),
                    read.len(),
                    0,
                    0,
                )
            };
            assert_eq!(usize::try_from(got).unwrap(), value.len());
            assert_eq!(&read[..value.len()], value);
        }
    }
}
