//! Preview tunnels between two devices on a simulated tailnet (`onicode_net_sim`), in virtual
//! time: an HTTP exchange crosses a tunnel whole, larger than the writer's window both ways, and
//! a reset at either end reaches the other while a clean finish still lets the other side answer.
//! Every run is seeded and prints its seed when it fails.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use onicode_net::transport::endpoint_on;
use onicode_net::{Role, StreamHead, StreamRx, StreamTx};
use onicode_net_sim::{Faults, Net, NodeSpec, SimTailnet, report_seed};
use onicode_peer::{
    LinksConfig, PeerConfig, PeerDirectory, PeerEndpoint, PeerLinks, Sockets, StreamHandler,
};
use onicode_preview::tunnel::{self, BoxIo, Connector, Tunnel};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::mpsc;

const USER: i64 = 3;
const HOST: &str = "dev-host";
const VIEWER: &str = "dev-viewer";
/// Served by a hyper server that answers `/echo` with the request body and `/big` with
/// [`BIG`] bytes.
const WEB: &str = "web";
/// A service that sends a few bytes and then fails, as a dev server that crashes mid-response.
const BROKEN: &str = "broken";
/// Not a service: the host hands the raw tunnel to the test.
const RAW: &str = "raw";
const BIG: usize = 1 << 20;
/// Longer than any exchange here takes on the simulated tailnet, in virtual time.
const PATIENCE: Duration = Duration::from_secs(120);

struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

/// The host's services, reached over in-memory pipes.
struct Services;

#[async_trait::async_trait]
impl Connector for Services {
    async fn connect(&self, service: &str) -> anyhow::Result<BoxIo> {
        match service {
            WEB => {
                let (client, server) = tokio::io::duplex(64 << 10);
                tokio::spawn(
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(server), service_fn(web)),
                );
                Ok(Box::new(client))
            }
            BROKEN => Ok(Box::new(Broken { sent: false })),
            _ => anyhow::bail!("no service {service}"),
        }
    }
}

async fn web(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let body = match request.uri().path() {
        "/big" => Bytes::from(pattern(BIG)),
        _ => request
            .into_body()
            .collect()
            .await
            .map(|body| body.to_bytes())
            .unwrap_or_default(),
    };
    Ok(Response::new(Full::new(body)))
}

fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Sends the start of a response, then fails.
struct Broken {
    sent: bool,
}

