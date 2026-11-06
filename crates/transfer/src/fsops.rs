//! Changes the path browser makes on a peer's filesystem beside copies.

use std::io;
use std::path::Path;

/// Moves `path` to the Trash, as Finder does (`NSFileManager trashItemAtURL`).
///
/// # Errors
///
/// What `NSFileManager` says; [`io::ErrorKind::Unsupported`] off macOS, which has no Trash to
/// use.
pub fn trash(path: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSFileManager, NSString, NSURL};
        let text = path
            .to_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path is not UTF-8"))?;
        let url = NSURL::fileURLWithPath(&NSString::from_str(text));
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|error| io::Error::other(error.localizedDescription().to_string()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no Trash on this platform",
        ))
    }
}
