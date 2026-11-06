//! The clipboard between this engine and the owner's others
//! (`docs/onicode/decisions/one-machine.md`, "Clipboard"; the logic is `onicode_clipboard`).
//!
//! - **Serving.** `WatchClipboard` is a subscription of this engine's offers; `ClipboardFetch`
//!   answers one type of one offer as base64 (a thin client's way); the `clip/...` file streams
//!   carry the same bytes raw ([`ClipboardHost::serve_stream`]); `ClipboardSettings` reads and sets
//!   this engine's settings, kept in `clipboard.json` in the data directory; `ClipboardFollow` is
//!   the app's viewer saying which session has focus.
//! - **Following.** This engine subscribes to `WatchClipboard` on every engine of the owner the
//!   directory finds, resubscribing as they come and go, and hands each offer to the clipboard,
//!   which applies the newer ones.
//! - **Pasting.** A promise fetches its bytes over a `clip/<seq>/<item>/<type>` file stream from
//!   the origin; files come through the transfer engine (`StartTransfer` to this engine, then
//!   `WatchTransfers` until done).
//!
//! The board is memory ([`onicode_clipboard::Held`]) until the runtime starts the clipboard on
//! the general pasteboard ([`ClipboardHost::start_general`]): engines assembled in tests and
//! fixtures never read or write the person's clipboard.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use bytes::Bytes;
use futures::StreamExt as _;
use onicode_clipboard::{
    Access, Board, Clipboard, ClipboardFetchRequest, ClipboardOffer, ClipboardSettingsRequest,
    Config, FETCH_MAX, Peers, SystemClock,
};
pub use onicode_clipboard::{ClipboardSettings, PasteAccess};
use onicode_net::{Role, StreamHead, StreamKind, StreamRx, StreamTx, close};
use onicode_peer::PeerLinks;
use onicode_rpc::{Caller, RpcError, RpcReply, RpcService, methods, parse_params};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

/// The settings file in the engine's data directory.
const SETTINGS_FILE: &str = "clipboard.json";
/// The first wait before a peer's subscription is opened again, doubling to [`RESUBSCRIBE_MAX`].
const RESUBSCRIBE_MIN: Duration = Duration::from_secs(1);
/// The longest wait between attempts on a peer that the directory still lists.
const RESUBSCRIBE_MAX: Duration = Duration::from_secs(30);
/// The most a `ClipboardFetch` answers in one reply; past it a caller opens a `clip/` stream.
const FETCH_REPLY_MAX: usize = 32 << 20;
/// What a served `clip/` stream writes at once, and how much it lets queue on the link.
const STREAM_CHUNK: usize = 256 << 10;
const STREAM_WINDOW: usize = 1 << 20;

/// `ClipboardFollow` (the app's own engine only): `deviceId`'s remote desktop viewer gained
/// (`focused`) or lost focus.
pub const CLIPBOARD_FOLLOW: &str = "ClipboardFollow";

/// `ClipboardSettings`' reply: the settings, and whether this device may read its pasteboard
/// unasked.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsReply {
    /// Follow the clipboard at all.
    pub enabled: bool,
    /// Send auto-generated items too.
    pub auto_generated: bool,
    /// macOS's "Paste from Other Apps" for this engine.
    #[serde(default)]
    pub paste: PasteAccess,
}

/// `ClipboardFollow`'s parameters.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowParams {
    /// The device whose desktop the viewer shows.
    pub device_id: String,
    /// The viewer has focus now.
    pub focused: bool,
}

