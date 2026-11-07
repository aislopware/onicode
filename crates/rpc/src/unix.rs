//! The engine's local door: onicode-rpc's envelopes as ndjson over one Unix socket, every call
//! and watch of a connection multiplexed on it (engine.md, "Encoding").
//!
//! Admission takes two gates, both required. The peer's uid (`getpeereid`, through
//! [`UnixStream::peer_cred`]) must be the engine's own, and the connection's first line must
//! carry one of the tokens the engine published at start ([`crate::ipc_auth`]): the full one
//! (the app) or the agent-scoped one (an agent's `onicode mcp`). The server answers that line
//! with `{"id":0,"ok":"full"|"agent"}`, or `{"id":0,"err":…}` and closes. What a scope may call
//! is the server's policy: [`serve_unix`] hands each admitted connection's [`Admission`] to the
//! caller, who returns the service that connection talks to.
//!
//! The path is lean: one reader and one writer per connection, the read buffer reused across
//! frames, and every frame that is ready when the writer wakes leaves in one `write`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::ipc_auth::{IpcTokens, Scope, token_scope};
use crate::{RpcClient, RpcService, ServerFrame, serve_connection};

/// The largest frame either end accepts: a runaway guard, past any transcript snapshot.
const MAX_FRAME: usize = 512 << 20;
/// The largest admission line: a token and a chat id.
const MAX_AUTH: usize = 4 << 10;
/// How long a connection may take to present its token before it is closed.
const AUTH_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a dial may take. A local engine answers in microseconds; this only bounds a wedged
/// one.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// The read buffer, and the write batch a connection starts with.
const BUFFER: usize = 64 << 10;
/// A buffer grown past this by one large frame (a snapshot) is let go after it.
const KEEP_BUFFER: usize = 4 << 20;
/// Frames ready at once are written together up to this size.
const BATCH: usize = 1 << 20;
/// Frames queued per direction per connection.
const QUEUE: usize = 256;

/// The first line a client writes.
#[derive(Serialize)]
struct AuthOut<'a> {
    token: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    chat: Option<&'a str>,
}

/// The first line as the server reads it.
#[derive(Deserialize)]
struct AuthIn {
    token: String,
    #[serde(default)]
    chat: Option<String>,
}

/// Who may connect: the engine's tokens, and the uid every peer must run as.
#[derive(Clone)]
pub struct Gate {
    /// The tokens a connection's first line must present one of.
    pub tokens: IpcTokens,
    /// The engine's own effective uid.
    pub uid: u32,
}

/// An admitted connection: the scope its token grants and the chat an agent's server says it
/// serves (claimed, for the record only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    /// What the presented token grants.
    pub scope: Scope,
    /// The chat the connection names, if any.
    pub chat: Option<String>,
}

/// Why a dial failed.
#[derive(Debug, thiserror::Error)]
pub enum DialError {
    /// Nothing serves the socket: no engine runs, or one is still starting.
    #[error("no engine answers: {0}")]
    Absent(String),
    /// The engine refused this client: a token from an earlier engine run, or another user.
    #[error("the engine refused this connection: {0}")]
    Denied(String),
    /// Anything else.
    #[error("{0}")]
    Failed(String),
}

/// Whether a peer running as `peer` may connect to an engine running as `own`.
#[must_use]
pub fn admits_uid(peer: u32, own: u32) -> bool {
    peer == own
}

/// Accepts connections on `listener` until this future is dropped, which closes every
/// connection it accepted. Each one passes `gate`, then talks to the service `admit` returns for
/// its [`Admission`].
pub async fn serve_unix<F>(listener: UnixListener, gate: Gate, admit: F)
where
    F: Fn(Admission) -> Arc<dyn RpcService> + Send + Sync + 'static,
{
    let gate = Arc::new(gate);
    let admit = Arc::new(admit);
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(serve_stream(stream, gate.clone(), admit.clone()));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "rpc: accept failed");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

