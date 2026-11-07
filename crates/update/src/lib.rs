//! onicode-update — release checking and self-update, shared by the engine (the
//! background checker + `ApplyUpdate`), the CLI (`onicode update`), and the UI
//! (its own report-only checker, the "Check for Updates…" menu item, the
//! sidebar update strip, and the desktop install paths).
//!
//! Release layout: the release feed (`ONICODE_UPDATE_FEED`, or
//! `ONICODE_RELEASES_URL` for its `releases` directory itself) serves
//! `{feed}/releases/*`. `manifest.json` carries the latest version plus a
//! sha256 per artifact; `latest.txt` (version only) remains as the fallback for
//! releases published before the manifest existed.
//!
//! Install kinds and their update paths:
//! - **MacApp** (running out of an app bundle): download the app tarball, swap the bundle
//!   directory, relaunch, driven by the UI; or, asked from another device, the engine's own trial
//!   of it ([`transaction`]).
//! - **Unmanaged** (source builds, hand-copied binaries): report only — the UI's advisory strip
//!   links to [`RELEASES_PAGE`].
//!
//! Desktop installs download as soon as a release is seen and install either
//! on "Restart to update" or when the app next quits, so an app nobody
//! restarts deliberately still lands on the new version at its next launch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, bail};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;

pub mod channel;
pub mod desktop;
pub mod transaction;
pub use channel::Channel;
pub use transaction::{UpdateReport, UpdateState};

/// The version compiled into this binary (the workspace version).
pub const fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

static APP_VERSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// The app's own version when it isn't this crate's: a beta interface built
/// apart from the workspace (`0.2.136-beta.1`). Set once, before the desktop
/// checker starts.
pub fn set_app_version(version: &str) {
    let _ = APP_VERSION.set(version.trim().to_owned());
}

/// The version the desktop app compares releases with: [`set_app_version`]'s,
/// or [`current_version`].
pub fn app_version() -> &'static str {
    APP_VERSION
        .get()
        .map(String::as_str)
        .unwrap_or(current_version())
}

/// The release feed this build reads: `ONICODE_UPDATE_FEED` when it is set
/// (empty turns updates off), else the feed the build was made with. None
/// leaves the updater off (`docs/onicode/decisions/releases.md`).
pub fn feed_url() -> Option<String> {
    feed_from(
        std::env::var("ONICODE_UPDATE_FEED").ok().as_deref(),
        option_env!("ONICODE_UPDATE_FEED"),
    )
}

fn feed_from(runtime: Option<&str>, built: Option<&str>) -> Option<String> {
    runtime
        .or(built)
        .map(str::trim)
        .filter(|feed| !feed.is_empty())
        .map(str::to_owned)
}

/// How far a download has got: the bytes so far, and the whole when the feed
/// says. Called from the download's task, once per chunk.
pub type Progress<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

/// A [`Progress`] for callers that show none.
pub fn no_progress(_done: u64, _total: Option<u64>) {}

/// Successful checks repeat this often. The feed is a sub-kilobyte
/// document, so hourly polling is free and bounds how long a
/// long-running app can sit on a stale release.
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Failed checks (offline boot, captive portal, transient feed error) back off
/// through these delays, then stay at the last one.
const CHECK_RETRY: [Duration; 4] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(15 * 60),
    Duration::from_secs(30 * 60),
];
/// Check deadlines are wall-clock times: a monotonic sleep stops counting
/// while the machine sleeps, so a laptop closed overnight used to push its
/// next check out by the whole night. The loop re-reads the clock this often.
const SCHEDULE_TICK: Duration = Duration::from_secs(60);
/// The engine's first check waits out engine boot (room joins, doc re-sync).
const ENGINE_INITIAL_DELAY: Duration = Duration::from_secs(20);
/// The desktop checker only needs the window on screen first.
const DESKTOP_INITIAL_DELAY: Duration = Duration::from_secs(2);
/// A staged binary must answer `--version` within this window.
const STAGED_VERSION_TIMEOUT: Duration = Duration::from_secs(30);
/// Release metadata is tiny. Bound both its buffered size and total transfer
/// time so a compromised or misconfigured public feed cannot hold a checker
/// forever or make every local install buffer an unbounded response.
const RELEASE_METADATA_MAX_BYTES: usize = 1024 * 1024;
const RELEASE_METADATA_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Release metadata
// ---------------------------------------------------------------------------

/// `{feed}/releases/manifest.json` — written by the release workflow.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    /// Artifact file name → metadata. Empty for pre-manifest releases resolved
    /// via `latest.txt` — downloads then skip checksum verification (with a log).
    #[serde(default)]
    pub files: BTreeMap<String, FileMeta>,
    /// The engine inside the release's app, when it isn't `version`'s own (a
    /// beta app carries the stable engine).
    #[serde(
        default,
        rename = "engineVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub engine_version: Option<String>,
    /// The oldest macOS the release's app runs on, when it's newer than the
    /// running app's floor: a Mac below it is held on its current version.
    #[serde(
        default,
        rename = "minimumMacOS",
        skip_serializing_if = "Option::is_none"
    )]
    pub minimum_macos: Option<String>,
    /// The release's notes, in Markdown: what the app's What's new shows for
    /// `version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The day `version` was released (`YYYY-MM-DD`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// The channel this manifest was fetched from: where its files are.
    #[serde(skip)]
    pub channel: Channel,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileMeta {
    #[serde(default)]
    pub sha256: Option<String>,
}

/// Artifact-name platform pair matching the packaging scripts.
pub fn platform_key() -> (&'static str, &'static str) {
    let os = std::env::consts::OS;
    let arch = match (os, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    };
    (os, arch)
}

fn require_mac_app_update_platform() -> anyhow::Result<()> {
    if std::env::consts::OS != "macos" {
        bail!(
            "macOS app updates are not supported on {}",
            std::env::consts::OS
        );
    }
    Ok(())
}

/// `onicode-<ver>-macos-<arch>-app.tar.gz` — the macOS app update payload.
pub fn mac_app_artifact(version: &str) -> String {
    let (_, arch) = platform_key();
    format!("onicode-{version}-macos-{arch}-app.tar.gz")
}

/// A release version: dotted numbers and, for a prerelease, its semver
/// identifiers (`0.2.136-beta.2` → `[0, 2, 136]`, `["beta", "2"]`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    core: Vec<u64>,
    pre: Vec<PreId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum PreId {
    // Numeric identifiers sort below alphanumeric ones (semver 11.4.3).
    Num(u64),
    Text(String),
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        self.core.cmp(&other.core).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                // A release outranks its prereleases.
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                // Identifier by identifier; a shorter prefix is lower.
                (false, false) => self.pre.cmp(&other.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Dotted numbers with an optional leading `v` and an optional `-prerelease`
/// (`0.2.136-beta.1`). Build metadata (`+…`) and anything else is rejected.
fn parse_version(v: &str) -> Option<Version> {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (v, None),
    };
    let core: Vec<u64> = core
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if core.is_empty() {
        return None;
    }
    let pre = match pre {
        None => Vec::new(),
        Some(pre) => pre
            .split('.')
            .map(|id| {
                if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    None
                } else if id.chars().all(|c| c.is_ascii_digit()) {
                    id.parse().ok().map(PreId::Num)
                } else {
                    Some(PreId::Text(id.to_ascii_lowercase()))
                }
            })
            .collect::<Option<_>>()?,
    };
    Some(Version { core, pre })
}

/// Strictly-newer compare (`0.1.10` > `0.1.9` > `0.1`), semver's for
/// prereleases: `0.2.135` < `0.2.136-beta.1` < `0.2.136-beta.2` < `0.2.136`.
/// Unparsable versions never count as newer — a garbage `latest.txt` must not
/// trigger an update loop.
pub fn version_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// What a release means for the build that's running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// Nothing to install.
    Current,
    /// A newer release on the build's own channel (or a newer beta for a
    /// stable build that chose beta).
    Newer,
    /// The chosen channel's release replaces this build of the other
    /// channel even though it isn't newer: leaving beta for stable.
    Switch,
    /// Not installable here, and why (in words for the person).
    Held(String),
}

impl Offer {
    pub fn installs(&self) -> bool {
        matches!(self, Self::Newer | Self::Switch)
    }
}

/// What `manifest` (the `selected` channel's) offers a build of `running`
/// whose bundle carries the engine `running_engine`, on macOS `macos`.
///
/// - Its channel's release installs when newer.
/// - Choosing beta on a stable build installs a beta only when it's newer: a stable that moved past
///   the last beta waits for the next one.
/// - Choosing stable on a beta build installs stable even when older, unless stable's engine is
///   older than the one the beta brought (it would run on data a newer engine wrote): then it waits
///   until stable catches up.
/// - A release needing a newer macOS than this one is held.
pub fn offer(
    manifest: &Manifest,
    running: &str,
    selected: Channel,
    running_engine: Option<&str>,
    macos: Option<&str>,
) -> Offer {
    let latest = manifest.version.as_str();
    if parse_version(latest).is_none() || latest.trim() == running.trim() {
        return Offer::Current;
    }
    let leaving_beta = Channel::of_version(running) == Channel::Beta && selected == Channel::Stable;
    if !leaving_beta && !version_newer(latest, running) {
        return Offer::Current;
    }
    if let (Some(minimum), Some(system)) = (manifest.minimum_macos.as_deref(), macos)
        && !channel::macos_at_least(system, minimum)
    {
        return Offer::Held(format!(
            "Onicode {latest} needs macOS {minimum} or later. This Mac has macOS {system}."
        ));
    }
    if !leaving_beta {
        return Offer::Newer;
    }
    let target_engine = manifest.engine_version.as_deref().unwrap_or(latest);
    if let Some(engine) = running_engine
        && version_newer(engine, target_engine)
    {
        return Offer::Held(format!(
            "This beta's engine ({engine}) is newer than Onicode {latest}'s. You'll move to Stable once it catches up."
        ));
    }
    if version_newer(latest, running) {
        Offer::Newer
    } else {
        Offer::Switch
    }
}

