//! The engine as broker of this Mac's desktop (`docs/onicode/decisions/remote-desktop.md`).
//!
//! The engine captures nothing and carries no media: its Screen Recording grant would be the
//! app's, and a frame through it would be a second hop. `onicode-desktopd`, the app's login-item
//! agent, owns capture, encode, injection and its own QUIC endpoint. A client asks this engine
//! (directly or forwarded by its own engine) and gets back where desktopd listens; it then dials
//! desktopd itself.
//!
//! The engine reaches desktopd on its control socket in the data directory
//! (`onicode_desktop_link::ctl`). When nothing answers there it asks launchd to start the agent
//! (`launchctl kickstart`): never a spawn of its own, since a capturer started from another
//! process is charged to that process's grants and gets `-3801`.
//!
//! Nothing here prompts, and nothing here reads grants: a preflight in the engine would read the
//! engine's own (the app's). desktopd reads its own (`Grants::fresh`) and says them in its status.

use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
use std::time::Duration;

use onicode_desktop_link::broker::{DesktopEndpoint, GrantsReply, OpenGuideParams};
use onicode_desktop_link::ctl::{self, CtlReply, CtlRequest, DesktopStatus};
use onicode_rpc::{RpcError, RpcReply, methods};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

/// How long one exchange on the control socket may take.
const CTL_TIMEOUT: Duration = Duration::from_secs(2);
/// How long desktopd has to answer after launchd was asked to start it: a cold start lists
/// displays and binds its endpoint, well under a second.
const START_TIMEOUT: Duration = Duration::from_secs(6);
/// How often the socket is tried while desktopd starts.
const START_POLL: Duration = Duration::from_millis(100);

/// Serves `OpenDesktop`, `DesktopGrants` and `DesktopOpenGuide` for the engine whose data
/// directory is `data_dir`.
///
/// # Errors
///
/// [`RpcError::BadParams`] for parameters that do not parse; [`RpcError::Failed`] when desktopd
/// cannot be reached or started, in words a person reads.
pub async fn handle(data_dir: &Path, method: &str, params: Value) -> Result<RpcReply, RpcError> {
    match method {
        methods::OPEN_DESKTOP => RpcReply::value(&open(data_dir).await?),
        methods::DESKTOP_GRANTS => RpcReply::value(&grants(data_dir).await?),
        methods::DESKTOP_OPEN_GUIDE => {
            let params: OpenGuideParams =
                serde_json::from_value(params).map_err(|e| RpcError::BadParams(e.to_string()))?;
            open_guide(data_dir, params).await?;
            RpcReply::value(&serde_json::json!({ "ok": true }))
        }
        other => Err(RpcError::UnknownMethod(other.to_owned())),
    }
}

/// Starts or finds desktopd and says where it listens.
async fn open(data_dir: &Path) -> Result<DesktopEndpoint, RpcError> {
    let status = running(data_dir).await?;
    if status.grants.complete() && status.addrs.is_empty() {
        return Err(RpcError::Failed(
            "this Mac's desktop is not on the tailnet: is Tailscale up here?".into(),
        ));
    }
    Ok(endpoint(&device_id(data_dir), status))
}

/// The broker's answer from desktopd's status.
fn endpoint(device_id: &str, status: DesktopStatus) -> DesktopEndpoint {
    DesktopEndpoint {
        device_id: device_id.to_owned(),
        host: status.host,
        addrs: status.addrs,
        wire: status.wire,
        grants: status.grants,
        displays: status.displays,
    }
}

/// The Mac's grants, as desktopd reads them (`Grants::fresh`), starting it to ask. Never a probe
/// of the engine's own: a preflight here reads this process's grants, which are the app's.
async fn grants(data_dir: &Path) -> Result<GrantsReply, RpcError> {
    let status = running(data_dir).await?;
    Ok(GrantsReply {
        grants: status.grants,
        running: true,
    })
}

/// Opens the guide to a pane for the person at this Mac and tells desktopd, which waits for the
/// grant and exits for launchd to restart it (Screen Recording reads stale until it does).
///
/// The guide's drag panel is the app's to show, on its main thread
/// (`onicode_desktop_grants::guide::show`): the UI does so itself for its own Mac. Asked by
/// another device, the engine has no main thread to show it on, so it opens System Settings at
/// the pane and leaves the panel to the app.
async fn open_guide(data_dir: &Path, params: OpenGuideParams) -> Result<(), RpcError> {
    let status = running(data_dir).await?;
    open_pane(params.pane);
    match exchange(data_dir, CtlRequest::GuideOpen { pane: params.pane }).await {
        Ok(CtlReply::Ok) => Ok(()),
        Ok(CtlReply::Error(why)) => Err(RpcError::Failed(why)),
        Ok(other) => {
            tracing::debug!(
                ?other,
                pid = status.pid,
                "desktopd answered the guide oddly"
            );
            Ok(())
        }
        Err(e) => Err(RpcError::Failed(format!(
            "Onicode Desktop did not hear the guide open: {e}"
        ))),
    }
}