async fn serve_stream<F>(stream: UnixStream, gate: Arc<Gate>, admit: Arc<F>)
where
    F: Fn(Admission) -> Arc<dyn RpcService> + Send + Sync + 'static,
{
    let uid = match stream.peer_cred() {
        Ok(credentials) => credentials.uid(),
        Err(err) => {
            tracing::warn!(error = %err, "rpc: cannot read a local peer's credentials");
            return;
        }
    };
    if !admits_uid(uid, gate.uid) {
        tracing::warn!(uid, "rpc: refused a local connection from another user");
        return;
    }
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::with_capacity(BUFFER, read);
    let mut line = Vec::with_capacity(BUFFER);
    let presented =
        tokio::time::timeout(AUTH_TIMEOUT, read_frame(&mut reader, &mut line, MAX_AUTH))
            .await
            .ok()
            .and_then(Result::ok)
            .filter(|read| *read)
            .and_then(|_| serde_json::from_slice::<AuthIn>(&line).ok());
    let admission = presented.and_then(|auth| {
        token_scope(&gate.tokens, auth.token.as_bytes()).map(|scope| Admission {
            scope,
            chat: auth.chat.map(|chat| chat.chars().take(128).collect()),
        })
    });
    let reply = match &admission {
        Some(admission) => ServerFrame {
            id: 0,
            ok: Some(serde_json::Value::from(match admission.scope {
                Scope::Full => "full",
                Scope::Agent => "agent",
            })),
            ..ServerFrame::default()
        },
        None => ServerFrame {
            id: 0,
            err: Some("missing or wrong IPC token (see {data_dir}/ipc-token)".into()),
            ..ServerFrame::default()
        },
    };
    let written = match serde_json::to_vec(&reply) {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            write.write_all(&bytes).await.is_ok()
        }
        Err(_) => false,
    };
    let Some(admission) = admission else {
        tracing::warn!("rpc: refused a local connection without the engine's token");
        return;
    };
    if !written {
        return;
    }
    let service = admit(admission);
    let (out_tx, out_rx) = mpsc::channel::<String>(QUEUE);
    let (in_tx, in_rx) = mpsc::channel::<String>(QUEUE);
    line.clear();
    // One task: whichever side ends first ends the connection, and dropping the others aborts
    // what the connection still runs.
    tokio::select! {
        () = serve_connection(service, out_tx, in_rx) => {}
        () = read_frames(reader, line, in_tx) => {}
        () = write_frames(write, out_rx) => {}
    }
}

/// Dials the engine at `path` with `token`, naming `chat` when an agent's server dials. Runs
/// inside a tokio runtime: the connection's reader and writer are spawned on it.
pub async fn connect_unix(
    path: &Path,
    token: &str,
    chat: Option<&str>,
) -> Result<RpcClient, DialError> {
    let stream = match tokio::time::timeout(CONNECT_TIMEOUT, UnixStream::connect(path)).await {
        Err(_) => {
            return Err(DialError::Failed(format!(
                "timed out dialing {}",
                path.display()
            )));
        }
        Ok(Err(err))
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(DialError::Absent(err.to_string()));
        }
        Ok(Err(err)) => return Err(DialError::Failed(err.to_string())),
        Ok(Ok(stream)) => stream,
    };
    let (read, mut write) = stream.into_split();
    let mut auth = serde_json::to_vec(&AuthOut { token, chat })
        .map_err(|err| DialError::Failed(err.to_string()))?;
    auth.push(b'\n');
    write
        .write_all(&auth)
        .await
        .map_err(|err| DialError::Denied(err.to_string()))?;
    let mut reader = BufReader::with_capacity(BUFFER, read);
    let mut line = Vec::with_capacity(BUFFER);
    match tokio::time::timeout(AUTH_TIMEOUT, read_frame(&mut reader, &mut line, MAX_AUTH)).await {
        Err(_) => {
            return Err(DialError::Failed(
                "the engine did not answer the token".into(),
            ));
        }
        Ok(Err(err)) => return Err(DialError::Denied(err.to_string())),
        Ok(Ok(false)) => return Err(DialError::Denied("the engine closed the connection".into())),
        Ok(Ok(true)) => {
            let reply: ServerFrame = serde_json::from_slice(&line)
                .map_err(|err| DialError::Failed(format!("garbled admission: {err}")))?;
            if let Some(err) = reply.err {
                return Err(DialError::Denied(err));
            }
        }
    }
    line.clear();
    let (out_tx, out_rx) = mpsc::channel::<String>(QUEUE);
    let (in_tx, in_rx) = mpsc::channel::<String>(QUEUE);
    let client = RpcClient::new(out_tx, in_rx);
    tokio::spawn(async move {
        tokio::select! {
            () = read_frames(reader, line, in_tx) => {}
            () = write_frames(write, out_rx) => {}
        }
    });
    Ok(client)
}