/// Fetch the newest release metadata: `manifest.json`, falling back to
/// `latest.txt` (version only, no checksums) for pre-manifest releases.
pub async fn fetch_latest(feed_url: &str) -> anyhow::Result<Manifest> {
    fetch_latest_on(feed_url, channel::selected()).await
}

/// [`fetch_latest`] from `channel`'s feed.
pub async fn fetch_latest_on(feed_url: &str, channel: Channel) -> anyhow::Result<Manifest> {
    let mut manifest = fetch_latest_from(&channel.base(&release_base(feed_url)?)).await?;
    manifest.channel = channel;
    Ok(manifest)
}

async fn fetch_latest_from(base: &str) -> anyhow::Result<Manifest> {
    let client = http_client()?;
    let manifest_url = format!("{base}/manifest.json");
    match fetch_release_metadata(&client, &manifest_url).await {
        Ok(response) if response.status.is_success() => {
            let manifest: Manifest =
                serde_json::from_slice(&response.body).context("parsing manifest.json")?;
            if manifest.version.trim().is_empty() {
                bail!("manifest.json has an empty version");
            }
            return Ok(manifest);
        }
        Ok(response) => {
            tracing::debug!(status = %response.status, "manifest.json unavailable; trying latest.txt")
        }
        Err(err) => tracing::debug!(error = %err, "manifest.json fetch failed; trying latest.txt"),
    }
    let latest_url = format!("{base}/latest.txt");
    let response = fetch_release_metadata(&client, &latest_url)
        .await
        .context("fetching latest.txt")?;
    anyhow::ensure!(
        response.status.is_success(),
        "fetching latest.txt: HTTP {}",
        response.status
    );
    let version = std::str::from_utf8(&response.body)
        .context("reading latest.txt as UTF-8")?
        .trim()
        .to_string();
    if version.is_empty() {
        bail!("latest.txt is empty");
    }
    Ok(Manifest {
        version,
        ..Manifest::default()
    })
}

#[derive(Debug)]
struct ReleaseMetadataResponse {
    status: reqwest::StatusCode,
    body: Vec<u8>,
}

async fn fetch_release_metadata(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<ReleaseMetadataResponse> {
    fetch_release_metadata_with_limits(
        client,
        url,
        RELEASE_METADATA_TIMEOUT,
        RELEASE_METADATA_MAX_BYTES,
    )
    .await
}

async fn fetch_release_metadata_with_limits(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
    max_bytes: usize,
) -> anyhow::Result<ReleaseMetadataResponse> {
    let request = async {
        let response = client
            .get(url)
            // Intermediaries must revalidate: a proxy holding an old manifest
            // is exactly the "prompt appears late" failure.
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .send()
            .await
            .with_context(|| format!("fetching {url}"))?;
        let status = response.status();
        if !status.is_success() {
            return Ok(ReleaseMetadataResponse {
                status,
                body: Vec::new(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > max_bytes as u64)
        {
            bail!("release metadata exceeds {max_bytes} bytes");
        }
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading release metadata")?;
            anyhow::ensure!(
                body.len().saturating_add(chunk.len()) <= max_bytes,
                "release metadata exceeds {max_bytes} bytes"
            );
            body.extend_from_slice(&chunk);
        }
        Ok(ReleaseMetadataResponse { status, body })
    };
    tokio::time::timeout(timeout, request)
        .await
        .with_context(|| format!("fetching {url} exceeded the metadata deadline"))?
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    http_client_with_timeouts(Duration::from_secs(15), Duration::from_secs(30))
}

fn http_client_with_timeouts(connect: Duration, read: Duration) -> anyhow::Result<reqwest::Client> {
    // reqwest's rustls carries no crypto provider (`rustls-no-provider`) and takes the process
    // default when the client is built: ring, which the rest of onicode already links. An `Err`
    // only says a default is installed already.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(connect)
        // Inactivity timeout, not a total download cap: slow progressing
        // updates remain viable on constrained links.
        .read_timeout(read)
        .user_agent(concat!("onicode/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("too many update redirects");
            }
            if attempt.previous().iter().any(|url| url.scheme() == "https")
                && attempt.url().scheme() != "https"
            {
                return attempt.error("update redirect would downgrade HTTPS");
            }
            attempt.follow()
        }))
        .build()
        .context("building http client")
}

fn validate_release_override(value: &str) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(value.trim()).context("invalid update feed URL")?;
    // A test install's local feed, in a debug build only: release builds can't be
    // pointed anywhere but HTTPS.
    let local_test_feed =
        cfg!(debug_assertions) && url.scheme() == "http" && url.host_str() == Some("127.0.0.1");
    anyhow::ensure!(
        local_test_feed || (url.scheme() == "https" && url.host_str().is_some()),
        "update feed must use HTTPS"
    );
    anyhow::ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "update feed must be a base URL without credentials, query, or fragment"
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// The project's GitHub releases page — the advisory update strip opens this
/// for unmanaged installs (source builds, hand-copied binaries), where no
/// updater flow exists to drive.
pub const RELEASES_PAGE: &str = "https://github.com/aislopware/onicode/releases";

/// The newest release's page — the download destination offered when this
/// installation cannot replace itself.
pub const LATEST_RELEASE_PAGE: &str = "https://github.com/aislopware/onicode/releases/latest";

fn release_base(feed_url: &str) -> anyhow::Result<String> {
    if let Ok(url) = std::env::var("ONICODE_RELEASES_URL")
        && !url.trim().is_empty()
    {
        return validate_release_override(&url);
    }
    let feed = feed_url.trim_end_matches('/');
    anyhow::ensure!(!feed.is_empty(), "no release feed is configured");
    Ok(format!("{feed}/releases"))
}

// ---------------------------------------------------------------------------
// Install-kind detection
// ---------------------------------------------------------------------------

/// How this binary was installed — decides the update path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    /// Running out of a macOS `.app` bundle.
    MacApp { bundle: PathBuf },
    /// Source build or hand-copied binary — updates are report-only.
    Unmanaged,
}

/// Why an installation that normally updates itself cannot right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateBlocker {
    /// macOS App Translocation ran the app from a randomized read-only path
    /// (launched from Downloads or a disk image without being moved first).
    Translocated,
    /// Running straight from the mounted disk image.
    DiskImage,
    /// The directory holding the installation is not writable by this user.
    NotWritable(PathBuf),
}

impl std::fmt::Display for UpdateBlocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Translocated | Self::DiskImage => f.write_str(
                "Onicode is running from a temporary, read-only location. Move Onicode to your \
                 Applications folder and reopen it to turn on updates.",
            ),
            Self::NotWritable(dir) => write!(
                f,
                "Onicode doesn't have permission to replace itself in {}.",
                dir.display()
            ),
        }
    }
}

impl InstallKind {
    /// Whether the desktop app downloads and installs updates itself.
    pub fn supports_desktop_update(&self) -> bool {
        match self {
            Self::MacApp { .. } => true,
            Self::Unmanaged => false,
        }
    }

    /// Why this installation cannot replace itself, when it can't. Checked
    /// before downloading so the UI can explain the fix instead of failing
    /// after a wasted download with a raw rename error.
    pub fn desktop_update_blocker(&self) -> Option<UpdateBlocker> {
        match self {
            Self::MacApp { bundle } => mac_bundle_blocker(bundle, dir_writable),
            Self::Unmanaged => None,
        }
    }

    pub async fn stage_desktop(
        &self,
        feed_url: &str,
        manifest: &Manifest,
        data_dir: &Path,
        progress: Progress<'_>,
    ) -> anyhow::Result<PathBuf> {
        match self {
            Self::MacApp { .. } => stage_mac_app(feed_url, manifest, data_dir, progress).await,
            Self::Unmanaged => bail!("this installation does not support desktop updates"),
        }
    }

    /// Install a staged update. With `relaunch`, the new version starts once
    /// this process exits ("Restart to update"); without it the update takes
    /// effect at the next launch ("install on quit"). Either way the caller
    /// must quit after this succeeds.
    pub fn apply_desktop(&self, staged: &Path, relaunch: bool) -> anyhow::Result<()> {
        // The engine is trying a new version out (asked from another device), and
        // going back from that needs the installed app to stay as it was.
        if channel::data_dir()
            .and_then(transaction::read)
            .is_some_and(|update| update.in_progress())
        {
            bail!("An update is already in progress on this device.");
        }
        match self {
            Self::MacApp { bundle } => {
                apply_mac_app(staged, bundle)?;
                // The swap copied the staged bundle; the cache is spent.
                if let Some(version_dir) = staged.parent()
                    && !process_runs_from(version_dir)
                {
                    let _ = std::fs::remove_dir_all(version_dir);
                }
                if relaunch {
                    relaunch_after_exit(Path::new("/usr/bin/open"), bundle);
                }
                Ok(())
            }
            Self::Unmanaged => bail!("this installation does not support desktop updates"),
        }
    }
}

fn mac_bundle_blocker(bundle: &Path, writable: impl Fn(&Path) -> bool) -> Option<UpdateBlocker> {
    if bundle
        .components()
        .any(|part| part.as_os_str() == "AppTranslocation")
    {
        return Some(UpdateBlocker::Translocated);
    }
    let parent = bundle.parent()?;
    if writable(parent) {
        return None;
    }
    if bundle.starts_with("/Volumes") {
        return Some(UpdateBlocker::DiskImage);
    }
    Some(UpdateBlocker::NotWritable(parent.to_path_buf()))
}

/// Probe by creating (and removing) a file: permission bits alone miss
/// read-only mounts, ACLs, and sandboxed locations.
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".onicode-write-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
            true
        }
        // A leftover probe from a crashed run still proves writability.
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// How this copy of Onicode is installed, as `UpdateStatus` reports it. A client
/// shows what the device can do from this and `can_apply_remotely`, rather
/// than guessing from its platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallMethod {
    /// The macOS app.
    MacApp,
    /// Built from source or copied by hand.
    Unmanaged,
    /// A newer engine's method this client doesn't know.
    #[serde(other)]
    Unknown,
}

