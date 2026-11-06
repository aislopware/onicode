//! Files on the clipboard: a copied file travels as the origin's path and lands through the
//! transfer engine in a cache folder of its own; the pasted URL names the local copy.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::Peers;
use crate::types::FILE_URL_TYPE;

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

/// The files of one received offer, fetched once for every item that pastes them.
pub(crate) struct Files {
    pub(crate) peers: Arc<dyn Peers>,
    pub(crate) origin: String,
    pub(crate) seq: u64,
    /// Where they land: `<cache>/<origin>-<seq>`.
    pub(crate) dir: PathBuf,
    /// Per offer item: `None` without a file; `Some(None)` for a file whose URL is fetched
    /// first (the origin could not read it unasked); `Some(Some(url))` with the origin's URL.
    pub(crate) sources: Vec<Option<Option<Vec<u8>>>>,
    /// The local URL of each item's file, once fetched. A failed fetch is not kept: the next
    /// paste tries again.
    pub(crate) done: tokio::sync::OnceCell<Vec<Option<Vec<u8>>>>,
}

impl Files {
    /// The local file URL of `item`'s file, fetching every file of the offer the first time.
    pub(crate) async fn url(self: Arc<Self>, item: usize) -> Option<Vec<u8>> {
        let urls = match self.done.get_or_try_init(|| self.fetch()).await {
            Ok(urls) => urls,
            Err(error) => {
                tracing::debug!(origin = %self.origin, seq = self.seq, %error, "clipboard files not fetched");
                return None;
            }
        };
        urls.get(item)?.clone()
    }

    async fn fetch(&self) -> Result<Vec<Option<Vec<u8>>>, String> {
        let mut found: Vec<(usize, PathBuf, OsString)> = Vec::new();
        for (item, source) in self.sources.iter().enumerate() {
            let Some(inline) = source else { continue };
            let url = match inline {
                Some(url) => url.clone(),
                None => {
                    self.peers
                        .fetch(&self.origin, self.seq, item, FILE_URL_TYPE)
                        .await?
                }
            };
            let path = path_of_url(&url).ok_or_else(|| "not a file URL".to_owned())?;
            let name = path
                .file_name()
                .ok_or_else(|| format!("{} has no name", path.display()))?
                .to_owned();
            found.push((item, path, name));
        }
        let mut urls = vec![None; self.sources.len()];
        let names: HashSet<&OsString> = found.iter().map(|(_, _, name)| name).collect();
        // One transfer when the names are all different; else one folder per item, so two
        // files of the same name from two folders both land.
        type Found = (usize, PathBuf, OsString);
        let groups: Vec<(PathBuf, Vec<&Found>)> = if names.len() == found.len() {
            vec![(self.dir.clone(), found.iter().collect())]
        } else {
            found
                .iter()
                .map(|entry| (self.dir.join(entry.0.to_string()), vec![entry]))
                .collect()
        };
        for (into, entries) in groups {
            tokio::fs::create_dir_all(&into)
                .await
                .map_err(|e| format!("{}: {e}", into.display()))?;
            let paths: Vec<String> = entries
                .iter()
                .map(|(_, path, _)| path.to_string_lossy().into_owned())
                .collect();
            self.peers.fetch_files(&self.origin, &paths, &into).await?;
            for (item, _, name) in entries {
                if let Some(slot) = urls.get_mut(*item) {
                    *slot = Some(url_of_path(&into.join(name)).into_bytes());
                }
            }
        }
        Ok(urls)
    }
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
