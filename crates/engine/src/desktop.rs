//! The Desktop group: the engine as broker of this Mac's desktop
//! (`docs/onicode/decisions/remote-desktop.md`), ported from zeron's
//! (`git show f01736b3^:crates/engine/src/desktop.rs`) onto the typed protocol.
//!
//! The engine captures nothing and carries no media: its Screen Recording grant would be the
//! app's, and a frame through it would be a second hop. `onicode-desktopd`, the app's login-item
//! agent, owns capture, encode, injection and its own QUIC endpoint. A viewer asks this engine
//! (directly or forwarded by its own engine) and gets back where desktopd listens; it then dials
//! desktopd itself.
//!
//! The engine reaches desktopd on its control socket in the data directory they share
//! (`onicode_desktop_link::ctl`). When nothing answers there it asks launchd to start the agent
//! (`launchctl kickstart`): never a spawn of its own, since a capturer started from another
//! process is charged to that process's grants and gets `-3801`.
//!
//! Nothing here prompts, and nothing here reads grants: a preflight in the engine would read the
//! engine's own (the app's). desktopd reads its own (`Grants::fresh`) and says them in its status.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use onicode_desktop_link::ctl::{self, CtlReply, CtlRequest, DesktopStatus};
use onicode_protocol::Call;
use onicode_protocol::desktop::{
    DesktopEndpoint, DesktopGrants, DesktopOpenGuide, GrantsReply, OpenDesktop, Pane,
};
use onicode_rpc::{RpcError, RpcReply, RpcService};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

use crate::peers::Identity;
use crate::service::{Group, reply, routed};

/// Every method this group serves.
const METHODS: [&str; 3] = [
    OpenDesktop::NAME,
    DesktopGrants::NAME,
    DesktopOpenGuide::NAME,
];

/// How long one exchange on the control socket may take.
const CTL_TIMEOUT: Duration = Duration::from_secs(2);
/// How long desktopd has to answer after launchd was asked to start it: a cold start lists
/// displays and binds its endpoint, well under a second.
const START_TIMEOUT: Duration = Duration::from_secs(6);
/// How often the socket is tried while desktopd starts.
const START_POLL: Duration = Duration::from_millis(100);

/// Serves `OpenDesktop`, `DesktopGrants` and `DesktopOpenGuide` for the engine whose data
/// directory is `data_dir`; an engine in memory has none, and no desktop.
pub struct DesktopService {
    data_dir: Option<PathBuf>,
    identity: Arc<Identity>,
}

impl DesktopService {
    #[must_use]
    pub fn new(data_dir: Option<PathBuf>, identity: Arc<Identity>) -> Self {
        Self { data_dir, identity }
    }

    fn data_dir(&self) -> Result<&Path, String> {
        self.data_dir
            .as_deref()
            .ok_or_else(|| "This engine keeps no data on disk, so it has no desktop".to_owned())
    }
}