/// What a device can do about its own updates, for `UpdateStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAbility {
    pub install: InstallMethod,
    /// `ApplyUpdate` works from another device.
    pub can_apply_remotely: bool,
    /// Why it doesn't, in words for the person.
    pub cannot_apply_reason: Option<String>,
}

impl UpdateAbility {
    fn refused(install: InstallMethod, reason: impl Into<String>) -> Self {
        Self {
            install,
            can_apply_remotely: false,
            cannot_apply_reason: Some(reason.into()),
        }
    }

    /// `supervised`: something starts this engine again once it stops (see
    /// [`running_as_installed_service`]). `blocker`: why the installation can't
    /// replace itself right now.
    fn decide(kind: &InstallKind, supervised: bool, blocker: Option<UpdateBlocker>) -> Self {
        match kind {
            // The app's background engine can carry out an update itself. One
            // run any other way has nothing to bring it back.
            InstallKind::MacApp { .. } => Self::desktop(
                InstallMethod::MacApp,
                "This Onicode wasn't started by the app, so nothing would bring it back after an \
                 update. Update it from the Onicode app on that Mac.",
                supervised,
                blocker,
            ),
            InstallKind::Unmanaged => Self::refused(
                InstallMethod::Unmanaged,
                "This copy of Onicode wasn't installed by the installer, so it can't replace itself.",
            ),
        }
    }

    fn desktop(
        install: InstallMethod,
        unsupervised: &str,
        supervised: bool,
        blocker: Option<UpdateBlocker>,
    ) -> Self {
        match blocker {
            _ if !supervised => Self::refused(install, unsupervised),
            Some(blocker) => Self::refused(install, blocker.to_string()),
            None => Self {
                install,
                can_apply_remotely: true,
                cannot_apply_reason: None,
            },
        }
    }

    /// This process's: how it's installed, and whether it can update from afar.
    pub fn current() -> Self {
        let kind = detect_install();
        Self::decide(
            &kind,
            running_as_installed_service(),
            kind.desktop_update_blocker(),
        )
    }
}

/// Set on a new version on trial: the installation it's running in place of
/// (the macOS app bundle).
pub const INSTALLED_AT_ENV: &str = "ONICODE_INSTALLED_AT";

/// Where this process was installed from, resolved once: an update moves
/// the running image aside, and a long-running engine must still recognise
/// the installation it belongs to.
pub fn detect_install() -> InstallKind {
    static INSTALL: std::sync::OnceLock<InstallKind> = std::sync::OnceLock::new();
    INSTALL
        .get_or_init(|| {
            // A new version on trial runs from beside the installation it's
            // going to become.
            if let Some(installed) = std::env::var_os(INSTALLED_AT_ENV).map(PathBuf::from) {
                return InstallKind::MacApp { bundle: installed };
            }
            let Ok(exe) = std::env::current_exe() else {
                return InstallKind::Unmanaged;
            };
            detect_install_from(&exe)
        })
        .clone()
}

fn detect_install_from(exe: &Path) -> InstallKind {
    for ancestor in exe.ancestors() {
        if ancestor.extension().is_some_and(|ext| ext == "app")
            && exe.starts_with(ancestor.join("Contents").join("MacOS"))
        {
            return InstallKind::MacApp {
                bundle: ancestor.to_path_buf(),
            };
        }
    }
    InstallKind::Unmanaged
}

/// The version currently installed on disk for `kind`, read without executing
/// anything. A long-running process compares it with its own version to notice
/// that something else (the desktop app) has already replaced its binary.
pub fn installed_version(kind: &InstallKind) -> Option<String> {
    let version = match kind {
        InstallKind::MacApp { bundle } => bundle_engine_version(bundle)?,
        InstallKind::Unmanaged => return None,
    };
    parse_version(&version).map(|_| version)
}

/// The engine a macOS app bundle carries: its Info.plist's `OnicodeEngineVersion`
/// (packaging writes it; a beta app's engine is stable's, older than the app),
/// else `CFBundleShortVersionString` (bundles from before the key, whose
/// engine was always the app's own version). An engine compares this with
/// its own to restart into a newer one; comparing the app's version would
/// restart a beta's engine forever.
pub fn bundle_engine_version(bundle: &Path) -> Option<String> {
    let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).ok()?;
    plist_string(&plist, "OnicodeEngineVersion")
        .or_else(|| plist_string(&plist, "CFBundleShortVersionString"))
}

/// The app's own version in a macOS bundle (a beta's isn't its engine's).
pub fn bundle_app_version(bundle: &Path) -> Option<String> {
    let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).ok()?;
    plist_string(&plist, "CFBundleShortVersionString")
}

/// A string value from an XML property list (the packaging template;
/// codesign leaves the format alone).
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after_key = plist.split(&format!("<key>{key}</key>")).nth(1)?;
    let value = after_key.trim_start().strip_prefix("<string>")?;
    let end = value.find("</string>")?;
    Some(value[..end].trim().to_owned()).filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// Download + verify
// ---------------------------------------------------------------------------

/// Stream `{feed}/releases/<file>` to `dest`, verifying the manifest sha256 when
/// present. Writes through a `.partial` sidecar so an interrupted download never
/// leaves a plausible-looking artifact behind.
pub async fn download_release_file(
    feed_url: &str,
    manifest: &Manifest,
    file: &str,
    dest: &Path,
    progress: Progress<'_>,
) -> anyhow::Result<()> {
    // From the feed the manifest came from: a beta's files are beta's.
    let url = format!("{}/{file}", manifest.channel.base(&release_base(feed_url)?));
    let expected = manifest.files.get(file).and_then(|m| m.sha256.as_deref());
    if expected.is_none() {
        tracing::warn!(
            file,
            "no checksum in release metadata; skipping verification"
        );
    }
    let partial = dest.with_extension("partial");
    let resp = http_client()?
        .get(&url)
        .send()
        .await
        .with_context(|| format!("downloading {url}"))?
        .error_for_status()
        .with_context(|| format!("downloading {url}"))?;
    let total = resp.content_length();
    let mut out = tokio::fs::File::create(&partial)
        .await
        .with_context(|| format!("creating {}", partial.display()))?;
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    let mut done = 0_u64;
    progress(done, total);
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading download stream")?;
        hasher.update(&chunk);
        out.write_all(&chunk).await.context("writing download")?;
        done = done.saturating_add(chunk.len() as u64);
        progress(done, total);
    }
    out.flush().await.context("flushing download")?;
    drop(out);
    if let Some(expected) = expected {
        let actual = base16ct::lower::encode_string(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected.trim()) {
            tokio::fs::remove_file(&partial).await.ok();
            bail!("checksum mismatch for {file}: expected {expected}, got {actual}");
        }
    }
    tokio::fs::rename(&partial, dest)
        .await
        .with_context(|| format!("moving {} into place", dest.display()))?;
    Ok(())
}

