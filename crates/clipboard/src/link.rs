//! The clipboard on a desktop link: [`LinkLine`] fetches from the other end on file streams of
//! their own, and [`serve_streams`] answers the other end's fetches.
//!
//! A fetch opens a [`StreamKind::File`] stream named by [`Fetch::name`] and sends nothing after
//! its head. The origin answers a type with its bytes, raw, and a file or folder with
//! [`FileEntry`] frames, each entry a JSON frame and a file's bytes in frames after it; then it
//! finishes the stream. A refused fetch is a reset ([`REFUSED`]). These streams never touch the
//! control stream, so a big paste holds up no input or cursor, and they are sent at the file
//! kind's priority, behind the picture's datagrams.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use onicode_desktop_proto::clipboard::{Clip, Fetch, FileEntry, REFUSED};
use onicode_net::stream::frame;
use onicode_net::{Link, StreamHead, StreamKind, StreamRx, StreamTx};
use tokio::io::AsyncWriteExt as _;

use crate::files::{CHUNK, Packed, landing, pack};
use crate::session::{Line, Served, Session};
use crate::sync::FETCH_MAX;

/// How long a stream the other end opens has to say what it is.
const HEAD_WITHIN: Duration = Duration::from_secs(10);
/// How much a served stream lets queue on the link before it waits.
const WINDOW: usize = 1 << 20;
/// The longest frame of a files stream: an entry, or [`CHUNK`] bytes and their length.
const FILES_FRAME: usize = CHUNK + 4096;

/// Sends a clip on the session's control stream, in the envelope of the end that holds it.
pub type Say = Box<dyn Fn(Clip) -> bool + Send + Sync>;

/// A desktop link as the clipboard reaches the other end.
pub struct LinkLine {
    link: Link,
    say: Say,
}

impl std::fmt::Debug for LinkLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkLine")
            .field("link", &self.link.id())
            .finish_non_exhaustive()
    }
}

impl LinkLine {
    /// The clipboard over `link`, its clips sent by `say`.
    #[must_use]
    pub fn new(link: Link, say: Say) -> Arc<Self> {
        Arc::new(Self { link, say })
    }

    /// Opens the fetch `fetch`; the stream to read the answer from.
    async fn open(&self, fetch: &Fetch) -> Result<StreamRx, String> {
        let (tx, rx) = self
            .link
            .open(&StreamHead::new(StreamKind::File, fetch.name()))
            .await
            .map_err(|e| e.to_string())?;
        // Nothing follows the head: the send side finishes once it is written.
        tx.finish();
        Ok(rx)
    }
}

#[async_trait]
impl Line for LinkLine {
    fn say(&self, clip: Clip) -> bool {
        (self.say)(clip)
    }

