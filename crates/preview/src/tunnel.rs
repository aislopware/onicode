//! Previews on the peer links. Every TCP connection to another device's preview (or to a
//! sign-in callback there) is one QUIC stream of its own (`StreamKind::Tunnel`, named by the
//! service it reaches), raw bytes both ways, sent behind terminals and RPC. A reset at either end
//! reaches the other: a browser that gives up cancels the request at the dev server, and a dev
//! server that fails mid-response fails it at the browser.
//!
//! A stream named [`CATALOG`] is the catalog feed instead: the host sends its local services as
//! JSON frames whenever they change, and the viewer lists them under the host's name.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use bytes::Bytes;
use onicode_net::{NetError, Role, StreamHead, StreamKind, StreamRx, StreamTx, close};
use onicode_peer::{PeerLinks, StreamHandler};
use onicode_proto::PreviewService;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::catalog::Catalog;

/// The tunnel name of the catalog feed (service ids are UUIDs and `agent-login:` names).
pub const CATALOG: &str = "catalog";
/// Bytes a tunnel's writer may queue ahead of the link before it waits: enough to keep a fast
/// path busy, little enough that an upload never piles up in memory.
const WINDOW: usize = 256 << 10;
/// How long the host gives its local service to accept a tunnel's connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest catalog frame read.
const MAX_CATALOG: usize = 1 << 20;
/// A catalog feed that ended while its link stays up is reopened after this.
const FEED_RETRY: Duration = Duration::from_secs(2);

/// A byte stream: a TCP socket, a tunnel, an in-memory pipe in tests.
pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
/// A boxed [`Io`].
pub type BoxIo = Box<dyn Io>;

/// Opens a connection to one of this device's services by id.
#[async_trait::async_trait]
pub trait Connector: Send + Sync + 'static {
    /// A connection to `service`, for this device itself.
    async fn connect(&self, service: &str) -> anyhow::Result<BoxIo>;
    /// [`Self::connect`] for a tunnel peer device `peer` opened. Services scoped to one device
    /// (sign-in callbacks) check `peer`; the rest ignore it.
    async fn connect_from(&self, peer: Option<&str>, service: &str) -> anyhow::Result<BoxIo> {
        let _ = peer;
        self.connect(service).await
    }
}

type Reading = Pin<Box<dyn Future<Output = (StreamRx, Result<Option<Bytes>, NetError>)> + Send>>;
type Room = Pin<Box<dyn Future<Output = ()> + Send>>;

/// One tunnel stream as a byte stream. Shutting its write side down finishes the stream;
/// dropping it before that resets it, which the other end reads as an error.
pub struct Tunnel {
    tx: Option<StreamTx>,
    rx: Option<StreamRx>,
    reading: Option<Reading>,
    buffered: Bytes,
    eof: bool,
    room: Option<Room>,
}

impl Tunnel {
    /// The tunnel over a stream just opened or accepted.
    #[must_use]
    pub fn new(tx: StreamTx, rx: StreamRx) -> Self {
        tx.set_priority(StreamKind::Tunnel.priority());
        Self {
            tx: Some(tx),
            rx: Some(rx),
            reading: None,
            buffered: Bytes::new(),
            eof: false,
            room: None,
        }
    }
}

impl AsyncRead for Tunnel {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if buf.remaining() == 0 || this.eof {
                return Poll::Ready(Ok(()));
            }
            if !this.buffered.is_empty() {
                let n = buf.remaining().min(this.buffered.len());
                buf.put_slice(&this.buffered.split_to(n));
                return Poll::Ready(Ok(()));
            }
            if this.reading.is_none() {
                let Some(mut rx) = this.rx.take() else {
                    return Poll::Ready(Err(io::ErrorKind::NotConnected.into()));
                };
                this.reading = Some(Box::pin(async move {
                    let read = rx.chunk().await;
                    (rx, read)
                }) as Reading);
            }
            let Some(reading) = this.reading.as_mut() else {
                continue;
            };
            let (rx, read) = ready!(reading.as_mut().poll(cx));
            this.reading = None;
            this.rx = Some(rx);
            match read {
                Ok(Some(chunk)) => this.buffered = chunk,
                Ok(None) => this.eof = true,
                Err(error) => {
                    return Poll::Ready(Err(io::Error::new(io::ErrorKind::ConnectionReset, error)));
                }
            }
        }
    }
}

impl AsyncWrite for Tunnel {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        loop {
            let Some(tx) = this.tx.as_ref() else {
                return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
            };
            if tx.is_closed() {
                return Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()));
            }
            if tx.pending() <= WINDOW {
                tx.send(Bytes::copy_from_slice(buf));
                this.room = None;
                return Poll::Ready(Ok(buf.len()));
            }
            let room = this
                .room
                .get_or_insert_with(|| Box::pin(tx.room(WINDOW)) as Room);
            ready!(room.as_mut().poll(cx));
            this.room = None;
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if let Some(tx) = self.get_mut().tx.take() {
            tx.finish();
        }
        Poll::Ready(Ok(()))
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            tx.reset(close::NORMAL);
        }
        if !self.eof
            && let Some(rx) = self.rx.as_mut()
        {
            rx.stop(close::NORMAL);
        }
    }
}