fn run(program: &str, args: &[&str]) -> anyhow::Result<()> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Run a freshly unpacked binary's `--version` before it can replace anything:
/// proves the payload is complete, executable on this machine, and the release
/// the manifest promised.
async fn verify_staged_binary(binary: &Path, version: &str) -> anyhow::Result<()> {
    let output = tokio::time::timeout(
        STAGED_VERSION_TIMEOUT,
        tokio::process::Command::new(binary)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("staged binary version check timed out")?
    .with_context(|| format!("running {} --version", binary.display()))?;
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    anyhow::ensure!(
        output.status.success() && reported == format!("onicode {version}"),
        "staged binary reported {reported:?} (exit {}), expected \"onicode {version}\"",
        output.status
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// The engine's restart
// ---------------------------------------------------------------------------

type ManagedRestart = Box<dyn Fn() -> anyhow::Result<()> + Send + Sync>;

static MANAGED_RESTART: std::sync::OnceLock<ManagedRestart> = std::sync::OnceLock::new();

/// This process is the desktop app's background engine, which no service
/// manager supervises: `restart` relaunches it (the replacement waits for
/// this one to let go of the data directory) and stops this one. With it set,
/// [`Updater::restart_when_superseded`] restarts into a newer installed
/// version once nothing is running, exactly as for an installed service.
pub fn set_managed_restart(restart: impl Fn() -> anyhow::Result<()> + Send + Sync + 'static) {
    let _ = MANAGED_RESTART.set(Box::new(restart));
}

type ManagedRestartReady = Box<dyn Fn() -> bool + Send + Sync>;
static MANAGED_RESTART_READY: std::sync::OnceLock<ManagedRestartReady> = std::sync::OnceLock::new();

/// When the background engine may restart into a newer installed version,
/// on top of nothing running (e.g. only while the app that starts it runs).
/// Checked on every pass, so a restart it holds back happens on a later one.
pub fn set_managed_restart_ready(ready: impl Fn() -> bool + Send + Sync + 'static) {
    let _ = MANAGED_RESTART_READY.set(Box::new(ready));
}

fn managed_restart_ready() -> bool {
    MANAGED_RESTART_READY.get().is_none_or(|ready| ready())
}

/// Whether this process is the engine service [`restart_service`] manages:
/// the desktop app's background engine ([`set_managed_restart`]), or the job
/// launchd names in `XPC_SERVICE_NAME`.
pub fn running_as_installed_service() -> bool {
    MANAGED_RESTART.get().is_some()
        || std::env::var("XPC_SERVICE_NAME").is_ok_and(|label| label == "dev.aislopware.onicode")
}

/// The systemd unit this engine runs as. `ONICODE_SERVICE_UNIT` names another, so
/// a test install beside a real one can never restart the real one.
pub fn service_unit() -> String {
    std::env::var("ONICODE_SERVICE_UNIT")
        .ok()
        .filter(|unit| !unit.trim().is_empty())
        .unwrap_or_else(|| "onicode.service".to_owned())
}

/// Restart the installed engine: the desktop app's background engine through
/// [`set_managed_restart`], a launchd job through `launchctl`. Called once a
/// newer version is in place. Only queues the restart: the caller may be the
/// service itself, which must stay responsive to the stop signal that follows.
pub fn restart_service() -> anyhow::Result<()> {
    if let Some(restart) = MANAGED_RESTART.get() {
        return restart();
    }
    let output = std::process::Command::new("id").arg("-u").output()?;
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    run(
        "launchctl",
        &[
            "kickstart",
            "-k",
            &format!("gui/{uid}/dev.aislopware.onicode"),
        ],
    )
}

// ---------------------------------------------------------------------------
// macOS app-bundle installs — the desktop path
// ---------------------------------------------------------------------------

/// Download + unpack the app tarball into `{data_dir}/updates/<ver>/Onicode.app`
/// (idempotent). The bundle is unpacked beside its final name and renamed in
/// only after its binary answers with the expected version, so an interrupted
/// unpack can never be mistaken for a staged update. Returns the staged bundle.
pub async fn stage_mac_app(
    feed_url: &str,
    manifest: &Manifest,
    data_dir: &Path,
    progress: Progress<'_>,
) -> anyhow::Result<PathBuf> {
    // Reject unsupported targets before creating a stage or making a request.
    require_mac_app_update_platform()?;
    let version = &manifest.version;
    anyhow::ensure!(
        parse_version(version).is_some(),
        "invalid release version {version:?}"
    );
    let updates = data_dir.join("updates");
    let dir = updates.join(version);
    let staged = dir.join("Onicode.app");
    let staged_binary = staged.join("Contents/MacOS/onicode");
    if staged_binary.exists() && verify_staged_binary(&staged_binary, version).await.is_ok() {
        return Ok(staged);
    }
    prune_stale_stages(&updates, version);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = mac_app_artifact(version);
    let tarball = dir.join(&file);
    download_release_file(feed_url, manifest, &file, &tarball, progress).await?;
    let unpack = dir.join(".unpack");
    std::fs::create_dir_all(&unpack)?;
    let unpacked = run(
        "tar",
        &[
            "-xzf",
            &tarball.to_string_lossy(),
            "-C",
            &unpack.to_string_lossy(),
        ],
    )
    .map(|()| unpack.join("Onicode.app"));
    std::fs::remove_file(&tarball).ok();
    let unpacked = unpacked?;
    let unpacked_binary = unpacked.join("Contents/MacOS/onicode");
    if !unpacked_binary.exists() {
        let _ = std::fs::remove_dir_all(&dir);
        bail!("app tarball {file} did not contain Onicode.app");
    }
    if let Err(err) = verify_staged_binary(&unpacked_binary, version).await {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(err);
    }
    std::fs::rename(&unpacked, &staged)
        .with_context(|| format!("moving {} into place", staged.display()))?;
    let _ = std::fs::remove_dir_all(&unpack);
    Ok(staged)
}

/// Staged bundles of other versions are dead weight (each is a whole app):
/// drop them whenever a new version stages.
fn prune_stale_stages(updates: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(updates) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() != keep && !process_runs_from(&entry.path()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether any process runs an executable from under `dir`. A bundle that's still
/// in use mustn't be deleted: with its image gone, AppleSystemPolicy rejects every
/// process it forks, thousands of times a minute (#1037). Can't tell: yes.
pub fn process_runs_from(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut pids = vec![0 as libc::pid_t; 8192];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: the buffer is `bytes` long; the call writes at most that.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if count <= 0 {
        return true;
    }
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    pids.iter()
        .take(count as usize)
        .filter(|pid| **pid > 0)
        .any(|&pid| {
            // SAFETY: `path` is PROC_PIDPATHINFO_MAXSIZE bytes, as the call needs.
            let len =
                unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
            len > 0
                && Path::new(std::ffi::OsStr::from_bytes(&path[..len as usize])).starts_with(&dir)
        })
}

/// Bundles an update set aside (`.Onicode.app.old-…`) or left half-installed
/// (`.Onicode.app.new-…`), once nothing runs from them.
pub fn prune_retired_mac_bundles(bundle: &Path) {
    let (Some(parent), Some(name)) = (bundle.parent(), bundle.file_name()) else {
        return;
    };
    let name = name.to_string_lossy();
    let (old, new) = (format!(".{name}.old-"), format!(".{name}.new-"));
    // An update waiting to be tried needs its copy.
    let waiting = channel::data_dir()
        .and_then(transaction::read)
        .filter(|update| update.in_progress())
        .map(|update| {
            let transaction::Layout::MacBundle { incoming, .. } = update.layout;
            incoming
        });
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if (file_name.starts_with(&old) || file_name.starts_with(&new))
            && waiting.as_deref() != Some(entry.path().as_path())
            && !process_runs_from(&entry.path())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Exchange two bundles in one step: `incoming` becomes `bundle` and the old one
/// is left under the retired name. Nothing ever finds `bundle` missing.
pub(crate) fn swap_mac_bundles(incoming: &Path, bundle: &Path) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = |path: &Path| std::ffi::CString::new(path.as_os_str().as_bytes());
    let (from, to) = (c(incoming)?, c(bundle)?);
    // SAFETY: both are NUL-terminated paths.
    if unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_SWAP) } != 0 {
        return Err(std::io::Error::last_os_error()).context("swapping in the new app");
    }
    let parent = bundle
        .parent()
        .context("app bundle has no parent directory")?;
    let name = bundle
        .file_name()
        .context("app bundle has no name")?
        .to_string_lossy();
    let retired = parent.join(format!(".{name}.old-{}", std::process::id()));
    // If this is lost to a crash, it's tidied up under the name it has.
    let _ = std::fs::rename(incoming, &retired);
    Ok(())
}

/// Swap the installed bundle for the staged one: `ditto` the staged copy next to
/// the target (metadata-preserving, cross-volume safe), then two renames — the
/// old bundle is restored if the second rename fails.
pub fn apply_mac_app(staged: &Path, bundle: &Path) -> anyhow::Result<()> {
    require_mac_app_update_platform()?;
    let parent = bundle
        .parent()
        .context("app bundle has no parent directory")?;
    let name = bundle
        .file_name()
        .context("app bundle has no name")?
        .to_string_lossy();
    let pid = std::process::id();
    let fresh = parent.join(format!(".{name}.new-{pid}"));
    let old = parent.join(format!(".{name}.old-{pid}"));
    let _ = std::fs::remove_dir_all(&fresh);
    if let Err(err) = run(
        "ditto",
        &[&staged.to_string_lossy(), &fresh.to_string_lossy()],
    ) {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(bundle, &old) {
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(err).context("moving the current app aside");
    }
    if let Err(err) = std::fs::rename(&fresh, bundle) {
        let _ = std::fs::rename(&old, bundle);
        let _ = std::fs::remove_dir_all(&fresh);
        return Err(err).context("installing the new app bundle");
    }
    // Not deleted: the background engine, this very app, or an update's trial may
    // still be running from it. Gone once nothing is (#1037).
    prune_retired_mac_bundles(bundle);
    Ok(())
}

/// Detached relauncher: waits for THIS process to exit, then runs `program`
/// (with `argument` unless empty). Launching before exit would race the
/// single-instance engine lock and the IPC port. Paths travel as positional
/// parameters, never spliced into the script. The caller quits after this.
pub fn relaunch_after_exit(program: &Path, argument: &Path) {
    use std::os::unix::process::CommandExt as _;
    let pid = std::process::id().to_string();
    let script = r#"while /bin/kill -0 "$1" 2>/dev/null; do sleep 0.2; done
if [ -n "$3" ]; then exec "$2" "$3"; else exec "$2"; fi"#;
    let mut command = std::process::Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .arg("onicode-relaunch")
        .arg(&pid)
        .arg(program)
        .arg(argument)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    if let Err(err) = command.spawn() {
        tracing::error!(error = %err, "failed to spawn the relauncher");
    }
}

// ---------------------------------------------------------------------------
// Background checker
// ---------------------------------------------------------------------------

/// What the checker reports (the engine's `UpdateStatus` stream, the desktop
/// strip). Version facts only — download/apply progress is owned by whoever
/// drives the update (UI or CLI).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub update_available: bool,
    /// Epoch ms of the last successful check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The channel the check followed (`stable` | `beta`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// `latest_version` exists but can't be installed here, and why (an
    /// older macOS; a beta's engine newer than stable's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<String>,
    /// Installing `latest_version` leaves this build's channel and may go
    /// back a version (beta → stable).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub switch: bool,
    /// How this copy is installed. Absent from older engines and from the
    /// desktop app's own checker: unknown, so don't guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<InstallMethod>,
    /// Whether another device can install the update (`ApplyUpdate`). Absent
    /// from older engines: they don't say. An older engine refuses
    /// `ApplyUpdate` itself when it can't, so treat absent as "try, and show
    /// its refusal", never as a promise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_apply_remotely: Option<bool>,
    /// Why it can't, in words for the person.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cannot_apply_reason: Option<String>,
    /// The latest update asked of this engine and how it went. Match its `id`
    /// with `ApplyUpdate`'s `updateId`: reconnecting doesn't say whether the
    /// update worked or went back. Absent from older engines, which only have
    /// `currentVersion` to go by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateReport>,
    /// On a macOS app install, the installed app's version, which is what
    /// `latestVersion` is compared with. It isn't `currentVersion` on a beta,
    /// whose engine is stable's. Absent elsewhere and from older engines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
}

impl UpdateStatus {
    fn initial(version: &str) -> Self {
        Self {
            current_version: version.to_string(),
            latest_version: None,
            update_available: false,
            checked_at: None,
            error: None,
            channel: None,
            held: None,
            switch: false,
            install: None,
            can_apply_remotely: None,
            cannot_apply_reason: None,
            update: None,
            app_version: None,
        }
    }

    fn with_ability(mut self, ability: UpdateAbility) -> Self {
        self.install = Some(ability.install);
        self.can_apply_remotely = Some(ability.can_apply_remotely);
        self.cannot_apply_reason = ability.cannot_apply_reason;
        self
    }
}

/// `ONICODE_AUTO_UPDATE=0|false|no` — the desktop app then only reports: no
/// background download and no install on quit. Unset means on.
pub fn desktop_auto_update_enabled() -> bool {
    std::env::var("ONICODE_AUTO_UPDATE")
        .map(|v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no"))
        .unwrap_or(true)
}

/// "Nothing would be interrupted by a restart right now" — wired by the engine
/// to its live-run and open-terminal registries. `None` = no gate.
pub type QuiescentCheck = Arc<dyn Fn() -> bool + Send + Sync>;

/// Who runs the checker — decides what it may do beyond reporting.
enum Role {
    /// The engine: a service daemon restarts into a binary someone else installed.
    Engine { quiescent: Option<QuiescentCheck> },
    /// The desktop app: report only — downloading and installing are UI
    /// decisions (see `crates/app/src/notifications/updater.rs`).
    Desktop,
}

/// Background release checker: polls `{feed}/releases` hourly on a wall-clock
/// schedule (so sleep/wake cannot stretch it), backs off on failure, and
/// publishes [`UpdateStatus`] over a watch channel. In the engine, a service
/// restarts into a newer installed version only in a quiet window: while
/// `quiescent` reports activity, the restart defers.
#[derive(Clone)]
pub struct Updater {
    feed_url: String,
    role: Arc<Role>,
    /// The version releases are compared with: the engine's own, or the app's
    /// ([`app_version`]).
    version: Arc<str>,
    status_tx: Arc<watch::Sender<UpdateStatus>>,
    /// Wakes the loop; `forced` says whether the wake demands a check or only
    /// a fresh look at the wall clock.
    wake_tx: Arc<watch::Sender<u64>>,
    forced: Arc<AtomicBool>,
    /// Set by the engine (`onicode-engine`): this process is the installed service
    /// and may restart itself into a newer installed binary.
    service: Arc<AtomicBool>,
    /// Flips to true exactly once; the check loop selects against it so
    /// cancellation lands at any await point (no tokio-util in this crate).
    shutdown_tx: Arc<watch::Sender<bool>>,
    check_task: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Updater {
    /// Spawn the engine's check loop (must run on a tokio runtime).
    pub fn spawn(feed_url: String, quiescent: Option<QuiescentCheck>) -> Self {
        Self::spawn_role(feed_url, Role::Engine { quiescent })
    }

    /// Spawn the desktop app's report-only check loop (must run on a tokio
    /// runtime).
    pub fn spawn_desktop(feed_url: String) -> Self {
        Self::spawn_role(feed_url, Role::Desktop)
    }

    fn spawn_role(feed_url: String, role: Role) -> Self {
        let initial_delay = match role {
            Role::Engine { .. } => ENGINE_INITIAL_DELAY,
            Role::Desktop => DESKTOP_INITIAL_DELAY,
        };
        let version: Arc<str> = match role {
            Role::Engine { .. } => current_version().into(),
            Role::Desktop => app_version().into(),
        };
        let initial = UpdateStatus::initial(&version);
        let initial = match role {
            Role::Engine { .. } => UpdateStatus {
                update: channel::data_dir().and_then(transaction::report),
                app_version: installed_app_version(),
                ..initial.with_ability(UpdateAbility::current())
            },
            Role::Desktop => initial,
        };
        let (status_tx, _) = watch::channel(initial);
        let (wake_tx, _) = watch::channel(0);
        let (shutdown_tx, _) = watch::channel(false);
        let updater = Self {
            feed_url,
            role: Arc::new(role),
            version,
            status_tx: Arc::new(status_tx),
            wake_tx: Arc::new(wake_tx),
            forced: Arc::new(AtomicBool::new(false)),
            service: Arc::new(AtomicBool::new(false)),
            shutdown_tx: Arc::new(shutdown_tx),
            check_task: Arc::new(std::sync::Mutex::new(None)),
        };
        // Subscribe before spawning so an immediate `check_now` cannot land
        // before the background task first polls and be lost behind the
        // initial delay.
        let wakes = updater.wake_tx.subscribe();
        let for_loop = updater.clone();
        let task = tokio::spawn(async move { for_loop.check_loop(wakes, initial_delay).await });
        *updater.check_task.lock().unwrap() = Some(task);
        updater
    }

    /// Stop the check loop and wait for it to exit — a replaced runtime must
    /// not keep polling `{feed}/releases` (or auto-applying) in the background.
    /// Idempotent, and callable from any clone.
    pub async fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        let task = self
            .check_task
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }

    pub fn watch(&self) -> watch::Receiver<UpdateStatus> {
        self.status_tx.subscribe()
    }

    /// Check immediately, for example when authentication recovers after the
    /// process started offline.
    pub fn check_now(&self) {
        self.forced.store(true, Ordering::SeqCst);
        self.wake_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    /// Re-read the wall clock now (window activation, system wake): checks
    /// only if one is due, so frequent pokes cost nothing.
    pub fn poke(&self) {
        self.wake_tx
            .send_modify(|epoch| *epoch = epoch.wrapping_add(1));
    }

    /// Called by the engine (`onicode-engine`): when this process is the installed
    /// engine service and something else installs a newer binary, restart into it
    /// at the next quiet moment. A hand-started `onicode-engine` is left alone —
    /// restarting the service unit would start a second engine beside it.
    pub fn restart_when_superseded(&self) {
        if !running_as_installed_service() {
            return;
        }
        self.service.store(true, Ordering::SeqCst);
        self.poke();
    }

    /// One user-requested check, awaited: publishes the result like a
    /// scheduled check and returns it.
    pub async fn check(&self) -> anyhow::Result<UpdateStatus> {
        match fetch_latest(&self.feed_url).await {
            Ok(manifest) => {
                self.publish(manifest);
                Ok(self.status_tx.borrow().clone())
            }
            Err(err) => {
                self.status_tx
                    .send_modify(|s| s.error = Some(format!("{err:#}")));
                Err(err)
            }
        }
    }

    fn quiescent_now(&self) -> bool {
        match &*self.role {
            Role::Engine { quiescent } => quiescent.as_ref().is_none_or(|check| check()),
            Role::Desktop => true,
        }
    }

    async fn check_loop(&self, mut wakes: watch::Receiver<u64>, initial_delay: Duration) {
        let mut shutdown = self.shutdown_tx.subscribe();
        // Shutdown must cut the loop at ANY await point — including mid
        // `check_once()` / `auto_apply_when_idle()` HTTP — so the whole body
        // races the flag rather than checking it between iterations.
        tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => {}
            _ = async {
                tokio::select! {
                    _ = tokio::time::sleep(initial_delay) => {}
                    _ = wakes.changed() => {}
                }
                let mut schedule = Schedule::default();
                let mut superseded = SupersededRestart::default();
                loop {
                    let forced = self.forced.swap(false, Ordering::SeqCst);
                    if forced || schedule.due(SystemTime::now()) {
                        let ok = self.check_once().await;
                        schedule.record(SystemTime::now(), ok);
                    }
                    if self.service.load(Ordering::SeqCst) {
                        superseded.poll(|| self.quiescent_now());
                    }
                    let wait = schedule.wait(SystemTime::now());
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = wakes.changed() => {}
                    }
                }
            } => {}
        }
    }

    fn publish(&self, manifest: Manifest) {
        // A macOS app's bundle carries the engine a switch must not go below, and
        // its own version is what releases are compared with: a beta's engine is
        // stable's, so by the engine's version a newer beta looks like nothing new.
        let bundle = match detect_install() {
            InstallKind::MacApp { bundle } => Some(bundle),
            _ => None,
        };
        let running_engine = bundle.as_deref().and_then(bundle_engine_version);
        let app_version = bundle.as_deref().and_then(bundle_app_version);
        let running = match &*self.role {
            Role::Engine { .. } => app_version.as_deref().unwrap_or(&self.version),
            Role::Desktop => &self.version,
        };
        let offer = offer(
            &manifest,
            running,
            manifest.channel,
            running_engine.as_deref(),
            channel::macos_version(),
        );
        let status = UpdateStatus {
            current_version: self.version.to_string(),
            update_available: offer.installs(),
            latest_version: Some(manifest.version.clone()),
            checked_at: Some(now_ms()),
            error: None,
            channel: Some(manifest.channel.as_str().to_owned()),
            held: match &offer {
                Offer::Held(why) => Some(why.clone()),
                _ => None,
            },
            switch: offer == Offer::Switch,
            install: None,
            can_apply_remotely: None,
            cannot_apply_reason: None,
            update: None,
            app_version: None,
        };
        let status = match &*self.role {
            Role::Engine { .. } => UpdateStatus {
                update: channel::data_dir().and_then(transaction::report),
                app_version,
                ..status.with_ability(UpdateAbility::current())
            },
            Role::Desktop => status,
        };
        if status.update_available {
            tracing::info!(
                latest = status.latest_version.as_deref().unwrap_or(""),
                current = %status.current_version,
                "update available"
            );
        }
        self.status_tx.send_replace(status);
    }

    /// One check; returns false on fetch failure (retry sooner).
    async fn check_once(&self) -> bool {
        if let InstallKind::MacApp { bundle } = detect_install() {
            prune_retired_mac_bundles(&bundle);
        }
        match fetch_latest(&self.feed_url).await {
            Ok(manifest) => {
                self.publish(manifest);
                true
            }
            Err(err) => {
                tracing::debug!(error = %err, "update check failed");
                self.status_tx
                    .send_modify(|s| s.error = Some(format!("{err:#}")));
                false
            }
        }
    }

    /// Publish the update's latest state, after something settled it.
    pub fn refresh_update_report(&self) {
        let update = channel::data_dir().and_then(transaction::report);
        let app_version = installed_app_version();
        self.status_tx.send_modify(|status| {
            status.update = update;
            status.current_version = self.version.to_string();
            status.app_version = app_version;
        });
    }

    /// Stage the newest release on THIS device and restart into a trial of it.
    /// The answer goes out first: the restart waits a moment so that it can.
    /// What became of the update is `UpdateStatus`'s `update`, under the id
    /// answered here.
    pub async fn apply(&self) -> anyhow::Result<AppliedUpdate> {
        let ability = UpdateAbility::current();
        if !ability.can_apply_remotely {
            bail!("{}", ability.cannot_apply_reason.unwrap_or_default());
        }
        let data_dir = channel::data_dir()
            .context("this engine has no data directory")?
            .to_path_buf();
        if transaction::read(&data_dir).is_some_and(|last| last.in_progress()) {
            bail!("An update is already in progress on this device.");
        }
        let manifest = fetch_latest(&self.feed_url).await?;
        let Some(prepared) = prepare_this_install(&self.feed_url, &manifest, &data_dir).await?
        else {
            bail!("already up to date ({})", current_version());
        };
        // Already on disk as `current`: there's nothing to try out, only to
        // restart into.
        let update_id = match prepared.layout {
            None => None,
            Some(layout) => {
                let requested = transaction::request(
                    &data_dir,
                    layout,
                    &prepared.from,
                    &manifest.version,
                    if MANAGED_RESTART.get().is_some() {
                        transaction::RestartMode::Relaunch
                    } else {
                        transaction::RestartMode::Service {
                            unit: service_unit(),
                        }
                    },
                )?;
                self.refresh_update_report();
                Some(requested.id)
            }
        };
        let updater = self.clone();
        let restarting = update_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            if let Err(err) = restart_service() {
                tracing::warn!(error = %err, "service restart failed — restart the engine to finish the update");
                if let (Some(id), Some(data_dir)) = (&restarting, channel::data_dir()) {
                    transaction::fail_request(
                        data_dir,
                        id,
                        "Onicode couldn't restart to install the update, so nothing changed."
                            .into(),
                    );
                    updater.refresh_update_report();
                }
            }
        });
        Ok(AppliedUpdate {
            version: manifest.version,
            update_id,
        })
    }
}

/// The installed macOS app's own version.
fn installed_app_version() -> Option<String> {
    match detect_install() {
        InstallKind::MacApp { bundle } => bundle_app_version(&bundle),
        _ => None,
    }
}

/// A release ready to try.
struct PreparedInstall {
    from: String,
    /// `None`: it's already what's installed.
    layout: Option<transaction::Layout>,
}

/// Whatever this kind of install needs done before anything stops. `None`: nothing
/// newer to install. A release that isn't installable here is an error, in words
/// for the person.
async fn prepare_this_install(
    feed_url: &str,
    manifest: &Manifest,
    data_dir: &Path,
) -> anyhow::Result<Option<PreparedInstall>> {
    match detect_install() {
        InstallKind::MacApp { bundle } => {
            // The engine's version isn't the app's (a beta carries stable's).
            let from =
                bundle_app_version(&bundle).context("reading the installed app's version")?;
            match offer(
                manifest,
                &from,
                manifest.channel,
                bundle_engine_version(&bundle).as_deref(),
                channel::macos_version(),
            ) {
                Offer::Newer | Offer::Switch => {}
                Offer::Held(why) => bail!(why),
                Offer::Current => return Ok(None),
            }
            let parent = bundle
                .parent()
                .context("app bundle has no parent directory")?;
            let download = artifact_size(feed_url, manifest, &mac_app_artifact(&manifest.version))
                .await
                .unwrap_or(transaction::UNKNOWN_ARTIFACT_BYTES);
            // A staged copy and the one beside the installed app.
            if let Err(why) = transaction::ensure_room(data_dir, parent, download.saturating_mul(2))
            {
                bail!(why);
            }
            let staged = stage_mac_app(feed_url, manifest, data_dir, &no_progress).await?;
            let name = bundle
                .file_name()
                .context("app bundle has no name")?
                .to_string_lossy();
            let incoming = parent.join(format!(".{name}.new-{}", transaction::new_id()));
            // Everything that can fail about installing it happens here, with
            // nothing stopped: it's a plain copy, and then one atomic swap.
            if let Err(err) = run(
                "ditto",
                &[&staged.to_string_lossy(), &incoming.to_string_lossy()],
            ) {
                let _ = std::fs::remove_dir_all(&incoming);
                return Err(err).context("copying the new app into place");
            }
            if let Some(version_dir) = staged.parent()
                && !process_runs_from(version_dir)
            {
                let _ = std::fs::remove_dir_all(version_dir);
            }
            Ok(Some(PreparedInstall {
                from,
                layout: Some(transaction::Layout::MacBundle { bundle, incoming }),
            }))
        }
        InstallKind::Unmanaged => bail!("this install doesn't update itself"),
    }
}

/// What `ApplyUpdate` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedUpdate {
    pub version: String,
    /// `UpdateStatus`'s `update.id`, to find out how it went. Absent when the
    /// version was already installed and only needed a restart.
    pub update_id: Option<String>,
}