/// desktopd's status, starting it through launchd when its socket does not answer.
async fn running(data_dir: &Path) -> Result<DesktopStatus, RpcError> {
    if let Ok(status) = status(data_dir).await {
        return Ok(status);
    }
    kickstart(data_dir).await?;
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        match status(data_dir).await {
            Ok(status) => return Ok(status),
            Err(e) if tokio::time::Instant::now() >= deadline => {
                return Err(RpcError::Failed(format!(
                    "Onicode Desktop did not start on this Mac: {e}"
                )));
            }
            Err(_) => tokio::time::sleep(START_POLL).await,
        }
    }
}

/// desktopd's status, if it answers now.
async fn status(data_dir: &Path) -> Result<DesktopStatus, String> {
    match exchange(data_dir, CtlRequest::Status).await? {
        CtlReply::Status(status) => Ok(status),
        CtlReply::Error(why) => Err(why),
        CtlReply::Ok => Err("desktopd answered no status".into()),
    }
}

/// One request and its reply on desktopd's control socket.
#[cfg(unix)]
async fn exchange(data_dir: &Path, request: CtlRequest) -> Result<CtlReply, String> {
    let path = ctl::socket_path(data_dir);
    let talk = async {
        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let (read, mut write) = stream.into_split();
        let mut line = onicode_desktop_link::encode(&request).to_vec();
        line.push(b'\n');
        write.write_all(&line).await.map_err(|e| e.to_string())?;
        let mut reply = Vec::new();
        let mut read = BufReader::new(read).take(ctl::MAX_LINE as u64);
        read.read_until(b'\n', &mut reply)
            .await
            .map_err(|e| e.to_string())?;
        onicode_desktop_link::decode::<CtlReply>(&reply).map_err(|e| e.to_string())
    };
    tokio::time::timeout(CTL_TIMEOUT, talk)
        .await
        .map_err(|_elapsed| "desktopd did not answer in time".to_owned())?
}

#[cfg(not(unix))]
async fn exchange(_data_dir: &Path, _request: CtlRequest) -> Result<CtlReply, String> {
    Err("remote desktop hosts only on macOS".into())
}

/// The engine's device-id, as its peers know it.
fn device_id(data_dir: &Path) -> String {
    std::fs::read_to_string(data_dir.join("device-id"))
        .map(|id| id.trim().to_owned())
        .unwrap_or_default()
}