/// This engine's clipboard: its settings, its links and, once started, the clipboard itself.
pub struct ClipboardHost {
    device: String,
    data_dir: PathBuf,
    settings: Mutex<ClipboardSettings>,
    clipboard: OnceLock<Clipboard>,
    access: OnceLock<Access>,
    peers: Arc<LinkPeers>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl ClipboardHost {
    /// The clipboard of the engine `device` whose data directory is `data_dir`, not yet started.
    #[must_use]
    pub fn new(data_dir: &Path, device: &str) -> Arc<Self> {
        Arc::new(Self {
            device: device.to_owned(),
            data_dir: data_dir.to_owned(),
            settings: Mutex::new(load_settings(data_dir)),
            clipboard: OnceLock::new(),
            access: OnceLock::new(),
            peers: Arc::new(LinkPeers {
                device: device.to_owned(),
                links: Mutex::new(None),
                services: Mutex::new(Vec::new()),
            }),
            tasks: Mutex::new(Vec::new()),
        })
    }

    /// Starts on the person's general pasteboard (macOS), or on a board in memory that the
    /// clipboard shims fill (elsewhere). Only the running engine calls it, never a test.
    pub fn start_general(&self) {
        #[cfg(target_os = "macos")]
        let (board, access): (Arc<dyn Board>, Access) = (
            Arc::new(onicode_clipboard::MacBoard::general()),
            Arc::new(|| paste_access(onicode_grants::paste::behavior())),
        );
        #[cfg(not(target_os = "macos"))]
        let (board, access): (Arc<dyn Board>, Access) = (
            Arc::new(onicode_clipboard::Held::default()),
            Arc::new(|| PasteAccess::Ungated),
        );
        let cache = system_cache();
        if let Err(error) = std::fs::remove_dir_all(&cache)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::debug!(cache = %cache.display(), %error, "clipboard cache kept");
        }
        self.start(board, access, cache);
    }

