//! desktopd's control socket: the engine on the same Mac asks what it is and may do, and tells
//! it the grants guide is open (`onicode_desktop_link::ctl`). One JSON request and one JSON
//! reply per line, on a Unix socket in the data directory, readable by its user alone.

use std::path::Path;
use std::sync::Arc;

use onicode_desktop_link::ctl::{CtlReply, CtlRequest, MAX_LINE};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// What answers a request.
pub type Handler = Arc<dyn Fn(CtlRequest) -> CtlReply + Send + Sync>;

/// The socket at `path`, taken over from a desktopd before, readable by this user alone.
///
/// # Errors
///
/// When it could not be bound.
pub fn listen(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _gone => {}
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Answer every connection on `listener` with `handler`, a line at a time, until the listener
/// fails.
pub async fn serve(listener: UnixListener, handler: Handler) {
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                tokio::spawn(answer(stream, Arc::clone(&handler)));
            }
            Err(e) => {
                tracing::warn!(error = %e, "the control socket stopped accepting");
                return;
            }
        }
    }
}

/// Answer one connection's requests until it closes.
async fn answer(stream: UnixStream, handler: Handler) {
    let (read, mut write) = stream.into_split();
    let mut read = BufReader::new(read);
    loop {
        let mut line = Vec::new();
        let limit = u64::try_from(MAX_LINE).unwrap_or(u64::MAX);
        match (&mut read).take(limit).read_until(b'\n', &mut line).await {
            Ok(0) => return,
            Ok(_read) => {}
            Err(e) => {
                tracing::debug!(error = %e, "a control connection");
                return;
            }
        }
        let reply = match onicode_desktop_link::decode::<CtlRequest>(&line) {
            Ok(request) => {
                let handler = Arc::clone(&handler);
                tokio::task::spawn_blocking(move || handler(request))
                    .await
                    .unwrap_or_else(|e| CtlReply::Error(e.to_string()))
            }
            Err(e) => CtlReply::Error(e.to_string()),
        };
        let mut out = onicode_desktop_link::encode(&reply).to_vec();
        out.push(b'\n');
        if write.write_all(&out).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use onicode_desktop_link::ctl::{DesktopStatus, socket_path};
    use onicode_desktop_link::{HostGrants, Pane, WIRE};
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn the_bundle_is_the_nearest_app() {
        let exe = Path::new(
            "/Applications/Onicode.app/Contents/Library/LoginItems/Onicode Desktop.app/Contents/MacOS/Onicode Desktop",
        );
        assert_eq!(
            crate::bundle_of(exe).as_deref(),
            Some(Path::new(
                "/Applications/Onicode.app/Contents/Library/LoginItems/Onicode Desktop.app"
            ))
        );
        assert_eq!(
            crate::bundle_of(Path::new("/usr/local/bin/onicode-desktopd")),
            None
        );
    }

    #[tokio::test]
    async fn a_line_gets_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = socket_path(dir.path());
        let listener = listen(&path).unwrap();
        let status = DesktopStatus {
            pid: 7,
            host: "mac".into(),
            addrs: Vec::new(),
            wire: WIRE,
            grants: HostGrants::default(),
            displays: Vec::new(),
            bundle: None,
            clients: 0,
        };
        let told = status.clone();
        let handler: Handler = Arc::new(move |request: CtlRequest| match request {
            CtlRequest::Status => CtlReply::Status(told.clone()),
            CtlRequest::GuideOpen { .. } => CtlReply::Error("no".into()),
        });
        tokio::spawn(serve(listener, handler));
        let stream = UnixStream::connect(&path).await.unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        for (request, reply) in [
            (CtlRequest::Status, CtlReply::Status(status)),
            (
                CtlRequest::GuideOpen {
                    pane: Pane::Accessibility,
                },
                CtlReply::Error("no".into()),
            ),
        ] {
            let mut line = onicode_desktop_link::encode(&request).to_vec();
            line.push(b'\n');
            write.write_all(&line).await.unwrap();
            let mut answer = Vec::new();
            read.read_until(b'\n', &mut answer).await.unwrap();
            assert_eq!(
                onicode_desktop_link::decode::<CtlReply>(&answer).unwrap(),
                reply
            );
        }
        write.write_all(b"{garbage\n").await.unwrap();
        let mut answer = Vec::new();
        read.read_until(b'\n', &mut answer).await.unwrap();
        assert!(matches!(
            onicode_desktop_link::decode::<CtlReply>(&answer).unwrap(),
            CtlReply::Error(_)
        ));
    }
}