/// Reads one `\n`-ended frame into `line` (without the newline): `Ok(false)` at the end of the
/// stream, an error past `max` bytes.
async fn read_frame<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<bool> {
    line.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(false);
        }
        let (taken, done) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => {
                line.extend_from_slice(available.get(..at).unwrap_or_default());
                (at + 1, true)
            }
            None => {
                line.extend_from_slice(available);
                (available.len(), false)
            }
        };
        reader.consume(taken);
        if line.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("a frame past {max} bytes"),
            ));
        }
        if done {
            return Ok(true);
        }
    }
}

/// Feeds `frames` from `reader` until it ends, fails or the receiver goes.
async fn read_frames<R: AsyncBufRead + Unpin>(
    mut reader: R,
    mut line: Vec<u8>,
    frames: mpsc::Sender<String>,
) {
    loop {
        match read_frame(&mut reader, &mut line, MAX_FRAME).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(err) => {
                tracing::warn!(error = %err, "rpc: dropping a local connection");
                return;
            }
        }
        match std::str::from_utf8(&line) {
            Ok(text) => {
                if frames.send(text.to_owned()).await.is_err() {
                    return;
                }
            }
            Err(err) => tracing::warn!(error = %err, "rpc: dropping a frame that is not UTF-8"),
        }
        if line.capacity() > KEEP_BUFFER {
            line = Vec::with_capacity(BUFFER);
        }
    }
}