    /// Starts on `board`, read as `access` says, received files landing under `cache`. Once:
    /// a second start is ignored. Call it inside the runtime.
    pub fn start(&self, board: Arc<dyn Board>, access: Access, cache: PathBuf) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("clipboard not started: no runtime");
            return;
        };
        if self.clipboard.get().is_some() {
            return;
        }
        let clipboard = Clipboard::new(
            Config {
                device: self.device.clone(),
                board,
                peers: Arc::clone(&self.peers) as Arc<dyn Peers>,
                clock: Arc::new(SystemClock),
                access: Arc::clone(&access),
                cache,
                settings: *lock(&self.settings),
            },
            runtime.clone(),
        );
        if self.clipboard.set(clipboard.clone()).is_err() {
            return;
        }
        let _ = self.access.set(access);
        lock(&self.tasks).push(runtime.spawn(clipboard.watcher()));
    }

    /// Attaches the links to the owner's other engines: fetches go over them, and this engine
    /// follows every engine the directory finds.
    pub fn set_links(self: &Arc<Self>, links: Arc<PeerLinks>) {
        *lock(&self.peers.links) = Some(Arc::clone(&links));
        let task = tokio::spawn(follow_peers(Arc::downgrade(self), links));
        lock(&self.tasks).push(task);
    }

    /// The engine's own RPC surface, for the transfers that bring copied files. Every service
    /// built is offered; the first still alive answers.
    pub fn set_service(&self, service: Weak<dyn RpcService>) {
        let mut services = lock(&self.peers.services);
        services.retain(|kept| kept.strong_count() > 0);
        services.push(service);
    }

    /// The clipboard, once started.
    #[must_use]
    pub fn clipboard(&self) -> Option<&Clipboard> {
        self.clipboard.get()
    }

    /// Whether `method` is one of the clipboard's.
    #[must_use]
    pub fn handles(method: &str) -> bool {
        matches!(
            method,
            methods::WATCH_CLIPBOARD
                | methods::CLIPBOARD_FETCH
                | methods::CLIPBOARD_SETTINGS
                | CLIPBOARD_FOLLOW
        )
    }

    /// Serves one of [`Self::handles`]' methods. Read the caller first: a spawned task has none.
    ///
    /// # Errors
    ///
    /// Parameters that do not parse; a peer that is not the owner's; a fetch of an offer that
    /// is gone.
    pub async fn handle(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<RpcReply, RpcError> {
        let caller = onicode_rpc::caller();
        match method {
            methods::WATCH_CLIPBOARD => {
                let subscriber = self.admit(&caller)?;
                let Some(clipboard) = self.clipboard.get() else {
                    return Ok(RpcReply::Stream(
                        futures::stream::pending::<serde_json::Value>().boxed(),
                    ));
                };
                let offers = clipboard
                    .subscribe(subscriber)
                    .filter_map(|offer| async move { serde_json::to_value(offer).ok() });
                Ok(RpcReply::Stream(offers.boxed()))
            }
            methods::CLIPBOARD_FETCH => {
                let _subscriber = self.admit(&caller)?;
                let request: ClipboardFetchRequest = parse_params(params)?;
                let clipboard =
                    self.clipboard.get().cloned().ok_or_else(|| {
                        RpcError::Failed("the clipboard is not running here".into())
                    })?;
                let bytes = tokio::task::spawn_blocking(move || {
                    clipboard.serve(request.origin_seq, request.item, &request.uti)
                })
                .await
                .map_err(|e| RpcError::Failed(e.to_string()))?
                .ok_or_else(|| RpcError::Failed("that copy is gone".into()))?;
                if bytes.len() > FETCH_REPLY_MAX {
                    return Err(RpcError::Failed(
                        "too big for a reply: open a clip stream".into(),
                    ));
                }
                RpcReply::value(&serde_json::json!({
                    "data": base64::engine::general_purpose::STANDARD.encode(bytes),
                }))
            }
            methods::CLIPBOARD_SETTINGS => {
                let request: ClipboardSettingsRequest = parse_params(params)?;
                let settings = self.update_settings(&request)?;
                RpcReply::value(&SettingsReply {
                    enabled: settings.enabled,
                    auto_generated: settings.auto_generated,
                    paste: self.paste(),
                })
            }
            CLIPBOARD_FOLLOW => {
                if !caller.is_local() {
                    return Err(RpcError::Failed("only this device's app follows".into()));
                }
                let params: FollowParams = parse_params(params)?;
                if let Some(clipboard) = self.clipboard.get() {
                    if params.focused {
                        clipboard.follow(Some(params.device_id));
                    } else {
                        clipboard.unfollow(&params.device_id);
                    }
                }
                RpcReply::value(&serde_json::json!({}))
            }
            other => Err(RpcError::UnknownMethod(other.to_owned())),
        }
    }

    /// Serves a `clip/<seq>/<item>/<type>` file stream (or `clip/<seq>/<type>` for item 0)
    /// that `device` opened: the bytes, then the end; a reset when the offer is gone. `name` is
    /// the stream's name, with or without its `clip/`. Call it inside the runtime.
    pub fn serve_stream(&self, device: &str, name: &str, tx: StreamTx, rx: StreamRx) {
        let Some(clipboard) = self.clipboard.get().cloned() else {
            tx.reset(close::NOT_GRANTED);
            return;
        };
        if !self.is_owner(device) {
            tx.reset(close::NOT_GRANTED);
            return;
        }
        let Some((seq, item, uti)) = parse_clip(name) else {
            tx.reset(close::PROTOCOL);
            return;
        };
        tokio::spawn(async move {
            // The puller sends nothing; its half is held until the bytes are out.
            let _rx = rx;
            let bytes = tokio::task::spawn_blocking(move || clipboard.serve(seq, item, &uti))
                .await
                .ok()
                .flatten();
            let Some(bytes) = bytes else {
                tx.reset(close::NOT_GRANTED);
                return;
            };
            let bytes = Bytes::from(bytes);
            let mut at = 0;
            while at < bytes.len() {
                tx.room(STREAM_WINDOW).await;
                if tx.is_closed() {
                    return;
                }
                let end = at.saturating_add(STREAM_CHUNK).min(bytes.len());
                tx.send(bytes.slice(at..end));
                at = end;
            }
            tx.finish();
        });
    }

    /// Clears the board if it holds a peer's promises not kept, and stops watching.
    pub fn shutdown(&self) {
        if let Some(clipboard) = self.clipboard.get()
            && clipboard.shutdown()
        {
            tracing::debug!("clipboard promises cleared at quit");
        }
        for task in lock(&self.tasks).drain(..) {
            task.abort();
        }
    }

    /// The subscriber a caller is: `None` for this device's app, the peer's device id for one
    /// of the owner's engines.
    fn admit(&self, caller: &Caller) -> Result<Option<String>, RpcError> {
        match caller {
            Caller::Local => Ok(None),
            Caller::Peer {
                device: Some(device),
            } if self.is_owner(device) => Ok(Some(device.clone())),
            _ => Err(RpcError::Failed(
                "the clipboard goes only to the owner's devices".into(),
            )),
        }
    }

    /// Whether `device` is one of the owner's engines.
    fn is_owner(&self, device: &str) -> bool {
        lock(&self.peers.links)
            .as_ref()
            .is_some_and(|links| links.directory().get(device).is_some_and(|peer| peer.owner))
    }

    fn paste(&self) -> PasteAccess {
        self.access
            .get()
            .map_or(PasteAccess::Ungated, |access| access())
    }

    fn update_settings(
        &self,
        request: &ClipboardSettingsRequest,
    ) -> Result<ClipboardSettings, RpcError> {
        let mut settings = lock(&self.settings);
        let next = ClipboardSettings {
            enabled: request.enabled.unwrap_or(settings.enabled),
            auto_generated: request.auto_generated.unwrap_or(settings.auto_generated),
        };
        if next != *settings {
            save_settings(&self.data_dir, next)
                .map_err(|e| RpcError::Failed(format!("{SETTINGS_FILE}: {e}")))?;
            *settings = next;
            if let Some(clipboard) = self.clipboard.get() {
                clipboard.set_settings(next);
            }
        }
        Ok(*settings)
    }

    fn receive(&self, offer: ClipboardOffer) {
        if let Some(clipboard) = self.clipboard.get() {
            let received = clipboard.receive(offer);
            tracing::trace!(?received, "clipboard offer");
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `onicode_grants`' paste behavior as the clipboard reads it.
#[cfg(target_os = "macos")]
const fn paste_access(behavior: onicode_grants::paste::PasteBehavior) -> PasteAccess {
    use onicode_grants::paste::PasteBehavior;
    match behavior {
        PasteBehavior::Default => PasteAccess::NotAsked,
        PasteBehavior::Ask => PasteAccess::Ask,
        PasteBehavior::AlwaysAllow => PasteAccess::Allowed,
        PasteBehavior::AlwaysDeny => PasteAccess::Denied,
    }
}

/// Where received files land: `~/Library/Caches/onicode/clipboard` on a Mac, the XDG cache
/// elsewhere.
fn system_cache() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let caches = if cfg!(target_os = "macos") {
        home.join("Library/Caches")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
    };
    caches.join("onicode/clipboard")
}

fn load_settings(data_dir: &Path) -> ClipboardSettings {
    std::fs::read(data_dir.join(SETTINGS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_settings(data_dir: &Path, settings: ClipboardSettings) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(&settings).map_err(std::io::Error::other)?;
    let path = data_dir.join(SETTINGS_FILE);
    let partial = data_dir.join(format!(".{SETTINGS_FILE}.partial"));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(partial, path)
}

/// `(seq, item, type)` of a `clip/` stream's name: `clip/<seq>/<item>/<type>`, or
/// `clip/<seq>/<type>` for item 0. A type never starts with a run of digits and a slash.
fn parse_clip(name: &str) -> Option<(u64, usize, String)> {
    let rest = name.strip_prefix("clip/").unwrap_or(name);
    let (seq, rest) = rest.split_once('/')?;
    let seq = seq.parse().ok()?;
    let (item, uti) = match rest.split_once('/') {
        Some((item, uti)) if !item.is_empty() && item.bytes().all(|b| b.is_ascii_digit()) => {
            (item.parse().ok()?, uti)
        }
        _ => (0, rest),
    };
    (!uti.is_empty()).then(|| (seq, item, uti.to_owned()))
}

/// Subscribes to `WatchClipboard` on every engine of the owner the directory lists, until the
/// host or the links go.
async fn follow_peers(host: Weak<ClipboardHost>, links: Arc<PeerLinks>) {
    let directory = Arc::clone(links.directory());
    let mut found = directory.subscribe();
    let mut following: HashMap<String, JoinHandle<()>> = HashMap::new();
    while let Some(own) = host.upgrade().map(|host| host.device.clone()) {
        let engines: Vec<String> = directory
            .peers()
            .into_iter()
            .filter(|peer| peer.device_id != own && peer.owner && peer.hello.role == Role::Engine)
            .map(|peer| peer.device_id)
            .collect();
        following.retain(|device, task| {
            let keep = engines.contains(device) && !task.is_finished();
            if !keep {
                task.abort();
            }
            keep
        });
        for device in engines {
            if let std::collections::hash_map::Entry::Vacant(slot) = following.entry(device) {
                let device = slot.key().clone();
                slot.insert(tokio::spawn(follow(
                    host.clone(),
                    Arc::clone(&links),
                    device,
                )));
            }
        }
        if found.changed().await.is_err() {
            break;
        }
    }
    for task in following.into_values() {
        task.abort();
    }
}

/// Follows one engine's offers, subscribing again whenever the subscription ends.
async fn follow(host: Weak<ClipboardHost>, links: Arc<PeerLinks>, device: String) {
    let mut wait = RESUBSCRIBE_MIN;
    loop {
        match links
            .subscribe(&device, methods::WATCH_CLIPBOARD, serde_json::json!({}))
            .await
        {
            Ok(mut offers) => {
                wait = RESUBSCRIBE_MIN;
                while let Some(item) = offers.recv().await {
                    let Some(host) = host.upgrade() else { return };
                    match serde_json::from_value::<ClipboardOffer>(item) {
                        Ok(offer) if offer.origin_device == device => host.receive(offer),
                        Ok(offer) => {
                            tracing::debug!(%device, origin = %offer.origin_device, "an offer it did not copy")
                        }
                        Err(error) => {
                            tracing::debug!(%device, %error, "an offer that does not parse")
                        }
                    }
                }
            }
            Err(error) => tracing::debug!(%device, %error, "clipboard: subscribe failed"),
        }
        tokio::time::sleep(wait).await;
        wait = wait.saturating_mul(2).min(RESUBSCRIBE_MAX);
    }
}

/// The owner's other engines over the links, and this engine's transfers.
struct LinkPeers {
    device: String,
    links: Mutex<Option<Arc<PeerLinks>>>,
    services: Mutex<Vec<Weak<dyn RpcService>>>,
}

impl LinkPeers {
    fn service(&self) -> Option<Arc<dyn RpcService>> {
        lock(&self.services).iter().find_map(Weak::upgrade)
    }
}

#[async_trait]
impl Peers for LinkPeers {
    async fn fetch(
        &self,
        origin: &str,
        seq: u64,
        item: usize,
        uti: &str,
    ) -> Result<Vec<u8>, String> {
        let links = lock(&self.links)
            .clone()
            .ok_or_else(|| "offline".to_owned())?;
        let head = StreamHead::new(StreamKind::File, format!("clip/{seq}/{item}/{uti}"));
        let (tx, mut rx) = links.open(origin, &head).await.map_err(|e| e.to_string())?;
        tx.finish();
        let mut bytes = Vec::new();
        while let Some(chunk) = rx.chunk().await.map_err(|e| e.to_string())? {
            if bytes.len().saturating_add(chunk.len()) > FETCH_MAX {
                rx.stop(close::NORMAL);
                return Err("past the most a paste takes".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn fetch_files(&self, origin: &str, paths: &[String], into: &Path) -> Result<(), String> {
        let service = self
            .service()
            .ok_or_else(|| "no transfer engine here".to_owned())?;
        let start = service
            .handle(
                methods::START_TRANSFER,
                serde_json::json!({
                    "from": { "deviceId": origin, "paths": paths },
                    "to": { "deviceId": self.device, "dir": into.to_string_lossy() },
                    "conflict": "replace",
                }),
            )
            .await
            .map_err(|e| e.to_string())?;
        let RpcReply::Value(start) = start else {
            return Err("StartTransfer answered a stream".into());
        };
        let id = start
            .get("transferId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "StartTransfer named no transfer".to_owned())?
            .to_owned();
        let RpcReply::Stream(mut updates) = service
            .handle(methods::WATCH_TRANSFERS, serde_json::json!({}))
            .await
            .map_err(|e| e.to_string())?
        else {
            return Err("WatchTransfers answered no stream".into());
        };
        while let Some(item) = updates.next().await {
            let list = match &item {
                serde_json::Value::Array(all) => all.iter().collect::<Vec<_>>(),
                one => vec![one],
            };
            for update in list {
                if update.get("transferId").and_then(serde_json::Value::as_str) != Some(&id) {
                    continue;
                }
                match update.get("state").and_then(serde_json::Value::as_str) {
                    Some("done") => return Ok(()),
                    Some(state @ ("failed" | "cancelled" | "conflict")) => {
                        let why = update
                            .get("error")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(state);
                        return Err(why.to_owned());
                    }
                    _ => {}
                }
            }
        }
        Err("the transfer list ended".into())
    }
}

impl onicode_transfer::ClipStreams for ClipboardHost {
    fn serve_stream(&self, device: &str, head: &str, tx: StreamTx, rx: StreamRx) {
        Self::serve_stream(self, device, head, tx, rx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_heads_name_an_offer_an_item_and_a_type() {
        assert_eq!(
            parse_clip("clip/42/1/public.png"),
            Some((42, 1, "public.png".to_owned()))
        );
        assert_eq!(
            parse_clip("42/public.png"),
            Some((42, 0, "public.png".to_owned()))
        );
        assert_eq!(
            parse_clip("clip/7/text/plain;charset=utf-8"),
            Some((7, 0, "text/plain;charset=utf-8".to_owned()))
        );
        assert_eq!(
            parse_clip("clip/7/2/text/plain"),
            Some((7, 2, "text/plain".to_owned()))
        );
        assert_eq!(parse_clip("clip/x/public.png"), None);
        assert_eq!(parse_clip("clip/7/"), None);
        assert_eq!(parse_clip("clip/7"), None);
    }

    #[test]
    fn settings_default_on_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_settings(dir.path()), ClipboardSettings::default());
        let host = ClipboardHost::new(dir.path(), "a");
        let set = host
            .update_settings(&ClipboardSettingsRequest {
                enabled: None,
                auto_generated: Some(true),
            })
            .unwrap();
        assert_eq!(
            set,
            ClipboardSettings {
                enabled: true,
                auto_generated: true
            }
        );
        assert_eq!(load_settings(dir.path()), set);
    }

    #[test]
    fn the_settings_reply_says_the_paste_grant() {
        let reply = SettingsReply {
            enabled: true,
            auto_generated: false,
            paste: PasteAccess::NotAsked,
        };
        assert_eq!(
            serde_json::to_value(reply).unwrap(),
            serde_json::json!({ "enabled": true, "autoGenerated": false, "paste": "notAsked" })
        );
    }
}