/// What the feed says the release's download weighs, if it says.
async fn artifact_size(feed_url: &str, manifest: &Manifest, file: &str) -> Option<u64> {
    let base = manifest.channel.base(&release_base(feed_url).ok()?);
    let url = format!("{base}/{file}");
    let response = http_client().ok()?.head(&url).send().await.ok()?;
    response
        .status()
        .is_success()
        .then(|| response.content_length())?
}

/// Wall-clock check deadlines with failure backoff.
#[derive(Debug, Default)]
struct Schedule {
    /// `None` until the first check: due immediately.
    next_due: Option<SystemTime>,
    failures: usize,
}

impl Schedule {
    fn due(&self, now: SystemTime) -> bool {
        match self.next_due {
            None => true,
            Some(due) => match due.duration_since(now) {
                Err(_) => true,
                // A deadline further out than any interval means the clock
                // jumped backwards; don't wait for it to catch up.
                Ok(remaining) => remaining.is_zero() || remaining > CHECK_INTERVAL,
            },
        }
    }

    fn record(&mut self, now: SystemTime, ok: bool) {
        let delay = if ok {
            self.failures = 0;
            CHECK_INTERVAL
        } else {
            let delay = CHECK_RETRY[self.failures.min(CHECK_RETRY.len() - 1)];
            self.failures = self.failures.saturating_add(1);
            delay
        };
        self.next_due = Some(now + delay);
    }

