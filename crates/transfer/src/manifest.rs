//! What a transfer carries: the source walks each path it was asked for, recursively, and lists
//! every file, folder and symlink under it with its mode, mtime and metadata.
//!
//! Bundles and packages are folders here, copied as trees; a symlink is copied as a symlink and
//! never followed. Sockets, FIFOs and devices are left out. Entries come in tree order, each
//! folder's children by name, so a folder is always listed before what is in it.

use std::collections::HashMap;
use std::fs::{self, Metadata};
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};

use crate::Error;

/// Bytes of a file read to judge whether zstd pays.
pub const SAMPLE: usize = 256 << 10;
/// zstd is used only when the sample shrinks below this share, in tenths.
const SHRINK_TENTHS: usize = 9;
/// Extensions whose contents are already compressed; their files skip the sample.
const COMPRESSED: &[&str] = &[
    "7z", "aac", "apk", "avi", "avif", "br", "bz2", "dmg", "docx", "epub", "flac", "gif", "gz",
    "heic", "heif", "ipa", "jar", "jpeg", "jpg", "jxl", "key", "lz4", "lzma", "m4a", "m4v", "mkv",
    "mov", "mp3", "mp4", "numbers", "ogg", "opus", "pages", "pdf", "pkg", "png", "pptx", "rar",
    "tbz", "tgz", "txz", "webm", "webp", "whl", "woff", "woff2", "xip", "xlsx", "xz", "zip", "zst",
];

/// What an entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A regular file: bytes to fetch.
    File,
    /// A folder (a bundle or package too).
    Dir,
    /// A symlink, carried as its target.
    Symlink,
}

/// One item of a manifest, as the `xfer/<id>/manifest` stream sends it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Its place in the manifest; ranges and packs name it by this.
    pub index: u32,
    /// Its path relative to the parent of the requested path it is under, `/`-separated.
    pub rel: String,
    /// What it is.
    pub kind: Kind,
    /// Bytes; 0 unless a file.
    pub size: u64,
    /// Unix permission bits.
    pub mode: u32,
    /// Last modification, epoch milliseconds.
    pub mtime_ms: i64,
    /// A symlink's target as stored.
    #[serde(default)]
    pub target: Option<String>,
    /// copyfile's AppleDouble of its xattrs, ACL and Finder info, base64; `None` when it has
    /// none.
    #[serde(default)]
    pub meta: Option<String>,
    /// Its ranges come zstd-compressed (level 1): its sample shrank below 90%.
    #[serde(default)]
    pub zstd: bool,
}

impl Entry {
    /// The requested path it is under: the first part of [`Self::rel`].
    #[must_use]
    pub fn root(&self) -> &str {
        self.rel.split('/').next().unwrap_or_default()
    }

    /// Its AppleDouble bytes, if it has any.
    ///
    /// # Errors
    ///
    /// [`Error::Protocol`] for base64 that does not decode.
    pub fn meta_bytes(&self) -> Result<Option<Vec<u8>>, Error> {
        self.meta
            .as_deref()
            .map(|text| {
                BASE64
                    .decode(text)
                    .map_err(|e| Error::Protocol(format!("meta of {}: {e}", self.rel)))
            })
            .transpose()
    }
}

/// A manifest and, on the source, the absolute path of each entry.
#[derive(Clone, Debug, Default)]
pub struct Walked {
    /// The entries, by index.
    pub entries: Vec<Entry>,
    /// Where each entry is on the source.
    pub paths: Vec<PathBuf>,
}

/// Walks `roots` (absolute paths) into a manifest. With `sample`, files at or over `small`
/// bytes are judged for zstd by their first [`SAMPLE`] bytes: the first file of each extension
/// decides for the rest, so a tree of many files costs one sample per kind.
///
/// # Errors
///
/// [`Error::Refused`] for a root that is relative or missing; the walk's I/O errors.
pub fn walk(roots: &[PathBuf], sample: bool, small: u64) -> Result<Walked, Error> {
    let mut walker = Walker {
        out: Walked::default(),
        sample,
        small,
        verdicts: HashMap::new(),
    };
    for root in roots {
        if !root.is_absolute() {
            return Err(Error::Refused(format!(
                "{} is not absolute",
                root.display()
            )));
        }
        let meta = fs::symlink_metadata(root)
            .map_err(|e| Error::Refused(format!("{}: {e}", root.display())))?;
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| Error::Refused(format!("{} names no item", root.display())))?;
        walker.visit(root, name, &meta)?;
    }
    Ok(walker.out)
}

struct Walker {
    out: Walked,
    sample: bool,
    small: u64,
    /// Whether zstd paid for the first sampled file of each extension.
    verdicts: HashMap<String, bool>,
}