/// Opens a tunnel to `service` on `device`. Takes no round trip: a refusal shows as a reset on
/// the first read.
///
/// # Errors
///
/// When there is no link to `device` and none can be dialled.
pub async fn open(links: &PeerLinks, device: &str, service: &str) -> anyhow::Result<Tunnel> {
    let (tx, rx) = links
        .open(device, &StreamHead::new(StreamKind::Tunnel, service))
        .await?;
    Ok(Tunnel::new(tx, rx))
}

/// Carries a tunnel `device` opened to `service` here, until either end is done. A service
/// that refuses, or fails, resets the stream.
pub async fn serve(
    connector: Arc<dyn Connector>,
    device: String,
    service: String,
    tx: StreamTx,
    rx: StreamRx,
) {
    let mut tunnel = Tunnel::new(tx, rx);
    let mut socket = match tokio::time::timeout(
        CONNECT_TIMEOUT,
        connector.connect_from(Some(&device), &service),
    )
    .await
    {
        Ok(Ok(socket)) => socket,
        Ok(Err(error)) => {
            tracing::debug!(%error, %device, %service, "tunnel refused");
            return;
        }
        Err(_) => {
            tracing::debug!(%device, %service, "tunnel's service did not accept in time");
            return;
        }
    };
    if let Err(error) = tokio::io::copy_bidirectional(&mut tunnel, &mut socket).await {
        tracing::debug!(%error, %device, %service, "tunnel ended");
    }
}

/// Sends this device's local services on a catalog stream whenever they change.
async fn serve_catalog(catalog: Catalog, tx: StreamTx, mut rx: StreamRx) {
    tx.set_priority(StreamKind::Tunnel.priority());
    let mut changes = catalog.subscribe();
    let mut advertised: Option<Vec<PreviewService>> = None;
    loop {
        let services = catalog.local_services();
        if advertised.as_ref() != Some(&services) {
            let Ok(json) = serde_json::to_vec(&services) else {
                return;
            };
            if tx.send_frame(&json).is_err() {
                return;
            }
            advertised = Some(services);
        }
        tokio::select! {
            changed = changes.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            frame = rx.frame() => {
                if !matches!(frame, Ok(Some(_))) {
                    return;
                }
            }
        }
    }
}

/// Takes the tunnels and catalog feeds peers open to this device.
pub struct Tunnels {
    pub(crate) connector: Arc<dyn Connector>,
    pub(crate) catalog: Catalog,
}

impl StreamHandler for Tunnels {
    fn stream(&self, device: &str, head: StreamHead, tx: StreamTx, rx: StreamRx) {
        if head.kind != StreamKind::Tunnel || head.name.is_empty() {
            tx.reset(close::PROTOCOL);
            return;
        }
        if head.name == CATALOG {
            tokio::spawn(serve_catalog(self.catalog.clone(), tx, rx));
        } else {
            tokio::spawn(serve(
                self.connector.clone(),
                device.to_owned(),
                head.name,
                tx,
                rx,
            ));
        }
    }
}

/// Reads `device`'s catalog feed into `catalog` until it ends, then drops its services.
async fn feed(links: Arc<PeerLinks>, catalog: Catalog, device: String) {
    match links
        .open(&device, &StreamHead::new(StreamKind::Tunnel, CATALOG))
        .await
    {
        Ok((tx, mut rx)) => {
            rx.set_max_frame(MAX_CATALOG);
            while let Ok(Some(frame)) = rx.frame().await {
                let listed = serde_json::from_slice::<Vec<PreviewService>>(&frame)
                    .map_err(anyhow::Error::from)
                    .and_then(|services| catalog.set_remote(&device, services));
                if let Err(error) = listed {
                    tracing::debug!(%error, %device, "unreadable preview catalog");
                    break;
                }
            }
            rx.stop(close::NORMAL);
            tx.finish();
        }
        Err(error) => tracing::debug!(%error, %device, "no preview catalog feed"),
    }
    catalog.remove_remote(&device);
}

/// Keeps one catalog feed open to every engine this device has a live link to, until `stop`.
pub async fn follow_catalogs(links: Arc<PeerLinks>, catalog: Catalog, stop: CancellationToken) {
    let mut changes = links.subscribe_changes();
    let (ended_tx, mut ended) = mpsc::unbounded_channel::<String>();
    let mut feeds = HashMap::new();
    loop {
        let live: HashSet<String> = links
            .links()
            .into_iter()
            .filter(|link| link.peer.role == Role::Engine)
            .map(|link| link.device)
            .collect();
        for device in live {
            if feeds.contains_key(&device) {
                continue;
            }
            let (links, catalog, ended_tx) = (links.clone(), catalog.clone(), ended_tx.clone());
            let task = tokio::spawn({
                let device = device.clone();
                async move {
                    feed(links, catalog, device.clone()).await;
                    tokio::time::sleep(FEED_RETRY).await;
                    let _ = ended_tx.send(device);
                }
            });
            feeds.insert(device, task);
        }
        tokio::select! {
            () = stop.cancelled() => break,
            changed = changes.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            Some(device) = ended.recv() => {
                feeds.remove(&device);
            }
        }
    }
    for task in feeds.values() {
        task.abort();
    }
    catalog.clear_remote();
}