    /// Monotonic sleep until the next look at the clock.
    fn wait(&self, now: SystemTime) -> Duration {
        self.next_due
            .and_then(|due| due.duration_since(now).ok())
            .unwrap_or(Duration::ZERO)
            .clamp(Duration::from_secs(1), SCHEDULE_TICK)
    }
}

/// Restarts a service daemon into a newer binary that is already installed
/// (the desktop app swapped the bundle).
/// Waits for quiescence, and tries at most once per installed version so a
/// broken service manager cannot turn into a restart loop.
#[derive(Debug, Default)]
struct SupersededRestart {
    attempted: Option<String>,
    deferred_logged: bool,
}

impl SupersededRestart {
    fn poll(&mut self, quiescent: impl Fn() -> bool) {
        let Some(installed) = installed_version(&detect_install()) else {
            return;
        };
        self.poll_installed(
            &installed,
            current_version(),
            || quiescent() && managed_restart_ready(),
            restart_service,
        );
    }

    fn poll_installed(
        &mut self,
        installed: &str,
        running: &str,
        quiescent: impl Fn() -> bool,
        restart: impl FnOnce() -> anyhow::Result<()>,
    ) {
        if !version_newer(installed, running) || self.attempted.as_deref() == Some(installed) {
            return;
        }
        if !quiescent() {
            if !self.deferred_logged {
                self.deferred_logged = true;
                tracing::info!(
                    installed,
                    running,
                    "newer version installed; restart deferred: sessions or terminals active, or waiting for the app"
                );
            }
            return;
        }
        self.attempted = Some(installed.to_owned());
        tracing::info!(
            installed,
            running,
            "newer version installed; restarting service"
        );
        if let Err(err) = restart() {
            tracing::warn!(error = %err, "service restart failed — restart the engine to finish the update");
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_cant_update_from_afar_says_why() {
        let bundle = InstallKind::MacApp {
            bundle: PathBuf::from("/Applications/Onicode.app"),
        };
        let mac = UpdateAbility::decide(&bundle, true, None);
        assert_eq!(mac.install, InstallMethod::MacApp);
        assert!(mac.can_apply_remotely, "the app's background engine can");
        assert_eq!(mac.cannot_apply_reason, None);
        let mac_by_hand = UpdateAbility::decide(&bundle, false, None);
        assert!(!mac_by_hand.can_apply_remotely);
        assert!(mac_by_hand.cannot_apply_reason.unwrap().contains("Mac"));
        let mac_translocated =
            UpdateAbility::decide(&bundle, true, Some(UpdateBlocker::Translocated));
        assert!(!mac_translocated.can_apply_remotely);
        assert!(
            mac_translocated
                .cannot_apply_reason
                .unwrap()
                .contains("Applications")
        );
        let read_only = UpdateAbility::decide(
            &bundle,
            true,
            Some(UpdateBlocker::NotWritable(PathBuf::from("/opt/onicode"))),
        );
        assert!(!read_only.can_apply_remotely);
        assert!(
            read_only
                .cannot_apply_reason
                .unwrap()
                .contains("/opt/onicode")
        );

        let source = UpdateAbility::decide(&InstallKind::Unmanaged, true, None);
        assert_eq!(source.install, InstallMethod::Unmanaged);
        assert!(!source.can_apply_remotely);
    }

    /// An older engine's report has none of the fields; a newer engine's
    /// method this build doesn't know is `Unknown`, not a parse failure that
    /// would lose the whole report.
    #[test]
    fn older_and_newer_engines_reports_still_read() {
        let older: UpdateStatus = serde_json::from_value(serde_json::json!({
            "currentVersion": "0.2.100",
            "latestVersion": "0.2.136",
            "updateAvailable": true,
        }))
        .unwrap();
        assert_eq!(older.install, None);
        assert_eq!(older.can_apply_remotely, None);
        assert_eq!(older.cannot_apply_reason, None);

        let newer: UpdateStatus = serde_json::from_value(serde_json::json!({
            "currentVersion": "0.2.200",
            "install": "somethingNew",
            "canApplyRemotely": false,
        }))
        .unwrap();
        assert_eq!(newer.install, Some(InstallMethod::Unknown));
        assert_eq!(newer.can_apply_remotely, Some(false));
    }

    #[test]
    fn the_report_names_its_fields_for_clients() {
        let status = UpdateStatus::initial("1.0.0").with_ability(UpdateAbility::decide(
            &InstallKind::Unmanaged,
            false,
            None,
        ));
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["install"], "unmanaged");
        assert_eq!(value["canApplyRemotely"], false);
        assert!(value["cannotApplyReason"].is_string());
    }

    #[tokio::test]
    async fn stalled_update_headers_and_body_time_out_but_progressing_body_survives() {
        use std::time::Duration;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for stall_body in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                let read = socket.read(&mut request).await.unwrap();
                assert!(read > 0, "client sent no request");
                if stall_body {
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nx")
                        .await
                        .unwrap();
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            });
            let client =
                http_client_with_timeouts(Duration::from_millis(200), Duration::from_millis(100))
                    .unwrap();
            let request = async {
                client
                    .get(format!("http://{address}"))
                    .send()
                    .await?
                    .bytes()
                    .await
            };
            let error = tokio::time::timeout(Duration::from_secs(1), request)
                .await
                .expect("bounded read")
                .unwrap_err();
            assert!(error.is_timeout());
            server.abort();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            assert!(read > 0, "client sent no request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n")
                .await
                .unwrap();
            for _ in 0..10 {
                socket.write_all(b"x").await.unwrap();
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        });
        let client =
            http_client_with_timeouts(Duration::from_secs(1), Duration::from_millis(200)).unwrap();
        assert_eq!(
            client
                .get(format!("http://{address}"))
                .send()
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .len(),
            10
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn release_metadata_has_size_and_total_time_limits() {
        use std::time::Duration;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Reject an advertised oversized body before buffering it.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            assert!(read > 0, "client sent no request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\n\r\n01234567890")
                .await
                .unwrap();
        });
        let client =
            http_client_with_timeouts(Duration::from_secs(1), Duration::from_secs(1)).unwrap();
        let error = fetch_release_metadata_with_limits(
            &client,
            &format!("http://{address}"),
            Duration::from_secs(1),
            10,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("exceeds 10 bytes"));
        server.await.unwrap();

