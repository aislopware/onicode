//! The source side: the transfers this engine serves, and the handler for file streams.
//!
//! A destination registers a transfer here with `ServeTransfer` before it opens a stream; a
//! stream naming a transfer that was never registered (or was forgotten) is reset with
//! [`reset::UNKNOWN`], so no path is ever read for a stream alone. `clip/...` heads go to the
//! clipboard's [`ClipStreams`], once one is set.

use std::collections::HashMap;
use std::fs::File;
use std::os::unix::fs::FileExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use bytes::{BufMut as _, Bytes, BytesMut};
use onicode_net::{StreamHead, StreamKind, StreamRx, StreamTx};
use onicode_peer::StreamHandler;

use crate::job::lock;
use crate::manifest::{self, Entry, Kind, Walked};
use crate::wire::{self, CHUNK, CLIP, Out, Request, XFER};
use crate::{Error, blocking, reset};

/// Bytes a stream may queue ahead of the link before its writer waits.
const WINDOW: usize = 1 << 20;
/// A registration unused this long is forgotten.
const FORGET_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
/// zstd level 1 runs at about this many bytes a second on one core; a link faster than this
/// is better fed raw.
pub const COMPRESSOR_RATE: u64 = 500_000_000;

/// The clipboard's side of file streams: every `clip/...` head
/// (`clip/<originSeq>/<type>`, or `clip/<originSeq>/<item>/<type>`).
pub trait ClipStreams: Send + Sync + 'static {
    /// A `clip/...` stream `device` opened; `head` is the whole head name, `clip/` included.
    /// Called on the link's task: spawn to do work.
    fn serve_stream(&self, device: &str, head: &str, tx: StreamTx, rx: StreamRx);
}

/// A peer's link bandwidth in bytes a second, if measured.
pub type Bandwidth = Arc<dyn Fn(&str) -> Option<u64> + Send + Sync>;

struct Registration {
    paths: Vec<PathBuf>,
    block: u64,
    /// The manifest last sent, which ranges and packs index into.
    walked: tokio::sync::Mutex<Option<Arc<Walked>>>,
    used: Mutex<Instant>,
}

/// The transfers this engine serves as a source.
#[derive(Default)]
pub struct Served {
    transfers: Mutex<HashMap<String, Arc<Registration>>>,
    bandwidth: RwLock<Option<Bandwidth>>,
}

impl Served {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Measures links with `bandwidth`: zstd is skipped for a link faster than the compressor.
    pub fn set_bandwidth(&self, bandwidth: Bandwidth) {
        *self
            .bandwidth
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(bandwidth);
    }

    /// Serves `paths` (absolute) as `transfer` from now on, in blocks of `block` bytes.
    /// Registering again replaces the paths and forgets the last manifest.
    ///
    /// # Errors
    ///
    /// [`Error::Refused`] for a relative path, no paths, or a block under 1 KiB.
    pub fn register(&self, transfer: &str, paths: Vec<PathBuf>, block: u64) -> Result<(), Error> {
        if transfer.is_empty() || transfer.contains('/') {
            return Err(Error::Refused("a transfer id names no transfer".into()));
        }
        if paths.is_empty() || paths.iter().any(|p| !p.is_absolute()) {
            return Err(Error::Refused("paths must be absolute".into()));
        }
        if block < 1024 {
            return Err(Error::Refused(format!("a block of {block} bytes")));
        }
        let now = Instant::now();
        let mut transfers = lock(&self.transfers);
        transfers.retain(|_, r| now.duration_since(*lock(&r.used)) < FORGET_AFTER);
        transfers.insert(
            transfer.to_owned(),
            Arc::new(Registration {
                paths,
                block,
                walked: tokio::sync::Mutex::new(None),
                used: Mutex::new(now),
            }),
        );
        Ok(())
    }

    /// Registers what a `ServeTransfer` call asks for.
    ///
    /// # Errors
    ///
    /// As [`Self::register`].
    pub fn register_request(
        &self,
        request: onicode_proto::files::ServeTransferRequest,
    ) -> Result<(), Error> {
        let paths = request.paths.iter().map(PathBuf::from).collect();
        self.register(
            &request.transfer_id,
            paths,
            request.block.unwrap_or(crate::BLOCK),
        )
    }

    /// Stops serving `transfer`.
    pub fn forget(&self, transfer: &str) {
        lock(&self.transfers).remove(transfer);
    }