impl Walker {
    fn visit(&mut self, path: &Path, rel: String, meta: &Metadata) -> Result<(), Error> {
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            Kind::Symlink
        } else if file_type.is_dir() {
            Kind::Dir
        } else if file_type.is_file() {
            Kind::File
        } else {
            return Ok(());
        };
        let index = u32::try_from(self.out.entries.len())
            .map_err(|_| Error::Refused("too many items".into()))?;
        let target = match kind {
            Kind::Symlink => Some(fs::read_link(path)?.to_string_lossy().into_owned()),
            _ => None,
        };
        let size = if kind == Kind::File { meta.len() } else { 0 };
        let packed = crate::meta::pack(path, kind == Kind::Symlink).unwrap_or_else(|error| {
            tracing::debug!(path = %path.display(), %error, "metadata not packed");
            None
        });
        let zstd = kind == Kind::File && size >= self.small && self.compresses(path);
        self.out.entries.push(Entry {
            index,
            rel: rel.clone(),
            kind,
            size,
            mode: meta.mode() & 0o7777,
            mtime_ms: mtime_ms(meta),
            target,
            meta: packed.map(|bytes| BASE64.encode(bytes)),
            zstd,
        });
        self.out.paths.push(path.to_path_buf());
        if kind == Kind::Dir {
            let mut children = fs::read_dir(path)?
                .filter_map(Result::ok)
                .map(|child| child.file_name())
                .collect::<Vec<_>>();
            children.sort();
            for name in children {
                let child = path.join(&name);
                // A child gone since the listing is left out.
                let Ok(meta) = fs::symlink_metadata(&child) else {
                    continue;
                };
                self.visit(&child, format!("{rel}/{}", name.to_string_lossy()), &meta)?;
            }
        }
        Ok(())
    }

    fn compresses(&mut self, path: &Path) -> bool {
        if !self.sample {
            return false;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if COMPRESSED.contains(&ext.as_str()) {
            return false;
        }
        if let Some(&verdict) = self.verdicts.get(&ext) {
            return verdict;
        }
        let verdict = sample_compresses(path).unwrap_or(false);
        self.verdicts.insert(ext, verdict);
        verdict
    }
}

/// Whether zstd level 1 shrinks the first [`SAMPLE`] bytes of `path` below 90%.
///
/// # Errors
///
/// Reading the file's.
pub fn sample_compresses(path: &Path) -> io::Result<bool> {
    let mut sample = Vec::with_capacity(SAMPLE);
    fs::File::open(path)?
        .take(u64::try_from(SAMPLE).unwrap_or(u64::MAX))
        .read_to_end(&mut sample)?;
    Ok(shrinks(&sample))
}

/// Whether zstd level 1 shrinks `sample` below 90%.
#[must_use]
pub fn shrinks(sample: &[u8]) -> bool {
    if sample.is_empty() {
        return false;
    }
    zstd::bulk::compress(sample, 1).is_ok_and(|packed| {
        packed.len().saturating_mul(10) < sample.len().saturating_mul(SHRINK_TENTHS)
    })
}

/// `meta`'s modification time in epoch milliseconds.
#[must_use]
pub fn mtime_ms(meta: &Metadata) -> i64 {
    meta.mtime()
        .saturating_mul(1000)
        .saturating_add(meta.mtime_nsec() / 1_000_000)
}

/// Checks a manifest from a peer: indices in order, relative paths that stay inside the
/// destination, a target on every symlink.
///
/// # Errors
///
/// [`Error::Protocol`] naming the first bad entry.
pub fn validate(entries: &[Entry]) -> Result<(), Error> {
    for (position, entry) in entries.iter().enumerate() {
        if usize::try_from(entry.index).ok() != Some(position) {
            return Err(Error::Protocol(format!("entry {position} out of order")));
        }
        let safe = !entry.rel.is_empty()
            && !entry.rel.contains('\0')
            && entry
                .rel
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..");
        if !safe {
            return Err(Error::Protocol(format!("unsafe path {:?}", entry.rel)));
        }
        if entry.kind == Kind::Symlink && entry.target.is_none() {
            return Err(Error::Protocol(format!(
                "symlink {} has no target",
                entry.rel
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tree_walks_in_order_with_links_as_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        fs::create_dir_all(root.join("b/App.app/Contents")).unwrap();
        fs::write(root.join("a.txt"), b"hello").unwrap();
        fs::write(root.join("b/App.app/Contents/Info.plist"), b"<plist/>").unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("link")).unwrap();
        let walked = walk(&[root], true, 4).unwrap();
        let rels: Vec<_> = walked.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(
            rels,
            [
                "tree",
                "tree/a.txt",
                "tree/b",
                "tree/b/App.app",
                "tree/b/App.app/Contents",
                "tree/b/App.app/Contents/Info.plist",
                "tree/empty",
                "tree/link",
            ]
        );
        let link = &walked.entries[7];
        assert_eq!(link.kind, Kind::Symlink);
        assert_eq!(link.target.as_deref(), Some("a.txt"));
        assert_eq!(walked.entries[1].size, 5);
        validate(&walked.entries).unwrap();
    }

    #[test]
    fn a_manifest_that_climbs_out_is_refused() {
        let entry = |rel: &str| Entry {
            index: 0,
            rel: rel.into(),
            kind: Kind::File,
            size: 0,
            mode: 0o644,
            mtime_ms: 0,
            target: None,
            meta: None,
            zstd: false,
        };
        for rel in ["../x", "a/../../x", "/etc/passwd", "a//b", "", "a/./b"] {
            assert!(validate(&[entry(rel)]).is_err(), "{rel}");
        }
        validate(&[entry("a/b.txt")]).unwrap();
    }

    #[test]
    fn text_compresses_and_noise_does_not() {
        let text = b"the same line again and again\n".repeat(10_000);
        assert!(shrinks(&text));
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let noise: Vec<u8> = (0..SAMPLE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect();
        assert!(!shrinks(&noise));
    }
}