        // Enforce the same cap when Content-Length is absent.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            assert!(read > 0, "client sent no request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n01234567890")
                .await
                .unwrap();
        });
        let error = fetch_release_metadata_with_limits(
            &client,
            &format!("http://{address}"),
            Duration::from_secs(1),
            10,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("exceeds 10 bytes"));
        server.await.unwrap();

        // Progressing bytes stay below the client's inactivity timeout but
        // must still obey the metadata operation's total deadline.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            let read = socket.read(&mut request).await.unwrap();
            assert!(read > 0, "client sent no request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n")
                .await
                .unwrap();
            for _ in 0..10 {
                if socket.write_all(b"x").await.is_err() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        });
        let error = fetch_release_metadata_with_limits(
            &client,
            &format!("http://{address}"),
            Duration::from_millis(80),
            100,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("metadata deadline"));
        server.abort();
    }

    #[tokio::test]
    async fn stalled_update_tls_handshake_has_a_connect_deadline() {
        use std::time::Duration;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        });
        let client =
            http_client_with_timeouts(Duration::from_millis(100), Duration::from_secs(30)).unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            client.get(format!("https://{address}")).send(),
        )
        .await
        .expect("bounded TLS handshake")
        .unwrap_err();
        assert!(error.is_timeout());
        server.abort();
    }

    #[test]
    fn update_feed_override_requires_an_https_base_url() {
        assert_eq!(
            validate_release_override(" https://example.com/releases/ ").unwrap(),
            "https://example.com/releases"
        );
        for url in [
            "http://example.com/releases",
            "file:///tmp/update",
            "https://user:password@example.com",
            "https://example.com?feed=x",
            "https://example.com/#fragment",
        ] {
            assert!(validate_release_override(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn version_compare() {
        assert!(version_newer("0.1.1", "0.1.0"));
        assert!(version_newer("0.2.0", "0.1.9"));
        assert!(version_newer("0.1.10", "0.1.9"));
        assert!(version_newer("v0.1.1", "0.1.0"));
        assert!(version_newer("0.1.0.1", "0.1.0"));
        assert!(!version_newer("0.1.0", "0.1.0"));
        assert!(!version_newer("0.1.0", "0.1.1"));
        // Garbage never counts as newer.
        assert!(!version_newer("", "0.1.0"));
        assert!(!version_newer("nightly", "0.1.0"));
    }

    #[test]
    fn install_kind_detection() {
        assert_eq!(
            detect_install_from(Path::new(
                "/Applications/Onicode.app/Contents/MacOS/onicode"
            )),
            InstallKind::MacApp {
                bundle: PathBuf::from("/Applications/Onicode.app")
            }
        );
        // A path merely containing `.app` without the bundle layout is not a bundle.
        assert_eq!(
            detect_install_from(Path::new("/tmp/foo.app/onicode")),
            InstallKind::Unmanaged
        );
        assert_eq!(
            detect_install_from(Path::new("/src/target/release/onicode")),
            InstallKind::Unmanaged
        );
    }

    #[test]
    fn artifact_names_match_packaging() {
        let (_, arch) = platform_key();
        assert_eq!(
            mac_app_artifact("0.2.0"),
            format!("onicode-0.2.0-macos-{arch}-app.tar.gz")
        );
    }

    #[test]
    fn manifest_parses_with_and_without_files() {
        let full: Manifest = serde_json::from_str(
            r#"{"version":"0.1.1","files":{"onicode-0.1.1-macos-arm64-app.tar.gz":{"sha256":"abc"}}}"#,
        )
        .unwrap();
        assert_eq!(full.version, "0.1.1");
        assert_eq!(
            full.files["onicode-0.1.1-macos-arm64-app.tar.gz"]
                .sha256
                .as_deref(),
            Some("abc")
        );
        let bare: Manifest = serde_json::from_str(r#"{"version":"0.1.1"}"#).unwrap();
        assert!(bare.files.is_empty());
    }

    #[test]
    fn schedule_is_wall_clock_with_failure_backoff() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut schedule = Schedule::default();
        assert!(schedule.due(start), "the first check is due immediately");

        schedule.record(start, true);
        assert!(!schedule.due(start + Duration::from_secs(59 * 60)));
        // A machine that slept through the deadline is due the moment it
        // looks at the clock again — no monotonic remainder to wait out.
        assert!(schedule.due(start + Duration::from_secs(9 * 60 * 60)));
        assert_eq!(schedule.wait(start), SCHEDULE_TICK);
        // A clock that jumped backwards does not postpone checks.
        assert!(schedule.due(start - Duration::from_secs(24 * 60 * 60)));

        let mut backoff = Vec::new();
        for _ in 0..6 {
            schedule.record(start, false);
            backoff.push(schedule.next_due.unwrap().duration_since(start).unwrap());
        }
        assert_eq!(
            backoff,
            [
                CHECK_RETRY[0],
                CHECK_RETRY[1],
                CHECK_RETRY[2],
                CHECK_RETRY[3],
                CHECK_RETRY[3],
                CHECK_RETRY[3]
            ]
        );
        schedule.record(start, true);
        assert_eq!(schedule.failures, 0);
    }

    #[test]
    fn superseded_service_restarts_once_when_quiet() {
        let restarts = std::cell::Cell::new(0);
        let restart = || {
            restarts.set(restarts.get() + 1);
            Ok(())
        };
        let mut superseded = SupersededRestart::default();
        // Same or older installed versions never restart.
        superseded.poll_installed("0.2.0", "0.2.0", || true, restart);
        superseded.poll_installed("0.1.9", "0.2.0", || true, restart);
        assert_eq!(restarts.get(), 0);
        // Busy: deferred, not attempted.
        superseded.poll_installed("0.2.1", "0.2.0", || false, restart);
        assert_eq!(restarts.get(), 0);
        // Quiet: exactly one attempt per installed version.
        superseded.poll_installed("0.2.1", "0.2.0", || true, restart);
        superseded.poll_installed("0.2.1", "0.2.0", || true, restart);
        assert_eq!(restarts.get(), 1);
        superseded.poll_installed("0.2.2", "0.2.0", || true, restart);
        assert_eq!(restarts.get(), 2);
    }

    #[test]
    fn bundle_version_reads_the_packaged_plist() {
        let template = include_str!("../../../packaging/macos/Info.plist");
        let plist = template.replace("__VERSION__", "0.3.1");
        assert_eq!(
            plist_string(&plist, "CFBundleShortVersionString").as_deref(),
            Some("0.3.1")
        );
        // Packaging names the engine it bundles.
        assert_eq!(
            plist_string(&plist, "OnicodeEngineVersion").as_deref(),
            Some("0.3.1")
        );
        assert_eq!(
            plist_string("<plist></plist>", "CFBundleShortVersionString"),
            None
        );
    }

    #[test]
    fn a_bundle_s_engine_version_is_its_key_else_the_app_s() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("Onicode.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        let write = |body: &str| std::fs::write(bundle.join("Contents/Info.plist"), body).unwrap();
        // A beta: the app is 0.2.136-beta.1, its engine stable's 0.2.135. An
        // engine weighing the app's version would restart forever.
        write(
            "<plist><dict><key>CFBundleShortVersionString</key><string>0.2.136</string>\
             <key>OnicodeEngineVersion</key><string>0.2.135</string></dict></plist>",
        );
        assert_eq!(bundle_engine_version(&bundle).as_deref(), Some("0.2.135"));
        let mac = InstallKind::MacApp {
            bundle: bundle.clone(),
        };
        assert_eq!(installed_version(&mac).as_deref(), Some("0.2.135"));
        // A bundle from before the key.
        write(
            "<plist><dict><key>CFBundleShortVersionString</key><string>0.2.134</string></dict></plist>",
        );
        assert_eq!(bundle_engine_version(&bundle).as_deref(), Some("0.2.134"));
    }

    #[test]
    fn prereleases_sort_as_semver_says() {
        let ordered = [
            "0.2.134",
            "0.2.135",
            "0.2.136-beta.1",
            "0.2.136-beta.2",
            "0.2.136-beta.10",
            "0.2.136",
            "0.2.137-beta.1",
        ];
        for pair in ordered.windows(2) {
            assert!(version_newer(pair[1], pair[0]), "{} > {}", pair[1], pair[0]);
            assert!(
                !version_newer(pair[0], pair[1]),
                "{} < {}",
                pair[0],
                pair[1]
            );
        }
        assert!(!version_newer("0.2.136-beta.1", "0.2.136-beta.1"));
        assert!(version_newer("v0.2.136-beta.1", "0.2.135"));
        // Garbage never counts as newer.
        for bad in ["0.2.136-", "0.2.136-beta..1", "0.2.136+build", "beta", ""] {
            assert!(!version_newer(bad, "0.0.1"), "{bad}");
            assert!(parse_version(bad).is_none(), "{bad}");
        }
        // The numeric compare is unchanged.
        assert!(version_newer("0.1.10", "0.1.9") && version_newer("0.1.0", "0.1"));
    }

    fn manifest_of(version: &str, channel: Channel) -> Manifest {
        Manifest {
            version: version.into(),
            channel,
            ..Manifest::default()
        }
    }

    #[test]
    fn a_build_takes_newer_releases_of_its_channel() {
        let stable = manifest_of("0.2.136", Channel::Stable);
        assert_eq!(
            offer(&stable, "0.2.135", Channel::Stable, None, None),
            Offer::Newer
        );
        assert_eq!(
            offer(&stable, "0.2.136", Channel::Stable, None, None),
            Offer::Current
        );
        assert_eq!(
            offer(&stable, "0.2.137", Channel::Stable, None, None),
            Offer::Current
        );
        let beta = manifest_of("0.2.136-beta.2", Channel::Beta);
        assert_eq!(
            offer(
                &beta,
                "0.2.136-beta.1",
                Channel::Beta,
                Some("0.2.135"),
                None
            ),
            Offer::Newer
        );
        assert_eq!(
            offer(
                &beta,
                "0.2.136-beta.2",
                Channel::Beta,
                Some("0.2.135"),
                None
            ),
            Offer::Current
        );
    }

    #[test]
    fn choosing_beta_on_stable_takes_only_a_newer_beta() {
        let beta = manifest_of("0.2.136-beta.1", Channel::Beta);
        assert_eq!(
            offer(&beta, "0.2.135", Channel::Beta, None, None),
            Offer::Newer
        );
        // Stable moved past the last beta: wait for the next one.
        assert_eq!(
            offer(&beta, "0.2.136", Channel::Beta, None, None),
            Offer::Current
        );
    }

    #[test]
    fn leaving_beta_installs_stable_even_when_older() {
        let stable = manifest_of("0.2.135", Channel::Stable);
        assert_eq!(
            offer(
                &stable,
                "0.2.136-beta.2",
                Channel::Stable,
                Some("0.2.135"),
                None
            ),
            Offer::Switch
        );
        // A stable that caught up is simply newer.
        let caught_up = manifest_of("0.2.136", Channel::Stable);
        assert_eq!(
            offer(
                &caught_up,
                "0.2.136-beta.2",
                Channel::Stable,
                Some("0.2.135"),
                None
            ),
            Offer::Newer
        );
    }

    #[test]
    fn leaving_beta_waits_when_stable_s_engine_is_older_than_the_beta_s() {
        let stable = manifest_of("0.2.135", Channel::Stable);
        let held = offer(
            &stable,
            "0.2.136-beta.3",
            Channel::Stable,
            Some("0.2.136"),
            None,
        );
        assert!(
            matches!(&held, Offer::Held(why) if why.contains("catches up")),
            "{held:?}"
        );
        // A manifest naming its engine is weighed by that.
        let named = Manifest {
            engine_version: Some("0.2.136".into()),
            ..stable
        };
        assert_eq!(
            offer(
                &named,
                "0.2.136-beta.3",
                Channel::Stable,
                Some("0.2.136"),
                None
            ),
            Offer::Switch
        );
    }

    #[test]
    fn a_release_for_a_newer_macos_is_held() {
        let beta = Manifest {
            minimum_macos: Some("28.0".into()),
            ..manifest_of("0.2.136-beta.1", Channel::Beta)
        };
        let held = offer(&beta, "0.2.135", Channel::Beta, None, Some("27.2"));
        assert!(
            matches!(&held, Offer::Held(why) if why.contains("macOS 28.0")),
            "{held:?}"
        );
        assert_eq!(
            offer(&beta, "0.2.135", Channel::Beta, None, Some("28.1")),
            Offer::Newer
        );
        assert!(!held.installs());
    }

    #[test]
    fn a_beta_manifest_reads_its_extra_fields() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"version":"0.2.136-beta.1","engineVersion":"0.2.135","minimumMacOS":"28.0","files":{}}"#,
        )
        .unwrap();
        assert_eq!(manifest.engine_version.as_deref(), Some("0.2.135"));
        assert_eq!(manifest.minimum_macos.as_deref(), Some("28.0"));
        // A stable manifest has neither.
        let stable: Manifest = serde_json::from_str(r#"{"version":"0.2.135","files":{}}"#).unwrap();
        assert_eq!((stable.engine_version, stable.minimum_macos), (None, None));
    }

    #[test]
    fn installed_version_follows_the_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = tmp.path().join("Onicode.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(
            bundle.join("Contents/Info.plist"),
            include_str!("../../../packaging/macos/Info.plist").replace("__VERSION__", "0.4.1"),
        )
        .unwrap();
        assert_eq!(
            installed_version(&InstallKind::MacApp { bundle }).as_deref(),
            Some("0.4.1")
        );
        assert_eq!(installed_version(&InstallKind::Unmanaged), None);
    }

    #[test]
    fn mac_bundles_that_cannot_replace_themselves_are_explained() {
        let writable = |_: &Path| true;
        let read_only = |_: &Path| false;
        assert_eq!(
            mac_bundle_blocker(Path::new("/Applications/Onicode.app"), writable),
            None
        );
        assert_eq!(
            mac_bundle_blocker(
                Path::new("/private/var/folders/x/T/AppTranslocation/ABC/d/Onicode.app"),
                writable
            ),
            Some(UpdateBlocker::Translocated)
        );
        assert_eq!(
            mac_bundle_blocker(Path::new("/Volumes/Onicode/Onicode.app"), read_only),
            Some(UpdateBlocker::DiskImage)
        );
        // An external drive the user can write to updates in place.
        assert_eq!(
            mac_bundle_blocker(Path::new("/Volumes/Work/Onicode.app"), writable),
            None
        );
        assert_eq!(
            mac_bundle_blocker(Path::new("/Applications/Onicode.app"), read_only),
            Some(UpdateBlocker::NotWritable(PathBuf::from("/Applications")))
        );
    }

    #[test]
    fn writability_probe_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(dir_writable(tmp.path()));
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
        assert!(!dir_writable(&tmp.path().join("missing")));
    }

    /// Serves `body` for every request until aborted.
    async fn serve_forever(body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut request = [0; 4096];
                    let _ = socket.read(&mut request).await;
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(head.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                });
            }
        });
        (base, server)
    }

    #[tokio::test]
    async fn desktop_checker_reports_a_newer_release_on_demand() {
        let (base, server) = serve_forever(br#"{"version":"999.0.0","files":{}}"#.to_vec()).await;
        let updater = Updater::spawn_desktop(base);
        let status = updater.check().await.unwrap();
        assert!(status.update_available);
        assert_eq!(status.latest_version.as_deref(), Some("999.0.0"));
        assert_eq!(status.current_version, current_version());
        assert!(status.checked_at.is_some());
        assert_eq!(*updater.watch().borrow(), status);
        updater.shutdown().await;
        server.abort();
    }

    #[test]
    fn a_manifest_carries_its_release_notes() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"version":"0.3.0","notes":"- Faster.\n","date":"2026-11-07","files":{}}"#,
        )
        .unwrap();
        assert_eq!(manifest.notes.as_deref(), Some("- Faster.\n"));
        assert_eq!(manifest.date.as_deref(), Some("2026-11-07"));
        let bare: Manifest = serde_json::from_str(r#"{"version":"0.3.0"}"#).unwrap();
        assert_eq!((bare.notes, bare.date), (None, None));
        // Absent notes stay absent when written back.
        let written = serde_json::to_value(manifest_of("0.3.0", Channel::Stable)).unwrap();
        assert!(written.get("notes").is_none() && written.get("date").is_none());
    }

    #[test]
    fn the_feed_set_at_run_time_wins_and_empty_turns_updates_off() {
        let built = Some("https://built.example");
        assert_eq!(
            feed_from(Some(" https://run.example "), built).as_deref(),
            Some("https://run.example")
        );
        assert_eq!(feed_from(None, built).as_deref(), built);
        assert_eq!(feed_from(Some(""), built), None);
        assert_eq!(feed_from(None, None), None);
    }

    #[tokio::test]
    async fn a_download_reports_its_progress_up_to_the_whole() {
        let body = vec![7_u8; 256 * 1024];
        let (base, server) = serve_forever(body.clone()).await;
        let tmp = tempfile::tempdir().unwrap();
        let manifest = Manifest {
            version: "9.9.9".into(),
            files: [(
                "blob".to_owned(),
                FileMeta {
                    sha256: Some(base16ct::lower::encode_string(&Sha256::digest(&body))),
                },
            )]
            .into(),
            ..Manifest::default()
        };
        let seen = std::sync::Mutex::new(Vec::new());
        let record = |done: u64, total: Option<u64>| seen.lock().unwrap().push((done, total));
        let dest = tmp.path().join("blob");
        download_release_file(&base, &manifest, "blob", &dest, &record)
            .await
            .unwrap();
        server.abort();
        let seen = seen.into_inner().unwrap();
        let whole = body.len() as u64;
        assert_eq!(seen.first(), Some(&(0, Some(whole))));
        assert_eq!(seen.last(), Some(&(whole, Some(whole))));
        assert!(
            seen.windows(2).all(|pair| pair[0].0 <= pair[1].0),
            "{seen:?}"
        );
        assert_eq!(std::fs::read(&dest).unwrap(), body);
    }
}