    /// Whether `transfer` is served.
    #[must_use]
    pub fn serves(&self, transfer: &str) -> bool {
        lock(&self.transfers).contains_key(transfer)
    }

    /// Answers the stream headed `name` that `device` opened, into `out`.
    ///
    /// # Errors
    ///
    /// [`Error::Unknown`] for a transfer not registered; [`Error::Refused`] for a head that
    /// asks for something not served; [`Error::Changed`] when an item changed since the
    /// manifest; the stream's.
    pub async fn serve(&self, device: &str, name: &str, out: &mut dyn Out) -> Result<(), Error> {
        let (transfer, request) = wire::parse(name)?;
        let registration = lock(&self.transfers)
            .get(transfer)
            .cloned()
            .ok_or(Error::Unknown)?;
        *lock(&registration.used) = Instant::now();
        match request {
            Request::Manifest => {
                let fast = self
                    .bandwidth
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                    .and_then(|measure| measure(device))
                    .is_some_and(|rate| rate > COMPRESSOR_RATE);
                let paths = registration.paths.clone();
                let small = crate::SMALL.min(registration.block);
                let walked =
                    Arc::new(blocking(move || manifest::walk(&paths, !fast, small)).await?);
                let json = serde_json::to_vec(&walked.entries)
                    .map_err(|e| Error::Protocol(e.to_string()))?;
                *registration.walked.lock().await = Some(walked);
                let len = u32::try_from(json.len())
                    .map_err(|_| Error::Refused("the manifest is too large".into()))?;
                let mut framed = BytesMut::with_capacity(json.len().saturating_add(4));
                framed.put_u32(len);
                framed.put_slice(&json);
                out.send(framed.freeze()).await
            }
            Request::Range { item, offset, len } => {
                let walked = Self::walked(&registration).await?;
                let (entry, path) = item_of(&walked, item)?;
                send_range(entry, path, offset, len, registration.block, out).await
            }
            Request::Pack { first, count } => {
                let walked = Self::walked(&registration).await?;
                send_pack(&walked, first, count, out).await
            }
        }
    }

    /// The manifest last sent. A source that has sent none (it restarted) makes the puller
    /// register again, which fetches the manifest first.
    async fn walked(registration: &Registration) -> Result<Arc<Walked>, Error> {
        registration
            .walked
            .lock()
            .await
            .clone()
            .ok_or(Error::Unknown)
    }
}

fn item_of(walked: &Walked, item: u32) -> Result<(&Entry, &Path), Error> {
    let at = usize::try_from(item).map_err(|_| Error::Refused("no such item".into()))?;
    match (walked.entries.get(at), walked.paths.get(at)) {
        (Some(entry), Some(path)) => Ok((entry, path.as_path())),
        _ => Err(Error::Refused(format!("no item {item}"))),
    }
}

/// Opens `path` as the file `entry` describes, or says it changed.
fn open_unchanged(entry: &Entry, path: &Path) -> Result<File, Error> {
    let file = File::open(path).map_err(|_| Error::Changed)?;
    let meta = file.metadata().map_err(|_| Error::Changed)?;
    if !meta.is_file() || meta.len() != entry.size || manifest::mtime_ms(&meta) != entry.mtime_ms {
        return Err(Error::Changed);
    }
    Ok(file)
}

async fn send_range(
    entry: &Entry,
    path: &Path,
    offset: u64,
    len: u64,
    block: u64,
    out: &mut dyn Out,
) -> Result<(), Error> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| Error::Refused("range overflows".into()))?;
    let aligned = offset.is_multiple_of(block) && (end.is_multiple_of(block) || end == entry.size);
    if entry.kind != Kind::File || end > entry.size || !aligned {
        return Err(Error::Refused(format!(
            "range {offset}+{len} of {}",
            entry.rel
        )));
    }
    let file = {
        let (entry, path) = (entry.clone(), path.to_path_buf());
        Arc::new(blocking(move || open_unchanged(&entry, &path)).await?)
    };
    let compress = entry.zstd;
    for (start, stop) in wire::blocks(offset, len, block) {
        let mut hasher = blake3::Hasher::new();
        let mut at = start;
        while at < stop {
            let n = usize::try_from((stop - at).min(CHUNK as u64)).unwrap_or(CHUNK);
            let file = Arc::clone(&file);
            let (data, back) = blocking(move || {
                let mut buf = vec![0_u8; n];
                file.read_exact_at(&mut buf, at)
                    .map_err(|_| Error::Changed)?;
                hasher.update(&buf);
                let data = if compress {
                    let packed = zstd::bulk::compress(&buf, 1)?;
                    let len = u32::try_from(packed.len())
                        .map_err(|_| Error::Protocol("a frame past 4 GiB".into()))?;
                    let mut framed = BytesMut::with_capacity(packed.len().saturating_add(4));
                    framed.put_u32(len);
                    framed.put_slice(&packed);
                    framed.freeze()
                } else {
                    Bytes::from(buf)
                };
                Ok((data, hasher))
            })
            .await?;
            hasher = back;
            out.send(data).await?;
            at = at.saturating_add(n as u64);
        }
        out.send(Bytes::copy_from_slice(hasher.finalize().as_bytes()))
            .await?;
    }
    Ok(())
}

