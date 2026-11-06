//! Where a transfer reads: the source engine over a file stream on the peer link, or this
//! engine itself for a copy that never leaves the machine. Both speak the same wire, so a
//! local copy runs the very code a remote one does.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use onicode_net::{StreamHead, StreamKind, StreamRx, StreamTx, close};
use onicode_peer::PeerLinks;
use onicode_proto::files::{ServeTransferRequest, TransferFrom};
use onicode_rpc::methods;
use tokio::sync::mpsc;

use crate::manifest::{self, Entry};
use crate::serve::Served;
use crate::wire::{self, Out, Reader, Request, Wire};
use crate::{Error, Tuning};

/// How long a stream may bring nothing before it is taken for dead.
pub const STALL: Duration = Duration::from_secs(30);

/// Streams of one transfer's source.
#[async_trait]
pub trait Source: Send + Sync {
    /// A stream answering `request`.
    ///
    /// # Errors
    ///
    /// When the source cannot be reached.
    async fn open(&self, request: Request) -> Result<Box<dyn Wire>, Error>;
}

/// The manifest `source` sends.
///
/// # Errors
///
/// The stream's; [`Error::Protocol`] for a manifest that does not parse or check out.
pub async fn fetch_manifest(source: &dyn Source) -> Result<Vec<Entry>, Error> {
    let mut reader = Reader::new(source.open(Request::Manifest).await?);
    let len = usize::try_from(reader.u32().await?).unwrap_or(usize::MAX);
    if len > wire::MAX_MANIFEST {
        return Err(Error::Protocol(format!("a manifest of {len} bytes")));
    }
    let json = reader.exact(len).await?;
    let entries: Vec<Entry> =
        serde_json::from_slice(&json).map_err(|e| Error::Protocol(format!("manifest: {e}")))?;
    manifest::validate(&entries)?;
    Ok(entries)
}

/// This engine's own files, through its [`Served`] registry, as a peer would read them.
pub struct LocalSource {
    served: Arc<Served>,
    transfer: String,
    device: String,
}

impl LocalSource {
    /// `transfer` as registered with `served`, read as `device`.
    #[must_use]
    pub const fn new(served: Arc<Served>, transfer: String, device: String) -> Self {
        Self {
            served,
            transfer,
            device,
        }
    }
}

struct ChannelOut(mpsc::Sender<Result<Bytes, Error>>);

#[async_trait]
impl Out for ChannelOut {
    async fn send(&mut self, data: Bytes) -> Result<(), Error> {
        self.0.send(Ok(data)).await.map_err(|_| Error::Stopped)
    }
}

struct ChannelWire(mpsc::Receiver<Result<Bytes, Error>>);

#[async_trait]
impl Wire for ChannelWire {
    async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        self.0.recv().await.transpose()
    }
}

#[async_trait]
impl Source for LocalSource {
    async fn open(&self, request: Request) -> Result<Box<dyn Wire>, Error> {
        let (tx, rx) = mpsc::channel(8);
        let served = Arc::clone(&self.served);
        let name = wire::head(&self.transfer, &request);
        let device = self.device.clone();
        tokio::spawn(async move {
            let mut out = ChannelOut(tx.clone());
            if let Err(error) = served.serve(&device, &name, &mut out).await {
                let _ = tx.send(Err(error)).await;
            }
        });
        Ok(Box::new(ChannelWire(rx)))
    }
}

/// A transfer's source on another device, over file streams on the link to it.
pub struct LinkSource {
    links: Arc<PeerLinks>,
    device: String,
    transfer: String,
}

impl LinkSource {
    /// `transfer` on `device`, over `links`.
    #[must_use]
    pub const fn new(links: Arc<PeerLinks>, device: String, transfer: String) -> Self {
        Self {
            links,
            device,
            transfer,
        }
    }
}

struct LinkWire {
    _tx: StreamTx,
    rx: StreamRx,
    ended: bool,
}

#[async_trait]
impl Wire for LinkWire {
    async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        match tokio::time::timeout(STALL, self.rx.chunk()).await {
            Ok(Ok(Some(chunk))) => Ok(Some(chunk)),
            Ok(Ok(None)) => {
                self.ended = true;
                Ok(None)
            }
            Ok(Err(error)) => {
                self.ended = true;
                Err(error.into())
            }
            Err(_) => Err(Error::Link("the stream stalled".into())),
        }
    }
}

impl Drop for LinkWire {
    fn drop(&mut self) {
        if !self.ended {
            self.rx.stop(close::NORMAL);
        }
    }
}

#[async_trait]
impl Source for LinkSource {
    async fn open(&self, request: Request) -> Result<Box<dyn Wire>, Error> {
        let head = StreamHead::new(StreamKind::File, wire::head(&self.transfer, &request));
        let (tx, rx) = self.links.open(&self.device, &head).await?;
        Ok(Box::new(LinkWire {
            _tx: tx,
            rx,
            ended: false,
        }))
    }
}

/// Finds a job's source and registers the job with it.
#[async_trait]
pub trait Origins: Send + Sync + 'static {
    /// Registers `transfer` for `from` with the device holding it, and returns how to read it.
    ///
    /// # Errors
    ///
    /// [`Error::Link`] when the device cannot be reached now; [`Error::Refused`] when it says
    /// no.
    async fn connect(
        &self,
        transfer: &str,
        from: &TransferFrom,
        tuning: &Tuning,
    ) -> Result<Arc<dyn Source>, Error>;
}

/// The peer links, once the engine is on the tailnet.
pub type LinksSlot = Arc<RwLock<Option<Arc<PeerLinks>>>>;

/// Sources as an engine finds them: itself through its own [`Served`], any other device over
/// its link, registered there with `ServeTransfer`.
pub struct PeerOrigins {
    device: String,
    served: Arc<Served>,
    links: LinksSlot,
}

impl PeerOrigins {
    /// The origins of the engine `device`, serving through `served`, reaching peers by `links`.
    #[must_use]
    pub const fn new(device: String, served: Arc<Served>, links: LinksSlot) -> Self {
        Self {
            device,
            served,
            links,
        }
    }
}

#[async_trait]
impl Origins for PeerOrigins {
    async fn connect(
        &self,
        transfer: &str,
        from: &TransferFrom,
        tuning: &Tuning,
    ) -> Result<Arc<dyn Source>, Error> {
        if from.device_id.is_empty() || from.device_id == self.device {
            let paths = from.paths.iter().map(PathBuf::from).collect();
            self.served.register(transfer, paths, tuning.block)?;
            return Ok(Arc::new(LocalSource::new(
                Arc::clone(&self.served),
                transfer.to_owned(),
                self.device.clone(),
            )));
        }
        let links = self
            .links
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(|| Error::Link("this engine is not on the tailnet".into()))?;
        let request = ServeTransferRequest {
            transfer_id: transfer.to_owned(),
            paths: from.paths.clone(),
            block: Some(tuning.block),
        };
        let params = serde_json::to_value(&request).map_err(|e| Error::Protocol(e.to_string()))?;
        links
            .call(&from.device_id, methods::SERVE_TRANSFER, params)
            .await?;
        Ok(Arc::new(LinkSource::new(
            links,
            from.device_id.clone(),
            transfer.to_owned(),
        )))
    }
}