/// Asks launchd to start desktopd, the login item's agent.
#[cfg(target_os = "macos")]
async fn kickstart(data_dir: &Path) -> Result<(), RpcError> {
    use std::os::unix::fs::MetadataExt as _;

    // The data directory is the person's own, so its owner is the GUI session's user: no
    // `getuid` call, no `unsafe`.
    let uid = std::fs::metadata(data_dir)
        .map_err(|e| RpcError::Failed(format!("{}: {e}", data_dir.display())))?
        .uid();
    let label = onicode_desktop_grants::bundle::helper_id(&app_bundle_id());
    let target = format!("gui/{uid}/{label}");
    let out = tokio::process::Command::new("/bin/launchctl")
        .args(["kickstart", &target])
        .output()
        .await
        .map_err(|e| RpcError::Failed(format!("launchctl: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    tracing::info!(%target, said = %said.trim(), "launchd did not start desktopd");
    Err(RpcError::Failed(
        "Onicode Desktop is not set up on this Mac: open Onicode there and allow its login item"
            .into(),
    ))
}

#[cfg(not(target_os = "macos"))]
async fn kickstart(_data_dir: &Path) -> Result<(), RpcError> {
    Err(RpcError::Failed(
        "remote desktop hosts only on macOS".into(),
    ))
}

/// Opens System Settings at `pane` on this Mac.
#[cfg(target_os = "macos")]
fn open_pane(pane: onicode_desktop_link::Pane) {
    use onicode_desktop_grants::Pane as GrantsPane;
    let pane = match pane {
        onicode_desktop_link::Pane::ScreenRecording => GrantsPane::ScreenRecording,
        onicode_desktop_link::Pane::Accessibility => GrantsPane::Accessibility,
    };
    if !onicode_desktop_grants::open_pane(pane) {
        tracing::info!(?pane, "System Settings did not open at the pane");
    }
}

#[cfg(not(target_os = "macos"))]
fn open_pane(_pane: onicode_desktop_link::Pane) {}

/// The bundle id of the app this engine runs in, which names desktopd's launchd label
/// (`onicode_desktop_grants::bundle::helper_id`): `dev.aislopware.onicode` for a release,
/// `dev.aislopware.onicode.dev` for a development build (`scripts/run-macos-dev.sh`).
/// Read from the enclosing app's `Info.plist`; `ONICODE_APP_BUNDLE_ID` overrides it for an
/// engine run outside a bundle.
#[cfg(target_os = "macos")]
fn app_bundle_id() -> String {
    const RELEASE: &str = "dev.aislopware.onicode";
    if let Ok(id) = std::env::var("ONICODE_APP_BUNDLE_ID")
        && !id.is_empty()
    {
        return id;
    }
    app_bundle()
        .and_then(|app| plist::Value::from_file(app.join("Contents/Info.plist")).ok())
        .and_then(|info| {
            info.as_dictionary()?
                .get("CFBundleIdentifier")?
                .as_string()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| RELEASE.to_owned())
}

/// The app bundle this engine runs from: the `.app` whose `Contents` holds the executable.
#[cfg(target_os = "macos")]
fn app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "Contents"))
        .and_then(Path::parent)
        .filter(|app| app.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
}

#[cfg(all(test, unix))]
mod tests {
    use onicode_desktop_link::{HostGrants, WIRE};
    use tokio::net::UnixListener;

    use super::*;

    /// A desktopd that answers one status per connection.
    fn fake_desktopd(dir: &Path, status: DesktopStatus) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(ctl::socket_path(dir)).unwrap();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (read, mut write) = stream.into_split();
                let mut line = Vec::new();
                BufReader::new(read)
                    .read_until(b'\n', &mut line)
                    .await
                    .unwrap();
                let reply = match onicode_desktop_link::decode::<CtlRequest>(&line).unwrap() {
                    CtlRequest::Status => CtlReply::Status(status.clone()),
                    CtlRequest::GuideOpen { .. } => CtlReply::Ok,
                };
                let mut out = onicode_desktop_link::encode(&reply).to_vec();
                out.push(b'\n');
                write.write_all(&out).await.unwrap();
            }
        })
    }

    fn status(grants: HostGrants, addrs: Vec<std::net::SocketAddr>) -> DesktopStatus {
        DesktopStatus {
            pid: 1,
            host: "mac-studio".into(),
            addrs,
            wire: WIRE,
            grants,
            displays: Vec::new(),
            bundle: None,
            clients: 0,
        }
    }

    const GRANTED: HostGrants = HostGrants {
        screen_recording: true,
        accessibility: true,
        post_event: true,
    };

    #[tokio::test]
    async fn a_running_desktopd_is_handed_back_with_the_device() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("device-id"), "dev-1\n").unwrap();
        let addr = "100.64.0.7:7434".parse().unwrap();
        let _desktopd = fake_desktopd(dir.path(), status(GRANTED, vec![addr]));
        let reply = handle(dir.path(), methods::OPEN_DESKTOP, Value::Null)
            .await
            .unwrap();
        let RpcReply::Value(value) = reply else {
            panic!("not a value")
        };
        let endpoint: DesktopEndpoint = serde_json::from_value(value).unwrap();
        assert_eq!(endpoint.device_id, "dev-1");
        assert_eq!(endpoint.addrs, vec![addr]);
        assert!(endpoint.grants.complete());
    }

    #[tokio::test]
    async fn a_granted_desktopd_off_the_tailnet_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let _desktopd = fake_desktopd(dir.path(), status(GRANTED, Vec::new()));
        let error = open(dir.path()).await.unwrap_err();
        assert!(error.to_string().contains("tailnet"), "{error}");
    }

    #[tokio::test]
    async fn an_ungranted_desktopd_is_handed_back_to_be_told() {
        let dir = tempfile::tempdir().unwrap();
        let _desktopd = fake_desktopd(dir.path(), status(HostGrants::default(), Vec::new()));
        let endpoint = open(dir.path()).await.unwrap();
        assert_eq!(endpoint.grants.missing().len(), 2);
        let reply = grants(dir.path()).await.unwrap();
        assert!(reply.running);
    }
}
