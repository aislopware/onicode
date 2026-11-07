//! Bounded file reads and atomic editor saves, extracted from Tauri's fs service.
use std::io::Read;

pub use onicode_protocol::files::document::{MAX_LINE_BYTES, MAX_TEXT_BYTES, is_video_path};

use super::*;
const MAX_PREVIEW_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct TextDocument {
    pub text: String,
    pub crlf: bool,
    pub cr: bool,
    pub bom: bool,
    /// Original bytes are retained to detect an external edit before saving.
    pub disk: Vec<u8>,
}
#[derive(Debug)]
pub enum Document {
    Text(TextDocument),
    Image {
        bytes: Vec<u8>,
        format: &'static str,
        width: u32,
        height: u32,
        original_size: usize,
    },
    Video {
        size: u64,
        modified: Option<std::time::SystemTime>,
    },
    Unsupported(String),
}

/// Resolve an editor link against its workspace without touching disk.
pub fn resolve_path(path: &Path, cwd: &Path) -> PathBuf {
    let path = expand_home(path);
    let path = if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    };
    let mut clean = PathBuf::new();
    for component in path.components() {
        if component == std::path::Component::ParentDir {
            clean.pop();
        } else if component != std::path::Component::CurDir {
            clean.push(component);
        }
    }
    clean
}

pub fn read_document(path: &Path) -> Result<Document, String> {
    let path = expand_home(path);
    let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("Not a regular file".into());
    }
    if is_video_path(&path) {
        return Ok(Document::Video {
            size: meta.len(),
            modified: meta.modified().ok(),
        });
    }
    if meta.len() > MAX_PREVIEW_BYTES {
        return Ok(Document::Unsupported(
            "File is too large to preview (maximum 32 MB).".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_PREVIEW_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_PREVIEW_BYTES {
        return Ok(Document::Unsupported(
            "File is too large to preview (maximum 32 MB).".into(),
        ));
    }
    document_from_bytes(&path, bytes)
}

fn document_from_bytes(path: &Path, bytes: Vec<u8>) -> Result<Document, String> {
    let format = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("webp")
    } else if bytes.starts_with(b"\x00\x00\x01\x00") {
        Some("ico")
    } else if bytes.get(4..8) == Some(b"ftyp")
        && matches!(bytes.get(8..12), Some(b"avif" | b"avis"))
    {
        Some("avif")
    } else if bytes.starts_with(b"BM") {
        Some("bmp")
    } else {
        None
    };
    if let Some(format) = format {
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok())
            .unwrap_or((0, 0));
        if (width == 0 || height == 0) && format != "avif" {
            return Ok(Document::Unsupported("Not a readable image.".into()));
        }
        return Ok(Document::Image {
            original_size: bytes.len(),
            bytes,
            format,
            width,
            height,
        });
    }
    if path
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "bmp" | "ico"
            )
        })
    {
        return Ok(Document::Unsupported("Not a readable image.".into()));
    }
    if bytes.len() > MAX_TEXT_BYTES {
        return Ok(Document::Unsupported(format!(
            "File is too large to edit (maximum {} MB).",
            MAX_TEXT_BYTES / 1024 / 1024
        )));
    }
    if bytes.contains(&0) {
        return Ok(Document::Unsupported(
            "Binary files cannot be edited.".into(),
        ));
    }
    let bom = bytes.starts_with(b"\xef\xbb\xbf");
    let Ok(raw) = std::str::from_utf8(if bom { &bytes[3..] } else { &bytes }) else {
        return Ok(Document::Unsupported("This file is not UTF-8 text.".into()));
    };
    let text = raw.replace("\r\n", "\n").replace('\r', "\n");
    if text.lines().any(|line| line.len() > MAX_LINE_BYTES) {
        return Ok(Document::Unsupported(
            "This file contains lines too long to edit (maximum 16 KB per line).".into(),
        ));
    }
    let crlf = raw.contains("\r\n");
    Ok(Document::Text(TextDocument {
        text,
        crlf,
        cr: !crlf && raw.contains('\r'),
        bom,
        disk: bytes,
    }))
}

impl TextDocument {
    pub fn encode(&self, text: &str) -> Vec<u8> {
        let mut out = if self.bom {
            vec![0xef, 0xbb, 0xbf]
        } else {
            Vec::new()
        };
        if self.crlf {
            out.extend_from_slice(text.replace('\n', "\r\n").as_bytes());
        } else if self.cr {
            out.extend_from_slice(text.replace('\n', "\r").as_bytes());
        } else {
            out.extend_from_slice(text.as_bytes());
        }
        out
    }
}

