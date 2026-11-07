//! Files on the clipboard: a copied file travels as a [`FileEntry`] and its bytes on a stream of
//! its own and lands in a cache folder of the offer's before the copy is written; the written URL
//! names the local copy. Only regular files of [`crate::FILES_MAX`] or less in all travel.
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

/// The head of the regular file at `path`, for a fetch.
///
/// # Errors
///
/// `path` is not a regular file, has no name, or could not be read.
pub fn entry_of(path: &Path) -> std::io::Result<FileEntry> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(std::io::Error::other("only a regular file travels"));
    }
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("the copy has no name"))?
        .to_string_lossy()
        .into_owned();
    Ok(FileEntry {
        name,
        size: meta.len(),
        exec: executable(&meta),
    })
}

/// Reads the first `size` bytes of the file at `path` into `sink`, [`CHUNK`] at a time; stops
/// early when `sink` says `false`. The bytes add up to `size` even when the file grows meanwhile.
///
/// # Errors
///
/// The file could not be read, or it shrank as it was read.
pub fn read_chunks(
    path: &Path,
    size: u64,
    sink: &mut dyn FnMut(Vec<u8>) -> bool,
) -> std::io::Result<()> {
    let mut file = File::open(path)?.take(size);
    let mut left = size;
    while left > 0 {
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
        if !sink(chunk) {
            break;
        }
    }
    Ok(())
}

fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o111 != 0
}

/// Where `entry` lands under `into`, if its name is one plain path component: no `/`, no `..`,
/// no root.
#[must_use]
pub fn landing(into: &Path, entry: &FileEntry) -> Option<PathBuf> {
    let mut parts = Path::new(&entry.name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(name)), None) => Some(into.join(name)),
        _ => None,
    }
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
    fn a_file_reads_whole_and_lands_only_under_its_cache() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, vec![7; CHUNK + 1]).unwrap();
        let entry = entry_of(&file).unwrap();
        assert_eq!(
            (entry.name.as_str(), entry.size),
            ("notes.txt", CHUNK as u64 + 1)
        );
        let mut chunks = Vec::new();
        read_chunks(&file, entry.size, &mut |chunk| {
            chunks.push(chunk.len());
            true
        })
        .unwrap();
        assert_eq!(chunks, [CHUNK, 1]);
        assert!(entry_of(dir.path()).is_err(), "a folder never travels");

        let into = Path::new("/cache/0");
        let named = |name: &str| FileEntry {
            name: name.into(),
            size: 0,
            exec: false,
        };
        assert_eq!(landing(into, &named("a.txt")), Some(into.join("a.txt")));
        for bad in ["/etc/passwd", "../x", "a/b", "", ".", ".."] {
            assert_eq!(landing(into, &named(bad)), None, "{bad}");
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