impl Group for DesktopService {
    fn serves(&self, method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait]
impl RpcService for DesktopService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        match method {
            OpenDesktop::NAME => {
                let (_, OpenDesktop {}) = routed(params)?;
                let data_dir = self.data_dir().map_err(RpcError::Failed)?;
                reply(open(data_dir, &self.identity.id().0).await)
            }
            DesktopGrants::NAME => {
                let (_, DesktopGrants {}) = routed(params)?;
                let data_dir = self.data_dir().map_err(RpcError::Failed)?;
                reply(grants(data_dir).await)
            }
            DesktopOpenGuide::NAME => {
                let (_, DesktopOpenGuide { pane }) = routed(params)?;
                let data_dir = self.data_dir().map_err(RpcError::Failed)?;
                reply(open_guide(data_dir, pane).await)
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}

/// Starts or finds desktopd and says where it listens, as the engine `device_id`.
async fn open(data_dir: &Path, device_id: &str) -> Result<DesktopEndpoint, String> {
    let status = running(data_dir).await?;
    if status.grants.complete() && status.addrs.is_empty() {
        return Err("This Mac's desktop is not on the tailnet: is Tailscale up there?".into());
    }
    Ok(endpoint(device_id, status))
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
async fn grants(data_dir: &Path) -> Result<GrantsReply, String> {
    let status = running(data_dir).await?;
    Ok(GrantsReply {
        grants: status.grants,
        running: true,
    })
}

/// Opens System Settings at `pane` for the person at this Mac and tells desktopd, which waits
/// for the grant and exits for launchd to restart it (Screen Recording reads stale until it
/// does).
///
/// The guide's drag panel is the app's to show, on its main thread
/// (`onicode_grants::guide::show`): the app does so itself for its own Mac. Asked by another
/// device, the engine has no main thread to show it on, so it opens System Settings at the pane
/// and leaves the panel to the app.
async fn open_guide(data_dir: &Path, pane: Pane) -> Result<(), String> {
    running(data_dir).await?;
    open_pane(pane);
    match exchange(data_dir, CtlRequest::GuideOpen { pane }).await {
        // A status where `Ok` was due still says desktopd heard it.
        Ok(CtlReply::Ok | CtlReply::Status(_)) => Ok(()),
        Ok(CtlReply::Error(why)) => Err(why),
        Err(error) => Err(format!(
            "Onicode Desktop did not hear the guide open: {error}"
        )),
    }
}

/// desktopd's status, starting it through launchd when its socket does not answer.
async fn running(data_dir: &Path) -> Result<DesktopStatus, String> {
    if let Ok(status) = status(data_dir).await {
        return Ok(status);
    }
    kickstart(data_dir).await?;
    let deadline = tokio::time::Instant::now() + START_TIMEOUT;
    loop {
        match status(data_dir).await {
            Ok(status) => return Ok(status),
            Err(error) if tokio::time::Instant::now() >= deadline => {
                return Err(format!(
                    "Onicode Desktop did not start on this Mac: {error}"
                ));
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
async fn exchange(data_dir: &Path, request: CtlRequest) -> Result<CtlReply, String> {
    let path = ctl::socket_path(data_dir);
    let talk = async {
        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let (read, mut write) = stream.into_split();
        let mut line = onicode_desktop_link::encode(&request).to_vec();
        line.push(b'\n');
        write
            .write_all(&line)
            .await
            .map_err(|error| error.to_string())?;
        let mut reply = Vec::new();
        let limit = u64::try_from(ctl::MAX_LINE).unwrap_or(u64::MAX);
        let mut read = BufReader::new(read).take(limit);
        read.read_until(b'\n', &mut reply)
            .await
            .map_err(|error| error.to_string())?;
        onicode_desktop_link::decode::<CtlReply>(&reply).map_err(|error| error.to_string())
    };
    tokio::time::timeout(CTL_TIMEOUT, talk)
        .await
        .map_err(|_elapsed| "desktopd did not answer in time".to_owned())?
}

/// Asks launchd to start desktopd, the login item's agent.
async fn kickstart(data_dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    // The data directory is the person's own, so its owner is the GUI session's user: no
    // `getuid` call, no `unsafe`.
    let uid = std::fs::metadata(data_dir)
        .map_err(|error| format!("{}: {error}", data_dir.display()))?
        .uid();
    let app_id = app_bundle_id();
    ensure_registered(&app_id).await?;
    let label = onicode_grants::bundle::helper_id(&app_id);
    let target = format!("gui/{uid}/{label}");
    let out = tokio::process::Command::new("/bin/launchctl")
        .args(["kickstart", &target])
        .output()
        .await
        .map_err(|error| format!("launchctl: {error}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(
        "Onicode Desktop is not set up on this Mac: open Onicode there and allow its login item"
            .into(),
    )
}

/// Registers desktopd's agent with `SMAppService` the first time a desktop is asked of this Mac,
/// so a Mac that never opened the old onboarding still hosts. The engine runs as
/// `Onicode.app/Contents/MacOS/Onicode Engine`, so its main bundle is the app's and the plist in
/// `Contents/Library/LaunchAgents` is found. Registering never prompts; macOS posts its own
/// "background item added" notification. An agent the person turned off in Login Items is left
/// off, and the settings pane is opened at it for the person at this Mac.
async fn ensure_registered(app_id: &str) -> Result<(), String> {
    use onicode_grants::agent::{self, Status};

    let plist = onicode_grants::bundle::agent_plist(app_id);
    let status = tokio::task::spawn_blocking(move || match agent::status(&plist) {
        Status::NotRegistered => agent::register(&plist).map_err(|error| error.to_string()),
        status => Ok(status),
    })
    .await
    .map_err(|error| error.to_string())??;
    match status {
        Status::Enabled => Ok(()),
        Status::RequiresApproval => {
            agent::open_login_items();
            Err("Onicode Desktop is off in Login Items on this Mac: allow it there".into())
        }
        Status::NotFound | Status::NotRegistered => {
            Err("This Onicode has no desktop helper: install a build that includes it".into())
        }
    }
}

/// Opens System Settings at `pane` on this Mac.
fn open_pane(pane: Pane) {
    use onicode_grants::Privilege;
    let privilege = match pane {
        Pane::ScreenRecording => Privilege::ScreenRecording,
        Pane::Accessibility => Privilege::Accessibility,
        Pane::PasteFromOtherApps => Privilege::Pasteboard,
    };
    if !onicode_grants::open_settings(privilege) {
        eprintln!("onicode-engine: System Settings did not open at {privilege:?}");
    }
}

/// The bundle id of the app this engine runs in, which names desktopd's launchd label
/// (`onicode_grants::bundle::helper_id`): the enclosing `Onicode.app`'s, since the engine runs as
/// its `Contents/MacOS/Onicode Engine`. `ONICODE_APP_BUNDLE_ID` names it for an engine run
/// outside a bundle; a bare engine falls back to the release app's.
fn app_bundle_id() -> String {
    const RELEASE: &str = "dev.aislopware.onicode";
    if let Ok(id) = std::env::var("ONICODE_APP_BUNDLE_ID")
        && !id.is_empty()
    {
        return id;
    }
    objc2_foundation::NSBundle::mainBundle()
        .bundleIdentifier()
        .map_or_else(|| RELEASE.to_owned(), |id| id.to_string())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

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

    fn status(grants: HostGrants, addrs: Vec<SocketAddr>) -> DesktopStatus {
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

    fn service(dir: &Path) -> DesktopService {
        DesktopService::new(Some(dir.to_path_buf()), Identity::of(None))
    }

    async fn call<C: Call>(service: &DesktopService, body: C::Params) -> Result<C::Reply, String> {
        let params = serde_json::to_value(onicode_protocol::Routed {
            engine: None,
            op: None,
            body,
        })
        .unwrap();
        match service.handle(C::NAME, params).await {
            Ok(RpcReply::Value(value)) => Ok(serde_json::from_value(value).unwrap()),
            Ok(_) => panic!("{} answered with no value", C::NAME),
            Err(error) => Err(error.to_string()),
        }
    }

    /// A running desktopd is handed back under the engine's own id, with its addresses.
    #[tokio::test]
    async fn a_running_desktopd_is_handed_back_with_the_engine() {
        let dir = tempfile::tempdir().unwrap();
        let addr = "100.64.0.7:7434".parse().unwrap();
        let _desktopd = fake_desktopd(dir.path(), status(GRANTED, vec![addr]));
        let service = service(dir.path());
        let endpoint = call::<OpenDesktop>(&service, OpenDesktop {}).await.unwrap();
        assert_eq!(endpoint.device_id, service.identity.id().0);
        assert_eq!(endpoint.addrs, vec![addr]);
        assert!(endpoint.grants.complete());
    }

    /// A granted desktopd with no tailnet address cannot be dialled: the engine says why.
    #[tokio::test]
    async fn a_granted_desktopd_off_the_tailnet_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let _desktopd = fake_desktopd(dir.path(), status(GRANTED, Vec::new()));
        let error = call::<OpenDesktop>(&service(dir.path()), OpenDesktop {})
            .await
            .unwrap_err();
        assert!(error.contains("tailnet"), "{error}");
    }

    /// A desktopd without its grants is still handed back, for the viewer to name what is off.
    #[tokio::test]
    async fn an_ungranted_desktopd_is_handed_back_to_be_told() {
        let dir = tempfile::tempdir().unwrap();
        let _desktopd = fake_desktopd(dir.path(), status(HostGrants::default(), Vec::new()));
        let service = service(dir.path());
        let endpoint = call::<OpenDesktop>(&service, OpenDesktop {}).await.unwrap();
        assert_eq!(endpoint.grants.missing().len(), 2);
        let reply = call::<DesktopGrants>(&service, DesktopGrants {})
            .await
            .unwrap();
        assert!(reply.running);
    }

    /// An engine in memory has no data directory, so no desktop, and says so.
    #[tokio::test]
    async fn an_engine_in_memory_has_no_desktop() {
        let service = DesktopService::new(None, Identity::of(None));
        let error = call::<OpenDesktop>(&service, OpenDesktop {})
            .await
            .unwrap_err();
        assert!(error.contains("no desktop"), "{error}");
    }
}
