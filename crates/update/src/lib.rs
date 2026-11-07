//! onicode-update — release checking and self-update for the desktop app: the
//! "Check for Updates…" menu item, the sidebar update row, and the install
//! paths.
//!
//! Release layout: the release feed (`ONICODE_UPDATE_FEED`, or
//! `ONICODE_RELEASES_URL` for its `releases` directory itself) serves
//! `{feed}/releases/*`. `manifest.json` carries the latest version plus a
//! sha256 per artifact; `latest.txt` (version only) remains as the fallback for
//! releases published before the manifest existed.
//!
//! Install kinds and their update paths:
//! - **MacApp** (running out of an app bundle): download the app tarball, swap the bundle
//!   directory, relaunch, driven by the UI.
//! - **Unmanaged** (source builds, hand-copied binaries): report only — the UI's advisory strip
//!   links to [`RELEASES_PAGE`].
//!
//! Desktop installs download as soon as a release is seen and install either
//! on "Restart to update" or when the app next quits, so an app nobody
//! restarts deliberately still lands on the new version at its next launch.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt as _;

pub mod channel;
pub mod desktop;
pub use channel::Channel;

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

/// Where this process was installed from, resolved once: an update moves
/// the running image aside, and a long-running app must still recognise
/// the installation it belongs to.
pub fn detect_install() -> InstallKind {
    static INSTALL: std::sync::OnceLock<InstallKind> = std::sync::OnceLock::new();
    INSTALL
        .get_or_init(|| {
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

/// The engine a macOS app bundle carries: its Info.plist's `OnicodeEngineVersion`
/// (packaging writes it; a beta app's engine is stable's, older than the app),
/// else `CFBundleShortVersionString` (bundles from before the key, whose
/// engine was always the app's own version). Leaving beta weighs it against
/// stable's engine ([`offer`]).
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
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if (file_name.starts_with(&old) || file_name.starts_with(&new))
            && !process_runs_from(&entry.path())
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
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
    // Not deleted: the background engine or this very app may still be running
    // from it. Gone once nothing is (#1037).
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // A beta: the app is 0.2.136, its engine stable's 0.2.135.
        write(
            "<plist><dict><key>CFBundleShortVersionString</key><string>0.2.136</string>\
             <key>OnicodeEngineVersion</key><string>0.2.135</string></dict></plist>",
        );
        assert_eq!(bundle_engine_version(&bundle).as_deref(), Some("0.2.135"));
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