/// Writes what arrives on `frames`, each followed by a newline, every frame that is ready in
/// one write, until the senders go or the write fails.
async fn write_frames<W: AsyncWrite + Unpin>(mut writer: W, mut frames: mpsc::Receiver<String>) {
    let mut batch: Vec<u8> = Vec::with_capacity(BUFFER);
    while let Some(first) = frames.recv().await {
        batch.clear();
        batch.extend_from_slice(first.as_bytes());
        batch.push(b'\n');
        drop(first);
        while batch.len() < BATCH {
            let Ok(next) = frames.try_recv() else {
                break;
            };
            batch.extend_from_slice(next.as_bytes());
            batch.push(b'\n');
        }
        if let Err(err) = writer.write_all(&batch).await {
            tracing::debug!(error = %err, "rpc: a local connection closed");
            return;
        }
        if batch.capacity() > KEEP_BUFFER {
            batch = Vec::with_capacity(BUFFER);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;

    use async_trait::async_trait;

    use super::*;
    use crate::{RpcError, RpcReply, caller};

    struct Echo;

    #[async_trait]
    impl RpcService for Echo {
        async fn handle(
            &self,
            method: &str,
            params: serde_json::Value,
        ) -> Result<RpcReply, RpcError> {
            match method {
                "Who" => RpcReply::value(&caller().record()),
                "Count" => {
                    let n = params.as_u64().unwrap_or(0);
                    Ok(RpcReply::Stream(Box::pin(futures::stream::iter(
                        (0..n).map(serde_json::Value::from),
                    ))))
                }
                "Big" => Ok(RpcReply::Value(serde_json::Value::from(
                    "x".repeat(3 << 20),
                ))),
                _ => Ok(RpcReply::Value(params)),
            }
        }
    }

    /// Serves `Echo` in `dir` for a peer that must run as `uid`; every connection's admission is
    /// recorded.
    fn serve(
        dir: &Path,
        uid: u32,
        tokens: IpcTokens,
    ) -> (
        std::path::PathBuf,
        Arc<std::sync::Mutex<Vec<Admission>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let path = dir.join("engine.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let admitted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = admitted.clone();
        let task = tokio::spawn(serve_unix(
            listener,
            Gate { tokens, uid },
            move |admission| {
                seen.lock().unwrap().push(admission);
                Arc::new(Echo) as Arc<dyn RpcService>
            },
        ));
        (path, admitted, task)
    }

    fn own_uid(dir: &Path) -> u32 {
        std::fs::metadata(dir).unwrap().uid()
    }

    /// Calls, streams and frames past the read buffer cross one connection; the token picks the
    /// scope the server is handed.
    #[tokio::test]
    async fn calls_and_streams_cross_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = IpcTokens::generate().unwrap();
        let (path, admitted, _server) = serve(dir.path(), own_uid(dir.path()), tokens.clone());
        let client = connect_unix(&path, &tokens.full, None).await.unwrap();
        assert_eq!(
            client
                .call("Echo", serde_json::json!({"x": 1}))
                .await
                .unwrap(),
            serde_json::json!({"x": 1})
        );
        let mut items = client
            .subscribe_scoped("Count", serde_json::json!(3))
            .await
            .unwrap();
        let mut got = Vec::new();
        while let Some(item) = items.recv().await {
            got.push(item);
        }
        assert_eq!(
            got,
            vec![
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2)
            ]
        );
        assert!(!items.lost(), "the server ended the stream");
        let big = client.call("Big", serde_json::Value::Null).await.unwrap();
        assert_eq!(big.as_str().map(str::len), Some(3 << 20));
        let agent = connect_unix(&path, &tokens.agent, Some("chat-1"))
            .await
            .unwrap();
        assert_eq!(
            agent.call("Echo", serde_json::json!(2)).await.unwrap(),
            serde_json::json!(2)
        );
        assert_eq!(
            *admitted.lock().unwrap(),
            vec![
                Admission {
                    scope: Scope::Full,
                    chat: None
                },
                Admission {
                    scope: Scope::Agent,
                    chat: Some("chat-1".into())
                },
            ]
        );
    }

    /// A wrong token is refused before any service sees the connection.
    #[tokio::test]
    async fn a_wrong_token_is_denied() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = IpcTokens::generate().unwrap();
        let (path, admitted, _server) = serve(dir.path(), own_uid(dir.path()), tokens);
        let stale = IpcTokens::generate().unwrap();
        assert!(matches!(
            connect_unix(&path, &stale.full, None).await,
            Err(DialError::Denied(_))
        ));
        assert!(admitted.lock().unwrap().is_empty());
    }

    /// A peer running as another user is closed before it may present a token.
    #[tokio::test]
    async fn another_users_connection_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = IpcTokens::generate().unwrap();
        let other = own_uid(dir.path()).wrapping_add(1);
        let (path, admitted, _server) = serve(dir.path(), other, tokens.clone());
        assert!(matches!(
            connect_unix(&path, &tokens.full, None).await,
            Err(DialError::Denied(_))
        ));
        assert!(admitted.lock().unwrap().is_empty());
        assert!(admits_uid(501, 501));
        assert!(!admits_uid(0, 501), "not even root's");
    }

    /// No listener, or a socket file a crashed engine left, reads as absent: the viewer launches.
    #[tokio::test]
    async fn a_stale_socket_reads_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("engine.sock");
        assert!(matches!(
            connect_unix(&path, "token", None).await,
            Err(DialError::Absent(_))
        ));
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "the crashed engine's socket file stays");
        assert!(matches!(
            connect_unix(&path, "token", None).await,
            Err(DialError::Absent(_))
        ));
    }

    /// The server going away closes the client's streams as lost, and its calls as closed.
    #[tokio::test]
    async fn a_dropped_server_reads_as_lost() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = IpcTokens::generate().unwrap();
        let (path, _, server) = serve(dir.path(), own_uid(dir.path()), tokens.clone());
        let client = connect_unix(&path, &tokens.full, None).await.unwrap();
        let mut items = client
            .subscribe_scoped("Count", serde_json::json!(u64::MAX))
            .await
            .unwrap();
        assert_eq!(items.recv().await, Some(serde_json::json!(0)));
        server.abort();
        while items.recv().await.is_some() {}
        assert!(items.lost());
        client.closed().await;
        assert!(client.is_closed());
        assert!(matches!(
            client.call("Echo", serde_json::Value::Null).await,
            Err(RpcError::Closed)
        ));
    }

    /// Frames split anywhere reassemble, frames past the cap fail the read, and the end of the
    /// stream mid-frame is the end.
    #[tokio::test]
    async fn frames_reassemble_and_runaways_fail() {
        let mut reader = BufReader::with_capacity(4, &b"{\"a\":1}\n{\"b\":2}\npartial"[..]);
        let mut line = Vec::new();
        assert!(read_frame(&mut reader, &mut line, 64).await.unwrap());
        assert_eq!(line, b"{\"a\":1}");
        assert!(read_frame(&mut reader, &mut line, 64).await.unwrap());
        assert_eq!(line, b"{\"b\":2}");
        assert!(!read_frame(&mut reader, &mut line, 64).await.unwrap());
        let mut reader = BufReader::with_capacity(4, &b"0123456789\n"[..]);
        assert!(read_frame(&mut reader, &mut line, 5).await.is_err());
    }
}