    async fn fetch(&self, fetch: Fetch) -> Result<Vec<u8>, String> {
        let mut rx = self.open(&fetch).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = rx.chunk().await.map_err(|e| e.to_string())? {
            if bytes.len().saturating_add(chunk.len()) > FETCH_MAX {
                rx.stop(REFUSED);
                return Err("the copy is too big to paste".to_owned());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn fetch_file(&self, fetch: Fetch, into: &Path) -> Result<PathBuf, String> {
        let mut rx = self.open(&fetch).await?;
        rx.set_max_frame(FILES_FRAME);
        unpack(&mut rx, into).await
    }
}

/// Reads [`FileEntry`] frames off `rx` into `into`; the local path of the copy's top entry.
async fn unpack(rx: &mut StreamRx, into: &Path) -> Result<PathBuf, String> {
    let mut root: Option<String> = None;
    while let Some(head) = rx.frame().await.map_err(|e| e.to_string())? {
        let entry: FileEntry = serde_json::from_slice(&head).map_err(|e| e.to_string())?;
        let at = landing(into, root.as_deref(), &entry)
            .ok_or_else(|| format!("{} would land outside the copy", entry.path))?;
        if root.is_none() {
            root = entry.path.split('/').next().map(str::to_owned);
        }
        let Some(size) = entry.size else {
            tokio::fs::create_dir_all(&at)
                .await
                .map_err(|e| format!("{}: {e}", at.display()))?;
            continue;
        };
        let mut file = tokio::fs::File::create(&at)
            .await
            .map_err(|e| format!("{}: {e}", at.display()))?;
        let mut left = size;
        while left > 0 {
            let chunk = rx
                .frame()
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("{} ended early", entry.path))?;
            let len = u64::try_from(chunk.len()).unwrap_or(u64::MAX);
            if len > left {
                return Err(format!("{} is longer than it said", entry.path));
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| format!("{}: {e}", at.display()))?;
            left -= len;
        }
        file.flush()
            .await
            .map_err(|e| format!("{}: {e}", at.display()))?;
        if entry.exec {
            use std::os::unix::fs::PermissionsExt as _;
            let perms = std::fs::Permissions::from_mode(0o755);
            tokio::fs::set_permissions(&at, perms)
                .await
                .map_err(|e| format!("{}: {e}", at.display()))?;
        }
    }
    root.map(|root| into.join(root))
        .ok_or_else(|| "the copy held nothing".to_owned())
}

/// Answers every fetch the other end opens on `link` from `session`, each stream on a task of
/// its own, until the link closes or the session is gone.
pub async fn serve_streams(link: Link, session: Weak<Session>) {
    while let Ok((tx, rx)) = link.accept().await {
        if session.strong_count() == 0 {
            tx.reset(REFUSED);
            return;
        }
        drop(tokio::spawn(serve_stream(tx, rx, session.clone())));
    }
}

async fn serve_stream(tx: StreamTx, mut rx: StreamRx, session: Weak<Session>) {
    let head = match tokio::time::timeout(HEAD_WITHIN, rx.head()).await {
        Ok(Ok(head)) => head,
        _silent => return tx.reset(REFUSED),
    };
    let fetch = if head.kind == StreamKind::File {
        Fetch::parse(&head.name)
    } else {
        None
    };
    let Some(fetch) = fetch else {
        return tx.reset(REFUSED);
    };
    // An accepted stream starts at the top priority; a paste's bytes go behind the picture.
    tx.set_priority(StreamKind::File.priority());
    let served = tokio::task::spawn_blocking(move || session.upgrade()?.serve(&fetch))
        .await
        .ok()
        .flatten();
    match served {
        Some(Served::Bytes(bytes)) => {
            let bytes = Bytes::from(bytes);
            let mut at = 0;
            while at < bytes.len() {
                let end = at.saturating_add(CHUNK).min(bytes.len());
                tx.room(WINDOW).await;
                tx.send(bytes.slice(at..end));
                at = end;
            }
            tx.finish();
        }
        Some(Served::Path(path)) => serve_tree(tx, path).await,
        None => tx.reset(REFUSED),
    }
}

/// Sends the file or folder at `path` as entries, read on a blocking thread that waits for
/// room on the link between frames.
async fn serve_tree(tx: StreamTx, path: PathBuf) {
    let runtime = tokio::runtime::Handle::current();
    let sent = tokio::task::spawn_blocking(move || {
        let packed = {
            let mut sink = |piece: Packed| {
                let payload = match piece {
                    Packed::Entry(entry) => serde_json::to_vec(&entry).unwrap_or_default(),
                    Packed::Bytes(bytes) => bytes,
                };
                let Ok(framed) = frame(&payload) else {
                    return false;
                };
                runtime.block_on(tx.room(WINDOW));
                if tx.is_closed() {
                    return false;
                }
                tx.send(framed);
                true
            };
            pack(&path, &mut sink)
        };
        (tx, packed)
    })
    .await;
    match sent {
        Ok((tx, Ok(()))) => tx.finish(),
        Ok((tx, Err(error))) => {
            tracing::debug!(%error, "a copied file not sent");
            tx.reset(REFUSED);
        }
        Err(_panicked) => {}
    }
}
