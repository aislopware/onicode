//! The editor's caps on a file it opens, and which files it plays rather than reads.
use std::path::Path;

pub const MAX_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_LINE_BYTES: usize = 16 * 1024;

pub fn is_video_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "mp4"
                    | "m4v"
                    | "mov"
                    | "webm"
                    | "ogv"
                    | "avi"
                    | "mkv"
                    | "mpg"
                    | "mpeg"
                    | "3gp"
                    | "3g2"
            )
        })
}
