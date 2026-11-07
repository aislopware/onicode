//! A client of the `LocalAPI`: HTTP/1.1, one connection a request, to loopback or a unix socket.
//! The daemon answers in well under a millisecond, so there is nothing to pool.
//!
//! Ported from slopty's `slopty-tailnet` `api.rs` (MIT); `watch` is new.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Limited};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, HOST};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::{Location, Notify, Path, Status, Tailnet, Watch, WhoIs, locate};

/// How long a call may take. The daemon is on this machine: a call that takes longer is a
/// daemon that is stuck, and the caller should get on without it.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(2);
/// The largest answer read. A status lists every peer, about a kilobyte each.
const LIMIT: usize = 32 << 20;
/// The largest line of a watch read before the watch is given up as garbage.
const WATCH_LINE_LIMIT: usize = 64 << 20;
/// Notifications buffered for a slow reader before the watch waits for it.
const WATCH_DEPTH: usize = 64;

/// Why a call to the `LocalAPI` failed.
#[derive(Debug, thiserror::Error)]
pub enum LocalApiError {
    /// No Tailscale this process can read is running.
    #[error("no Tailscale this process can read is running")]
    Missing,
    /// The daemon could not be reached.
    #[error("tailscale is not reachable: {0}")]
    Io(#[from] std::io::Error),
    /// The HTTP exchange failed.
    #[error("tailscale's local API: {0}")]
    Http(#[from] hyper::Error),
    /// The daemon did not answer in time.
    #[error("tailscale's local API did not answer within {TIMEOUT:?}")]
    Timeout,
    /// The daemon refused or failed the call.
    #[error("tailscale's local API answered {code}: {body}")]
    Status {
        /// The HTTP status.
        code: u16,
        /// The start of what it said.
        body: String,
    },
    /// The answer was not what was asked for.
    #[error("tailscale's local API answered something unreadable: {0}")]
    Decode(#[from] serde_json::Error),
    /// The answer was larger than any answer should be.
    #[error("tailscale's local API answered more than {LIMIT} bytes")]
    TooLarge,
}

/// The local Tailscale daemon.
#[derive(Debug, Clone)]
pub struct LocalApi {
    at: Location,
    timeout: Duration,
}

/// A ping's answer: how long the round trip took and the path it took.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pong {
    /// The round trip.
    pub latency: Duration,
    /// How it travelled.
    pub path: Path,
}

impl LocalApi {
    /// This machine's daemon, or `None` when no Tailscale this process can read is running.
    #[must_use]
    pub fn find() -> Option<Self> {
        locate::find().map(Self::at)
    }

    /// The daemon at `at`.
    #[must_use]
    pub const fn at(at: Location) -> Self {
        Self {
            at,
            timeout: TIMEOUT,
        }
    }

    /// The tailnet as the daemon sees it: this node and every peer.
    ///
    /// # Errors
    ///
    /// When the daemon cannot be reached or answers something else.
    pub async fn status(&self) -> Result<Status, LocalApiError> {
        let body = self.call(Method::GET, "/localapi/v0/status").await?;
        Ok(serde_json::from_slice(&body)?)
    }

    /// Who sent from `from`, or `None` when no node of the tailnet has that address.
    ///
    /// # Errors
    ///
    /// When the daemon cannot be reached or answers something else.
    pub async fn whois(&self, from: SocketAddr) -> Result<Option<WhoIs>, LocalApiError> {
        let from = SocketAddr::new(from.ip().to_canonical(), from.port());
        let addr = from.to_string().replace('[', "%5B").replace(']', "%5D");
        match self
            .call(Method::GET, &format!("/localapi/v0/whois?addr={addr}"))
            .await
        {
            Ok(body) => Ok(Some(serde_json::from_slice(&body)?)),
            Err(LocalApiError::Status { code: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The UDP port this node serves as a Tailscale peer relay on, `None` when it serves as
    /// none (`ipn.Prefs.RelayServerPort`, nil when off; `0` is a port the daemon picks).
    ///
    /// # Errors
    ///
    /// When the daemon cannot be reached or answers something else.
    pub async fn relay_server_port(&self) -> Result<Option<u16>, LocalApiError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Prefs {
            #[serde(default)]
            relay_server_port: Option<u16>,
        }
        let body = self.call(Method::GET, "/localapi/v0/prefs").await?;
        Ok(serde_json::from_slice::<Prefs>(&body)?.relay_server_port)
    }

    /// A disco ping to the node at `ip`: `WireGuard`'s own path, without the IP stack at either
    /// end. Pinging a peer that is idle also makes the daemon find it a path now, so the next
    /// packet does not start on DERP.
    ///
    /// # Errors
    ///
    /// When the daemon cannot be reached, or the peer did not answer.
    pub async fn ping(&self, ip: IpAddr) -> Result<Pong, LocalApiError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Answer {
            #[serde(default)]
            err: String,
            #[serde(default)]
            latency_seconds: f64,
            #[serde(default)]
            endpoint: String,
            #[serde(default)]
            peer_relay: String,
            #[serde(rename = "DERPRegionCode", default)]
            derp: String,
        }
        let ip = ip.to_canonical();
        let body = self
            .call(
                Method::POST,
                &format!("/localapi/v0/ping?ip={ip}&type=disco"),
            )
            .await?;
        let answer: Answer = serde_json::from_slice(&body)?;
        if !answer.err.is_empty() {
            return Err(LocalApiError::Status {
                code: 200,
                body: answer.err,
            });
        }
        let path = if let Ok(direct) = answer.endpoint.parse() {
            Path::Direct(direct)
        } else if !answer.peer_relay.is_empty() {
            Path::PeerRelay(answer.peer_relay)
        } else {
            Path::Derp(answer.derp)
        };
        let latency = Duration::try_from_secs_f64(answer.latency_seconds).unwrap_or_default();
        Ok(Pong { latency, path })
    }

    /// The daemon's notifications from now on (`GET /localapi/v0/watch-ipn-bus`), one per line
    /// of its streamed answer, starting with the current state and status. Only opening the
    /// watch is held to the call timeout; the watch itself lasts until the daemon ends it.
    ///
    /// # Errors
    ///
    /// When the daemon cannot be reached or refuses the watch.
    pub async fn watch(&self) -> Result<Watch, LocalApiError> {
        let uri = format!("/localapi/v0/watch-ipn-bus?mask={}", crate::watch::MASK);
        let opened = tokio::time::timeout(self.timeout, self.open(Method::GET, &uri))
            .await
            .map_err(|_elapsed| LocalApiError::Timeout)??;
        let (response, driver) = opened;
        let code = response.status();
        if code != StatusCode::OK {
            let body = read_limited(response).await;
            driver.abort();
            return Err(refusal(code, &body?));
        }
        let (tx, rx) = mpsc::channel(WATCH_DEPTH);
        tokio::spawn(async move {
            let mut body = response.into_body();
            let mut line = Vec::new();
            while let Some(frame) = body.frame().await {
                let Ok(frame) = frame else { break };
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                line.extend_from_slice(&data);
                while let Some(end) = line.iter().position(|b| *b == b'\n') {
                    let one: Vec<u8> = line.drain(..=end).collect();
                    let Some(notify) = Notify::parse(&one) else {
                        continue;
                    };
                    if tx.send(notify).await.is_err() {
                        driver.abort();
                        return;
                    }
                }
                if line.len() > WATCH_LINE_LIMIT {
                    tracing::warn!(
                        "tailscale's watch sent a line past {WATCH_LINE_LIMIT} bytes; ended"
                    );
                    break;
                }
            }
            driver.abort();
        });
        Ok(rx)
    }

    async fn call(&self, method: Method, uri: &str) -> Result<Bytes, LocalApiError> {
        let exchange = async {
            let (response, driver) = self.open(method, uri).await?;
            let code = response.status();
            let body = read_limited(response).await;
            driver.abort();
            let body = body?;
            if code == StatusCode::OK {
                Ok(body)
            } else {
                Err(refusal(code, &body))
            }
        };
        tokio::time::timeout(self.timeout, exchange)
            .await
            .map_err(|_elapsed| LocalApiError::Timeout)?
    }

    /// Send one request; the response with its body still to read, and the task driving the
    /// connection, to abort once the body is read.
    async fn open(
        &self,
        method: Method,
        uri: &str,
    ) -> Result<(Response<Incoming>, JoinHandle<()>), LocalApiError> {
        match &self.at {
            Location::Tcp { port, token } => {
                let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, *port)).await?;
                stream.set_nodelay(true)?;
                send(stream, method, uri, Some(token)).await
            }
            Location::Unix(path) => {
                send(
                    tokio::net::UnixStream::connect(path).await?,
                    method,
                    uri,
                    None,
                )
                .await
            }
        }
    }

    /// [`Self::status`]'s timeout, shortened for a test.
    #[cfg(test)]
    pub(crate) const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[async_trait::async_trait]
impl Tailnet for LocalApi {
    async fn status(&self) -> Result<Status, LocalApiError> {
        Self::status(self).await
    }

    async fn whois(&self, from: SocketAddr) -> Result<Option<WhoIs>, LocalApiError> {
        Self::whois(self, from).await
    }

    async fn watch(&self) -> Result<Watch, LocalApiError> {
        Self::watch(self).await
    }
}

fn refusal(code: StatusCode, body: &Bytes) -> LocalApiError {
    let said = String::from_utf8_lossy(body.get(..body.len().min(200)).unwrap_or_default());
    LocalApiError::Status {
        code: code.as_u16(),
        body: said.trim().to_owned(),
    }
}

async fn read_limited(response: Response<Incoming>) -> Result<Bytes, LocalApiError> {
    match Limited::new(response.into_body(), LIMIT).collect().await {
        Ok(body) => Ok(body.to_bytes()),
        Err(e) => Err(match e.downcast::<hyper::Error>() {
            Ok(e) => LocalApiError::Http(*e),
            Err(_) => LocalApiError::TooLarge,
        }),
    }
}

async fn send<S>(
    stream: S,
    method: Method,
    uri: &str,
    token: Option<&str>,
) -> Result<(Response<Incoming>, JoinHandle<()>), LocalApiError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    // The daemon ignores the host, but HTTP/1.1 needs one; this is the name its own CLI sends.
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(HOST, "local-tailscaled.sock");
    if let Some(token) = token {
        request = request.header(AUTHORIZATION, basic(token));
    }
    let request = match request.body(Empty::<Bytes>::new()) {
        Ok(request) => request,
        Err(e) => {
            driver.abort();
            return Err(LocalApiError::Status {
                code: 0,
                body: e.to_string(),
            });
        }
    };
    match sender.send_request(request).await {
        Ok(response) => Ok((response, driver)),
        Err(e) => {
            driver.abort();
            Err(LocalApiError::Http(e))
        }
    }
}

/// `Basic` credentials with an empty user and the token as the password, as the daemon reads
/// them (`client/local/local.go`).
fn basic(token: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let raw = format!(":{token}");
    let mut out = String::from("Basic ");
    for chunk in raw.as_bytes().chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            let sextet = usize::try_from((n >> shift) & 63).unwrap_or(0);
            let digit = ALPHABET.get(sextet).copied().unwrap_or(b'=');
            out.push(if i <= chunk.len() {
                char::from(digit)
            } else {
                '='
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use tokio::net::TcpListener;

    use super::*;
    use crate::status::tests::STATUS;
    use crate::whois::tests::MINE;

    #[test]
    fn credentials_are_the_token_as_a_password() {
        assert_eq!(basic("abc"), "Basic OmFiYw==");
        assert_eq!(basic("9f86d081"), "Basic OjlmODZkMDgx");
        assert_eq!(basic("ab"), "Basic OmFi");
    }

    async fn daemon(answer: fn(&str) -> (u16, String)) -> (LocalApi, crate::fake::Seen) {
        crate::fake::daemon(answer).await.unwrap()
    }

    /// Status and whois read the daemon's answers, sending the token each time; an address
    /// the tailnet does not know is `None`, and an IPv6 caller's address is escaped.
    #[tokio::test]
    async fn the_daemon_is_asked_with_its_token_and_read() {
        let (api, seen) = daemon(|path| {
            if path == "/localapi/v0/status" {
                (200, STATUS.to_owned())
            } else if path.contains("100.64.0.4") || path.contains("fd7a") {
                (200, MINE.to_owned())
            } else {
                (404, "no match for IP:port".to_owned())
            }
        })
        .await;
        let status = api.status().await.unwrap();
        assert_eq!(status.peer.len(), 4);
        let who = api
            .whois("100.64.0.4:5000".parse().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(who.user_profile.id, 2);
        assert!(
            api.whois("[fd7a:115c:a1e0::4]:5000".parse().unwrap())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            api.whois("100.64.9.9:1".parse().unwrap())
                .await
                .unwrap()
                .is_none()
        );
        let seen = seen.lock().clone();
        assert!(
            seen.iter()
                .all(|(_, auth)| auth.as_deref() == Some("Basic OmFiYw==")),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|(p, _)| p == "/localapi/v0/whois?addr=%5Bfd7a:115c:a1e0::4%5D:5000")
        );
    }

    /// The relay port reads from the prefs: set, a port the daemon picks, and off.
    #[tokio::test]
    async fn the_peer_relay_port_reads_from_the_prefs() {
        let answers = [
            |_: &str| {
                (
                    200,
                    r#"{"WantRunning":true,"RelayServerPort":40000}"#.to_owned(),
                )
            },
            |_: &str| {
                (
                    200,
                    r#"{"WantRunning":true,"RelayServerPort":0}"#.to_owned(),
                )
            },
            |_: &str| (200, r#"{"WantRunning":true}"#.to_owned()),
        ];
        let mut got = Vec::new();
        for answer in answers {
            let (api, seen) = daemon(answer).await;
            got.push(api.relay_server_port().await.unwrap());
            assert_eq!(seen.lock()[0].0, "/localapi/v0/prefs");
        }
        assert_eq!(got, [Some(40000), Some(0), None]);
    }

    /// A refusal says what the daemon said; a mapped IPv4 caller is asked about as IPv4.
    #[tokio::test]
    async fn a_refusal_is_an_error_that_says_why() {
        let (api, seen) = daemon(|_| (403, "access denied\n".to_owned())).await;
        let error = api
            .whois("[::ffff:100.64.0.4]:1".parse().unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(&error, LocalApiError::Status { code: 403, body } if body == "access denied"),
            "{error}"
        );
        assert!(seen.lock()[0].0.ends_with("addr=100.64.0.4:1"));
    }

    /// The watch reads one notification per line of the streamed answer, and asks with the
    /// mask that brings the initial state and peer changes.
    #[tokio::test]
    async fn a_watch_reads_one_notification_per_line() {
        let (api, seen) = daemon(|_| {
            (
                200,
                "{\"State\":6}\n{\"PeerChanges\":[{}]}\n{\"ErrMessage\":\"stopping\"}\n".to_owned(),
            )
        })
        .await;
        let mut watch = api.watch().await.unwrap();
        let first = watch.recv().await.unwrap();
        assert_eq!(first.state, Some(crate::BackendState::Running));
        let second = watch.recv().await.unwrap();
        assert_eq!(second.state, None);
        let third = watch.recv().await.unwrap();
        assert_eq!(third.error.as_deref(), Some("stopping"));
        assert!(
            watch.recv().await.is_none(),
            "the watch ends with the daemon's answer"
        );
        let asked = seen.lock()[0].0.clone();
        assert_eq!(
            asked,
            format!("/localapi/v0/watch-ipn-bus?mask={}", crate::watch::MASK)
        );
    }

    /// A daemon that accepts and never answers is given up on, not waited for.
    #[tokio::test]
    async fn a_daemon_that_never_answers_is_given_up_on() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let _held = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let api = LocalApi::at(Location::Tcp {
            port,
            token: "abc".into(),
        })
        .with_timeout(Duration::from_millis(100));
        assert!(matches!(api.status().await, Err(LocalApiError::Timeout)));
    }

    /// No daemon at all is an I/O error at once.
    #[tokio::test]
    async fn no_daemon_is_an_error_at_once() {
        let api = LocalApi::at(Location::Unix(
            "/tmp/onicode-no-such-tailscaled.sock".into(),
        ));
        assert!(matches!(api.status().await, Err(LocalApiError::Io(_))));
    }

    /// Garbage where JSON should be is a decode error, not a panic.
    #[tokio::test]
    async fn an_unreadable_answer_is_an_error() {
        let (api, _) = daemon(|_| (200, "<html>".to_owned())).await;
        assert!(matches!(api.status().await, Err(LocalApiError::Decode(_))));
    }
}