async fn send_pack(
    walked: &Walked,
    first: u32,
    count: u32,
    out: &mut dyn Out,
) -> Result<(), Error> {
    let last = first
        .checked_add(count)
        .ok_or_else(|| Error::Refused("pack overflows".into()))?;
    let mut items = Vec::new();
    for item in first..last {
        let (entry, path) = item_of(walked, item)?;
        if entry.kind == Kind::File && entry.size > wire::MAX_PACKED {
            return Err(Error::Refused(format!("{} is too big to pack", entry.rel)));
        }
        items.push((entry.clone(), path.to_path_buf()));
    }
    let bodies = blocking(move || {
        items
            .into_iter()
            .map(|(entry, path)| {
                if entry.kind != Kind::File {
                    return Ok(Vec::new());
                }
                let file = open_unchanged(&entry, &path)?;
                let mut buf = vec![0_u8; usize::try_from(entry.size).unwrap_or(0)];
                file.read_exact_at(&mut buf, 0)
                    .map_err(|_| Error::Changed)?;
                Ok(buf)
            })
            .collect::<Result<Vec<_>, Error>>()
    })
    .await?;
    for body in bodies {
        let mut framed = BytesMut::with_capacity(body.len().saturating_add(8 + wire::DIGEST));
        framed.put_u64(body.len() as u64);
        framed.put_slice(&body);
        framed.put_slice(blake3::hash(&body).as_bytes());
        out.send(framed.freeze()).await?;
    }
    Ok(())
}

/// A stream's sending half as an [`Out`] that waits while the link is behind.
struct StreamOut(StreamTx);

#[async_trait::async_trait]
impl Out for StreamOut {
    async fn send(&mut self, data: Bytes) -> Result<(), Error> {
        self.0.room(WINDOW).await;
        if self.0.is_closed() {
            return Err(Error::Stopped);
        }
        self.0.send(data);
        Ok(())
    }
}

/// The handler for `StreamKind::File`: transfers by `xfer/`, the clipboard by `clip/`.
pub struct FileStreams {
    served: Arc<Served>,
    clip: RwLock<Option<Arc<dyn ClipStreams>>>,
}

impl FileStreams {
    /// Serves the transfers registered with `served`.
    #[must_use]
    pub fn new(served: Arc<Served>) -> Self {
        Self {
            served,
            clip: RwLock::new(None),
        }
    }

    /// The registry it serves.
    #[must_use]
    pub const fn served(&self) -> &Arc<Served> {
        &self.served
    }

    /// Hands `clip/...` streams to `clip` from now on.
    pub fn set_clip(&self, clip: Arc<dyn ClipStreams>) {
        *self
            .clip
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(clip);
    }
}

impl StreamHandler for FileStreams {
    fn stream(&self, device: &str, head: StreamHead, tx: StreamTx, rx: StreamRx) {
        if head.name.starts_with(XFER) {
            tx.set_priority(StreamKind::File.priority());
            let served = Arc::clone(&self.served);
            let device = device.to_owned();
            tokio::spawn(async move {
                let _rx = rx;
                let mut out = StreamOut(tx);
                match served.serve(&device, &head.name, &mut out).await {
                    Ok(()) => out.0.finish(),
                    Err(error) => {
                        tracing::debug!(%device, head = %head.name, %error, "file stream refused");
                        out.0.reset(error.reset_code());
                    }
                }
            });
        } else if head.name.starts_with(CLIP) {
            let clip = self
                .clip
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            match clip {
                Some(clip) => clip.serve_stream(device, &head.name, tx, rx),
                None => tx.reset(reset::UNKNOWN),
            }
        } else {
            tx.reset(reset::BAD);
        }
    }
}
