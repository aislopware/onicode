//! Files on the clipboard: a copied file travels as [`FileEntry`] frames on a stream of its own
//! and lands in a cache folder of the offer's before the copy is written; the written URL names
//! the local copy. Only regular files of [`crate::FILES_MAX`] or less in all travel.
//!
//! The origin serves only what its newest offer names ([`crate::Clipboard::serve_file`]), so a
//! fetch can reach no other path on that Mac.

use std::fs::File;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use onicode_desktop_proto::clipboard::FileEntry;

use crate::Peers;

/// How many bytes of a file go in one frame after its entry.
pub const CHUNK: usize = 256 << 10;

/// The local path a `file://` URL names, if it is one.
#[must_use]
pub fn path_of_url(url: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(url).ok()?.trim_end_matches('\0').trim();
    let rest = text.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let path = rest.split(['?', '#']).next().unwrap_or(rest);
    String::from_utf8(percent_decode(path.as_bytes())?)
        .ok()
        .map(PathBuf::from)
}

/// The `file://` URL of `path`, percent-encoded.
#[must_use]
pub fn url_of_path(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut url = String::from("file://");
    for &byte in path.to_string_lossy().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~') {
            url.push(char::from(byte));
        } else {
            url.push('%');
            url.push(char::from(HEX[usize::from(byte >> 4)]));
            url.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    url
}

fn percent_decode(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.iter();
    while let Some(&byte) = bytes.next() {
        if byte == b'%' {
            let hi = char::from(*bytes.next()?).to_digit(16)?;
            let lo = char::from(*bytes.next()?).to_digit(16)?;
            out.push(u8::try_from(hi * 16 + lo).ok()?);
        } else {
            out.push(byte);
        }
    }
    Some(out)
}

/// The size of the regular file a URL names; `None` for a folder or anything unreadable.
pub(crate) fn file_size(url: &[u8]) -> Option<u64> {
    let meta = std::fs::metadata(path_of_url(url)?).ok()?;
    meta.is_file().then_some(meta.len())
}

/// What [`pack`] hands its sink: an entry, or the next bytes of the file it opened.
#[derive(Debug, PartialEq, Eq)]
pub enum Packed {
    /// An entry, its file's bytes (if any) following.
    Entry(FileEntry),
    /// The next bytes of the entry before.
    Bytes(Vec<u8>),
}

/// Walks the file or folder at `root` into `sink`, folders before what they hold, names in order;
/// stops early when `sink` says `false`. A file's bytes add up to the size its entry says, even
/// when it changes meanwhile. Links inside a folder are left out: they could name anything on
/// this Mac.
///
/// # Errors
///
/// `root` or a file under it could not be read, or a file shrank as it was read.
pub fn pack(root: &Path, sink: &mut dyn FnMut(Packed) -> bool) -> std::io::Result<()> {
    let name = root
        .file_name()
        .ok_or_else(|| std::io::Error::other("the copy has no name"))?
        .to_string_lossy()
        .into_owned();
    let meta = std::fs::metadata(root)?;
    let mut going = true;
    pack_one(root, &name, &meta, sink, &mut going)
}

fn pack_one(
    path: &Path,
    name: &str,
    meta: &std::fs::Metadata,
    sink: &mut dyn FnMut(Packed) -> bool,
    going: &mut bool,
) -> std::io::Result<()> {
    if !*going {
        return Ok(());
    }
    if meta.is_dir() {
        *going = sink(Packed::Entry(FileEntry {
            path: name.to_owned(),
            size: None,
            exec: false,
        }));
        let mut children: Vec<_> = std::fs::read_dir(path)?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        children.sort();
        for child in children {
            let at = path.join(&child);
            let Ok(meta) = std::fs::symlink_metadata(&at) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let named = format!("{name}/{}", child.to_string_lossy());
            pack_one(&at, &named, &meta, sink, going)?;
        }
        return Ok(());
    }
    if !meta.is_file() {
        return Ok(());
    }
    let size = meta.len();
    *going = sink(Packed::Entry(FileEntry {
        path: name.to_owned(),
        size: Some(size),
        exec: executable(meta),
    }));
    let mut file = File::open(path)?.take(size);
    let mut left = size;
    while *going && left > 0 {
        let mut chunk = vec![0; usize::try_from(left).unwrap_or(CHUNK).min(CHUNK)];
        let read = file.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::other(format!(
                "{} shrank as it was read",
                path.display()
            )));
        }
        chunk.truncate(read);
        left = left.saturating_sub(u64::try_from(read).unwrap_or(u64::MAX));
        *going = sink(Packed::Bytes(chunk));
    }
    Ok(())
}

fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

/// Where `entry` lands under `into`, if its path is a plain relative one under `root` (the
/// copy's own name, which the first entry sets): no `..`, no root, no prefix.
#[must_use]
pub fn landing(into: &Path, root: Option<&str>, entry: &FileEntry) -> Option<PathBuf> {
    let path = Path::new(&entry.path);
    let mut parts = path.components();
    let first = match parts.next()? {
        Component::Normal(first) => first.to_str()?,
        _ => return None,
    };
    if root.is_some_and(|root| root != first) {
        return None;
    }
    if !parts.all(|part| matches!(part, Component::Normal(_))) {
        return None;
    }
    Some(into.join(path))
}

/// Fetches the file each of `items` of `origin`'s offer `seq` names into `dir`, one folder per
/// item (`<dir>/<item>/`, so two files of one name from two folders both land); each item's local
/// file URL.
///
/// # Errors
///
/// The origin is gone, no longer holds the offer, or a copy failed.
pub(crate) async fn fetch(
    peers: &dyn Peers,
    origin: &str,
    seq: u64,
    dir: &Path,
    items: &[usize],
) -> Result<Vec<(usize, Vec<u8>)>, String> {
    let mut urls = Vec::with_capacity(items.len());
    for &item in items {
        let into = dir.join(item.to_string());
        tokio::fs::create_dir_all(&into)
            .await
            .map_err(|e| format!("{}: {e}", into.display()))?;
        let landed = peers.fetch_file(origin, seq, item, &into).await?;
        urls.push((item, url_of_path(&landed).into_bytes()));
    }
    Ok(urls)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn urls_name_paths_and_back() {
        let path = Path::new("/Users/me/A file #1 (é).txt");
        let url = url_of_path(path);
        assert!(url.starts_with("file:///Users/me/A%20file%20%231"), "{url}");
        assert_eq!(path_of_url(url.as_bytes()).as_deref(), Some(path));
        assert_eq!(
            path_of_url(b"file://localhost/tmp/x%20y/").as_deref(),
            Some(Path::new("/tmp/x y/"))
        );
        assert_eq!(path_of_url(b"https://example.com/a"), None);
        assert_eq!(path_of_url(b"file://host/a"), None, "another host's path");
        assert_eq!(path_of_url(b"file:///a%2"), None, "a cut escape");
    }

    #[test]
    fn a_folder_packs_in_order_and_lands_only_under_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("notes");
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("a.txt"), b"one").unwrap();
        std::fs::write(root.join("b/c.txt"), vec![7; CHUNK + 1]).unwrap();
        let mut packed = Vec::new();
        pack(&root, &mut |piece| {
            packed.push(piece);
            true
        })
        .unwrap();
        let entries: Vec<(String, Option<u64>)> = packed
            .iter()
            .filter_map(|piece| match piece {
                Packed::Entry(entry) => Some((entry.path.clone(), entry.size)),
                Packed::Bytes(_) => None,
            })
            .collect();
        assert_eq!(
            entries,
            [
                ("notes".to_owned(), None),
                ("notes/a.txt".to_owned(), Some(3)),
                ("notes/b".to_owned(), None),
                ("notes/b/c.txt".to_owned(), Some(CHUNK as u64 + 1)),
            ]
        );
        let bytes: usize = packed
            .iter()
            .map(|piece| match piece {
                Packed::Bytes(bytes) => bytes.len(),
                Packed::Entry(_) => 0,
            })
            .sum();
        assert_eq!(bytes, 3 + CHUNK + 1);

        let into = Path::new("/cache/0");
        let entry = |path: &str| FileEntry {
            path: path.into(),
            size: None,
            exec: false,
        };
        assert_eq!(
            landing(into, Some("notes"), &entry("notes/b/c.txt")),
            Some(into.join("notes/b/c.txt"))
        );
        for bad in ["/etc/passwd", "notes/../../x", "other/x", "", "./notes"] {
            assert_eq!(landing(into, Some("notes"), &entry(bad)), None, "{bad}");
        }
    }

    proptest! {
        #[test]
        fn every_path_survives_its_url(name in "[^\u{0}/]{1,40}") {
            let path = Path::new("/tmp").join(&name);
            prop_assert_eq!(path_of_url(url_of_path(&path).as_bytes()), Some(path));
        }

        #[test]
        fn reading_a_url_never_panics(url in proptest::collection::vec(any::<u8>(), 0..64)) {
            let _ = path_of_url(&url);
        }
    }
}