/// Preserve permissions and symlinks, and refuse to overwrite a changed file.
pub fn save_document(path: &Path, document: &TextDocument, text: &str) -> Result<Vec<u8>, String> {
    if text.len() > MAX_TEXT_BYTES || text.split('\n').any(|line| line.len() > MAX_LINE_BYTES) {
        return Err("Text exceeds the editor size limit.".into());
    }
    let destination = std::fs::canonicalize(expand_home(path)).map_err(|e| e.to_string())?;
    if !matches_disk(&destination, &document.disk)? {
        return Err("The file changed on disk. Reload it before saving your edits.".into());
    }
    let parent = destination.parent().ok_or("File has no parent directory")?;
    let name = destination
        .file_name()
        .ok_or("Invalid file name")?
        .to_string_lossy();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(
        ".{name}.monocode-{}-{stamp}.tmp",
        std::process::id()
    ));
    let bytes = document.encode(text);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        file.write_all(&bytes).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let permissions = std::fs::metadata(&destination)
            .map_err(|e| e.to_string())?
            .permissions();
        std::fs::set_permissions(&temporary, permissions).map_err(|e| e.to_string())?;
        // Check again after writing, so an edit during the save is also caught.
        if !matches_disk(&destination, &document.disk)? {
            return Err("The file changed on disk. Reload it before saving your edits.".into());
        }
        drop(file);
        std::fs::rename(&temporary, &destination).map_err(|e| e.to_string())?;
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(bytes)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn matches_disk(path: &Path, expected: &[u8]) -> Result<bool, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut bytes = Vec::with_capacity(expected.len());
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    Ok(bytes == expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_keeps_its_limit_and_line_endings() {
        let path = Path::new("/code/a.txt");
        assert!(matches!(
            document_from_bytes(path, vec![b'a'; MAX_TEXT_BYTES + 1]).unwrap(),
            Document::Unsupported(_)
        ));
        let Document::Text(document) =
            document_from_bytes(path, b"\xef\xbb\xbfa\r\n".to_vec()).unwrap()
        else {
            panic!("expected text")
        };
        assert_eq!(document.encode("changed\n"), b"\xef\xbb\xbfchanged\r\n");
    }
    #[test]
    fn atomic_saves_preserve_crlf_bom_permissions_and_reject_external_changes() {
        let root = std::env::temp_dir().join(format!("onicode-document-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("source.rs");
        std::fs::write(&path, b"\xef\xbb\xbffn main() {}\r\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let Document::Text(mut doc) = read_document(&path).unwrap() else {
            panic!()
        };
        assert_eq!(doc.text, "fn main() {}\n");
        doc.disk = save_document(&path, &doc, "// 🙂\n").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            "\u{feff}// 🙂\r\n".as_bytes()
        );
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        std::fs::write(&path, "external").unwrap();
        assert!(
            save_document(&path, &doc, "draft")
                .unwrap_err()
                .contains("changed on disk")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "external");
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn editor_saves_keep_each_line_ending_convention() {
        let path =
            std::env::temp_dir().join(format!("onicode-line-endings-{}.txt", std::process::id()));
        for eol in ["\n", "\r\n", "\r"] {
            std::fs::write(&path, format!("first{eol}second{eol}")).unwrap();
            let Document::Text(doc) = read_document(&path).unwrap() else {
                panic!("text document")
            };
            assert_eq!(doc.text, "first\nsecond\n");
            save_document(&path, &doc, "🙂\nchanged\n").unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!("🙂{eol}changed{eol}")
            );
        }
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn binary_invalid_utf8_and_long_lines_are_explicit() {
        let path =
            std::env::temp_dir().join(format!("onicode-document-types-{}", std::process::id()));
        for bytes in [
            vec![0],
            vec![0xff],
            vec![b'x'; MAX_LINE_BYTES + 1],
            vec![b'x'; MAX_TEXT_BYTES + 1],
        ] {
            std::fs::write(&path, bytes).unwrap();
            assert!(matches!(
                read_document(&path).unwrap(),
                Document::Unsupported(_)
            ));
        }
        std::fs::write(&path, b"").unwrap();
        assert!(matches!(read_document(&path).unwrap(), Document::Text(_)));
        std::fs::remove_file(path).unwrap();
    }
}