impl AsyncRead for Broken {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.sent {
            return Poll::Ready(Err(io::ErrorKind::ConnectionAborted.into()));
        }
        self.sent = true;
        buf.put_slice(b"HTTP/1.1 200 OK\r\n");
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Broken {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// The host: [`RAW`] tunnels go to the test, the rest are served as the engine serves them.
struct Host {
    raw: mpsc::UnboundedSender<Tunnel>,
}

impl StreamHandler for Host {
    fn stream(&self, device: &str, head: StreamHead, tx: StreamTx, rx: StreamRx) {
        if head.name == RAW {
            let _ = self.raw.send(Tunnel::new(tx, rx));
            return;
        }
        tokio::spawn(tunnel::serve(
            Arc::new(Services),
            device.to_owned(),
            head.name,
            tx,
            rx,
        ));
    }
}

/// `device` on a node of its own, its links serving `handler`.
async fn join(
    tailnet: &SimTailnet,
    device: &str,
    seed: u64,
    handler: Option<Arc<dyn StreamHandler>>,
) -> (Arc<PeerLinks>, Arc<PeerDirectory>) {
    let node = tailnet.add(NodeSpec::owned(device, USER));
    let api = tailnet.local_api(node);
    let endpoint = PeerEndpoint::bind(
        Arc::clone(&api),
        &SimSockets(tailnet.net().clone()),
        PeerConfig::new(device, Role::Engine).with_seed(seed),
    )
    .await
    .expect("the device binds its tailnet addresses");
    let directory = PeerDirectory::new(Arc::clone(&endpoint), api);
    let links = Arc::new(PeerLinks::new(
        endpoint,
        Arc::clone(&directory),
        LinksConfig {
            service: None,
            handler,
            seed: Some(seed),
        },
    ));
    links.start();
    (links, directory)
}

/// The host's links, the viewer's links, and the raw tunnels the host accepts.
async fn pair(
    tailnet: &SimTailnet,
    seed: u64,
) -> (
    Arc<PeerLinks>,
    Arc<PeerLinks>,
    mpsc::UnboundedReceiver<Tunnel>,
) {
    let (raw, raws) = mpsc::unbounded_channel();
    let (host, host_directory) = join(tailnet, HOST, seed, Some(Arc::new(Host { raw }))).await;
    let (viewer, viewer_directory) = join(tailnet, VIEWER, seed + 1, None).await;
    for directory in [host_directory, viewer_directory] {
        directory
            .refresh()
            .await
            .expect("the simulated tailnet answers");
    }
    (host, viewer, raws)
}

async fn get(
    links: &PeerLinks,
    path: &str,
    body: Vec<u8>,
) -> Result<Bytes, Box<dyn std::error::Error>> {
    let tunnel = tunnel::open(links, HOST, WEB).await?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(tunnel)).await?;
    tokio::spawn(connection);
    let request =
        Request::post(format!("http://preview{path}")).body(Full::new(Bytes::from(body)))?;
    let response = sender.send_request(request).await?;
    assert!(response.status().is_success());
    Ok(response.into_body().collect().await?.to_bytes())
}

#[tokio::test(start_paused = true)]
async fn http_crosses_a_tunnel_whole_both_ways() {
    const SEED: u64 = 81;
    let _seed = report_seed("http_crosses_a_tunnel_whole_both_ways", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let (_host, viewer, _raws) = pair(&tailnet, SEED).await;

    let upload = pattern(600 << 10);
    let echoed = tokio::time::timeout(PATIENCE, get(&viewer, "/echo", upload.clone()))
        .await
        .expect("the echo answers in time")
        .expect("the echo answers");
    assert_eq!(echoed, upload, "an upload past the window arrives whole");

    let big = tokio::time::timeout(PATIENCE, get(&viewer, "/big", Vec::new()))
        .await
        .expect("the download answers in time")
        .expect("the download answers");
    assert_eq!(
        big,
        pattern(BIG),
        "a download past the window arrives whole"
    );
}

#[tokio::test(start_paused = true)]
async fn a_reset_at_either_end_reaches_the_other() {
    const SEED: u64 = 82;
    let _seed = report_seed("a_reset_at_either_end_reaches_the_other", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let (_host, viewer, mut raws) = pair(&tailnet, SEED).await;
    tokio::time::timeout(PATIENCE, async {
        // A viewer that gives up resets the host's side.
        let mut near = tunnel::open(&viewer, HOST, RAW).await.unwrap();
        near.write_all(b"hello").await.unwrap();
        let mut far = raws.recv().await.unwrap();
        let mut hello = [0; 5];
        far.read_exact(&mut hello).await.unwrap();
        assert_eq!(&hello, b"hello");
        drop(near);
        let error = far.read(&mut [0; 16]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);

        // A host that fails resets the viewer's side.
        let mut near = tunnel::open(&viewer, HOST, RAW).await.unwrap();
        near.write_all(b"again").await.unwrap();
        let mut far = raws.recv().await.unwrap();
        far.read_exact(&mut hello).await.unwrap();
        drop(far);
        let error = near.read(&mut [0; 16]).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);

        // A finish is not a reset: the other side reads the end and still answers.
        let mut near = tunnel::open(&viewer, HOST, RAW).await.unwrap();
        near.write_all(b"ask").await.unwrap();
        near.shutdown().await.unwrap();
        let mut far = raws.recv().await.unwrap();
        let mut asked = Vec::new();
        far.read_to_end(&mut asked).await.unwrap();
        assert_eq!(asked, b"ask");
        far.write_all(b"answer").await.unwrap();
        far.shutdown().await.unwrap();
        let mut answer = Vec::new();
        near.read_to_end(&mut answer).await.unwrap();
        assert_eq!(answer, b"answer");
    })
    .await
    .expect("every end is seen in time");
}

#[tokio::test(start_paused = true)]
async fn a_service_that_fails_or_is_missing_resets_the_viewer() {
    const SEED: u64 = 83;
    let _seed = report_seed("a_service_that_fails_or_is_missing_resets_the_viewer", SEED);
    let tailnet = SimTailnet::new(Faults::CLEAR, SEED);
    let (_host, viewer, _raws) = pair(&tailnet, SEED).await;
    tokio::time::timeout(PATIENCE, async {
        for service in [BROKEN, "missing"] {
            let mut tunnel = tunnel::open(&viewer, HOST, service).await.unwrap();
            tunnel.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
            let mut read = Vec::new();
            let error = tunnel.read_to_end(&mut read).await.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionReset, "{service}");
        }
    })
    .await
    .expect("every failure is seen in time");
}
