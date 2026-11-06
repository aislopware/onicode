//! Engine-owned discovery/proxy lifecycle, independent of headed UI lifetimes.
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::{StreamExt, stream};
use onicode_peer::{PeerLinks, StreamHandler};
use tokio_util::sync::CancellationToken;

use crate::catalog::Catalog;
use crate::login::{self, CallbackForwarder, CallbackRoutes, CallbackTunnels};
use crate::tunnel::{self, BoxIo, Connector};
use crate::{discovery, proxy};

/// Discovery must not turn into HTTP traffic against the user's dev servers.
/// A listening socket is probed until it answers HTTP once; that verdict then
/// holds for the socket's lifetime, because the process enumeration each
/// cycle already proves the same process still owns the same port. Sockets
/// that did not answer HTTP (a server bound but not yet serving) are retried
/// with a capped exponential backoff instead of on every cycle.
#[derive(Default)]
pub(crate) struct ProbeMemory {
    verdicts: HashMap<(u32, u64, SocketAddr), Verdict>,
}
enum Verdict {
    Http,
    NotHttp {
        retry_at: Instant,
        backoff: Duration,
    },
}
const NOT_HTTP_INITIAL_BACKOFF: Duration = Duration::from_secs(4);
const NOT_HTTP_MAX_BACKOFF: Duration = Duration::from_secs(60);

/// (servers already confirmed HTTP, listeners to probe this cycle)
type Planned<T> = (Vec<(T, discovery::Listener)>, Vec<(T, discovery::Listener)>);

fn probe_key(listener: &discovery::Listener) -> (u32, u64, SocketAddr) {
    (listener.pid, listener.started_at, listener.address)
}

impl ProbeMemory {
    /// Splits this cycle's candidates into servers already known to speak
    /// HTTP and listeners that need a probe now. Verdicts for sockets that
    /// are no longer listed are forgotten so a reused port is probed afresh.
    pub(crate) fn plan<T>(
        &mut self,
        candidates: Vec<(T, discovery::Listener)>,
        now: Instant,
    ) -> Planned<T> {
        let present: std::collections::HashSet<_> =
            candidates.iter().map(|(_, l)| probe_key(l)).collect();
        self.verdicts.retain(|key, _| present.contains(key));
        let mut known = Vec::new();
        let mut probe = Vec::new();
        for candidate in candidates {
            match self.verdicts.get(&probe_key(&candidate.1)) {
                Some(Verdict::Http) => known.push(candidate),
                Some(Verdict::NotHttp { retry_at, .. }) if *retry_at > now => {}
                _ => probe.push(candidate),
            }
        }
        (known, probe)
    }

    pub(crate) fn record(&mut self, listener: &discovery::Listener, http: bool, now: Instant) {
        let key = probe_key(listener);
        let verdict = if http {
            Verdict::Http
        } else {
            let backoff = match self.verdicts.get(&key) {
                Some(Verdict::NotHttp { backoff, .. }) => (*backoff * 2).min(NOT_HTTP_MAX_BACKOFF),
                _ => NOT_HTTP_INITIAL_BACKOFF,
            };
            Verdict::NotHttp {
                retry_at: now + backoff,
                backoff,
            }
        };
        self.verdicts.insert(key, verdict);
    }
}
#[derive(Clone)]
pub struct PreviewService(Arc<Inner>);
struct Inner {
    catalog: Catalog,
    stop: CancellationToken,
    started: AtomicBool,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Sign-in callbacks peers may reach on this device (see [`login`]).
    callback_routes: CallbackRoutes,
    /// Sign-in callbacks this device forwards to the device running them.
    callback_tunnels: CallbackTunnels,
    /// What a tunnel reaches here: a preview's dev server or a sign-in callback.
    connector: Arc<LocalConnector>,
    /// The links to the user's other engines, once the tailnet is up.
    links: Arc<OnceLock<Arc<PeerLinks>>>,
}
/// What discovery serves, re-read every scan.
#[derive(Clone, Default)]
pub struct Scope {
    /// Every chat this device hosts, with its checkout. A listener counts only
    /// when one of these chats' agents or terminals started it; it is listed
    /// under the deepest checkout containing its working directory, else under
    /// the checkout of the chat that started it.
    pub chats: Vec<(String, PathBuf)>,
    /// Terminal id → chat, for the engine's terminals (project actions run in
    /// one), including ones since closed.
    pub terminals: HashMap<String, String>,
    /// Ports agent CLIs serve their own protocol on (`opencode serve`).
    pub reserved_ports: HashSet<u16>,
}
pub type Projects = Arc<dyn Fn() -> Scope + Send + Sync>;

/// This scan's candidates (`None` when the process table could not be read),
/// each with the checkout it is listed under.
fn candidates(
    scope: Scope,
    origins: &mut discovery::Origins,
) -> Option<Vec<(PathBuf, discovery::Listener)>> {
    let Scope {
        chats,
        terminals,
        reserved_ports,
    } = scope;
    let mut canonical = HashMap::new();
    let chats: HashMap<String, PathBuf> = chats
        .into_iter()
        .filter_map(|(chat, cwd)| {
            let cwd = canonical
                .entry(cwd.clone())
                .or_insert_with(|| cwd.canonicalize().ok())
                .clone()?;
            Some((chat, cwd))
        })
        .collect();
    let mut roots: Vec<PathBuf> = chats
        .values()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    if roots.is_empty() {
        return Some(Vec::new());
    }
    roots.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    let listeners = discovery::listeners(origins)?;
    Some(
        listeners
            .into_iter()
            .filter(|l| {
                l.onicode_owned
                    && l.address.port() != onicode_proto::PREVIEW_PROXY_PORT
                    && !reserved_ports.contains(&l.address.port())
                    && l.pid != std::process::id()
                    && !l.is_authentication_command()
                    && !l.signing_in
            })
            .filter_map(|mut listener| {
                // A chat this device no longer hosts (or another engine's,
                // inherited by a nested one) owns nothing here.
                let chat = match listener.origin.as_deref()?.split_once(':')? {
                    ("chat", chat) => chat.to_owned(),
                    ("terminal", terminal) => terminals.get(terminal)?.clone(),
                    _ => return None,
                };
                let chat_root = chats.get(&chat)?;
                listener.chat_id = Some(chat);
                let root = roots
                    .iter()
                    .find(|root| listener.belongs_to(root))
                    .unwrap_or(chat_root)
                    .clone();
                Some((root, listener))
            })
            .collect(),
    )
}
impl PreviewService {
    pub fn new(file: PathBuf, device_id: String, device_name: String) -> anyhow::Result<Self> {
        let catalog = Catalog::open(file, device_id, device_name)?;
        let callback_routes = CallbackRoutes::default();
        Ok(Self(Arc::new(Inner {
            connector: Arc::new(LocalConnector(catalog.clone(), callback_routes.clone())),
            catalog,
            stop: CancellationToken::new(),
            started: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
            callback_routes,
            callback_tunnels: CallbackTunnels::default(),
            links: Arc::default(),
        })))
    }

    /// The links to the user's other engines: previews and sign-in callbacks there are reached
    /// over them, and their catalogs are followed from now on. Set once.
    pub fn set_links(&self, links: Arc<PeerLinks>) {
        if self.0.links.set(links.clone()).is_err() {
            tracing::debug!("preview links were already set");
            return;
        }
        let task = tokio::spawn(tunnel::follow_catalogs(
            links,
            self.0.catalog.clone(),
            self.0.stop.child_token(),
        ));
        self.0.tasks.lock().unwrap().push(task);
    }

    /// What serves the tunnels and catalog feeds peers open to this device: register it for
    /// `StreamKind::Tunnel`.
    pub fn tunnels(&self) -> Arc<dyn StreamHandler> {
        Arc::new(tunnel::Tunnels {
            connector: self.0.connector.clone(),
            catalog: self.0.catalog.clone(),
        })
    }

    pub fn catalog(&self) -> &Catalog {
        &self.0.catalog
    }

    /// The registry the agent-login flows publish their callback ports to.
    pub fn callback_routes(&self) -> CallbackRoutes {
        self.0.callback_routes.clone()
    }

    /// Forward loopback `port` on this device to login `login_id`'s callback
    /// on `device` until [`Self::close_login_tunnel`] or `lifetime` passes.
    /// Idempotent per login; a port taken here is an error.
    pub async fn open_login_tunnel(
        &self,
        login_id: &str,
        device: &str,
        port: u16,
        lifetime: Duration,
    ) -> anyhow::Result<()> {
        if self.0.callback_tunnels.contains(login_id) {
            return Ok(());
        }
        let links = self.0.links.get().cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "Connections to other devices aren't available yet — try again in a moment."
            )
        })?;
        let device = device.to_owned();
        let service = login::service_id(login_id);
        let open: login::Opener = Arc::new(move || {
            let (links, device, service) = (links.clone(), device.clone(), service.clone());
            Box::pin(async move {
                Ok(Box::new(tunnel::open(&links, &device, &service).await?) as BoxIo)
            })
        });
        let forwarder = CallbackForwarder::bind(port, lifetime, open).await?;
        self.0.callback_tunnels.insert(login_id, forwarder);
        Ok(())
    }

    pub fn close_login_tunnel(&self, login_id: &str) {
        self.0.callback_tunnels.close(login_id);
    }

    pub async fn start(&self, projects: Projects) {
        if self.0.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let router = proxy::Router {
            catalog: self.0.catalog.clone(),
            local: self.0.connector.clone(),
            links: self.0.links.clone(),
            stop: self.0.stop.child_token(),
        };
        let proxy_catalog = self.0.catalog.clone();
        let proxy_stop = self.0.stop.clone();
        let listener_task = tokio::spawn(async move {
            loop {
                let binding = tokio::select! {
                    _ = proxy_stop.cancelled() => break,
                    binding = serve_proxy(router.clone(), &proxy_stop) => binding,
                };
                match binding {
                    Ok((port, tasks)) => {
                        proxy_catalog.set_proxy_status(port, None);
                        for task in tasks {
                            let _ = task.await;
                        }
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "local previews could not start; retrying");
                        proxy_catalog.set_proxy_status(
                            onicode_proto::PREVIEW_PROXY_PORT,
                            Some(format!("Local previews could not start: {error}")),
                        )
                    }
                }
                tokio::select! { _ = proxy_stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
            }
        });
        self.0.tasks.lock().unwrap().push(listener_task);
        let catalog = self.0.catalog.clone();
        let stop = self.0.stop.clone();
        let scanner = tokio::spawn(async move {
            let mut memory = ProbeMemory::default();
            let mut origins = discovery::Origins::default();
            loop {
                let memory = &mut memory;
                let origins = &mut origins;
                let scan = async {
                    let projects = projects.clone();
                    let taken = std::mem::take(origins);
                    let scanned = tokio::task::spawn_blocking(move || {
                        let mut origins = taken;
                        (candidates(projects(), &mut origins), origins)
                    })
                    .await;
                    let candidates = match scanned {
                        Ok((Some(candidates), kept)) => {
                            *origins = kept;
                            candidates
                        }
                        // A failed enumeration says nothing about what runs:
                        // keep the services already listed.
                        Ok((None, kept)) => {
                            *origins = kept;
                            tracing::debug!(
                                "preview scan could not read processes; keeping the last result"
                            );
                            return;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "preview scan failed; keeping the last result");
                            return;
                        }
                    };
                    let (mut servers, probe) = memory.plan(candidates, Instant::now());
                    let probed: Vec<_> = stream::iter(probe)
                        .map(|(root, listener)| async move {
                            let http = discovery::is_http(listener.address).await;
                            (root, listener, http)
                        })
                        .buffer_unordered(12)
                        .collect()
                        .await;
                    let now = Instant::now();
                    for (root, listener, http) in probed {
                        memory.record(&listener, http, now);
                        if http {
                            servers.push((root, listener));
                        }
                    }
                    if let Err(error) = catalog.replace_local(servers) {
                        tracing::warn!(%error, "could not update preview services");
                    }
                };
                tokio::select! { _ = stop.cancelled() => break, _ = scan => {} }
                tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
            }
        });
        self.0.tasks.lock().unwrap().push(scanner);
    }

    pub fn stop(&self) {
        self.0.stop.cancel();
        self.0.catalog.clear_remote();
    }

    pub async fn shutdown(&self) {
        self.stop();
        let tasks = std::mem::take(&mut *self.0.tasks.lock().unwrap());
        for task in tasks {
            let _ = task.await;
        }
    }
}
/// This device's own services, by id: a preview's dev server, or a sign-in callback (for the
/// peer that started the login only).
struct LocalConnector(Catalog, CallbackRoutes);
#[async_trait::async_trait]
impl Connector for LocalConnector {
    async fn connect(&self, id: &str) -> anyhow::Result<BoxIo> {
        self.connect_from(None, id).await
    }

    async fn connect_from(&self, peer: Option<&str>, id: &str) -> anyhow::Result<BoxIo> {
        // A sign-in callback: only the registered port, only for the device
        // that started the login, only while it runs.
        if let Some(port) = self.1.target(peer, id) {
            return CallbackRoutes::connect(port?).await;
        }
        let route = self
            .0
            .local_route(id)
            .ok_or_else(|| anyhow::anyhow!("preview service stopped"))?;
        // Recheck process identity before dialing: a stopped dev server's port
        // may have been reused since the last scan. Checking the one pid is
        // cheap; enumerating every listener here spawned lsof and ps for each
        // asset a remote page requested, which showed up as memory and CPU
        // churn on the hosting Mac.
        let (pid, started_at) = (route.listener.pid, route.listener.started_at);
        let valid =
            tokio::task::spawn_blocking(move || discovery::same_process(pid, started_at)).await?;
        anyhow::ensure!(valid, "preview process changed; waiting for rediscovery");
        Ok(Box::new(
            tokio::net::TcpStream::connect(route.listener.address).await?,
        ))
    }
}

/// The local preview proxy, listening: on its usual port, or, with that
/// taken (another Onicode engine on this computer has it: the app's and a
/// development one, say), on one the system picks. Every preview address is
/// built from the port it got (`PreviewSnapshot::proxy_port`), so nothing
/// that opens a preview depends on which.
async fn serve_proxy(
    router: proxy::Router,
    stop: &CancellationToken,
) -> anyhow::Result<(u16, Vec<tokio::task::JoinHandle<()>>)> {
    match proxy::serve(
        router.clone(),
        onicode_proto::PREVIEW_PROXY_PORT,
        stop.child_token(),
    )
    .await
    {
        Err(error) if address_in_use(&error) => {
            tracing::info!(
                port = onicode_proto::PREVIEW_PROXY_PORT,
                "the preview proxy's port is taken; listening on another"
            );
            proxy::serve(router, 0, stop.child_token()).await
        }
        served => served,
    }
}

fn address_in_use(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::AddrInUse)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn listener(pid: u32, port: u16) -> discovery::Listener {
        discovery::Listener {
            pid,
            parent: 1,
            cwd: "/work/app".into(),
            args: vec!["node".into()],
            started_at: 1_000,
            address: ([127, 0, 0, 1], port).into(),
            onicode_owned: true,
            origin: Some("chat:chat".into()),
            chat_id: Some("chat".into()),
            signing_in: false,
        }
    }
    #[test]
    fn a_confirmed_http_server_is_never_probed_again() {
        let mut memory = ProbeMemory::default();
        let t0 = Instant::now();
        let (known, probe) = memory.plan(vec![((), listener(7, 8081))], t0);
        assert!(known.is_empty());
        assert_eq!(probe.len(), 1);
        memory.record(&probe[0].1, true, t0);
        for cycle in 1..1_000u64 {
            let now = t0 + Duration::from_secs(2 * cycle);
            let (known, probe) = memory.plan(vec![((), listener(7, 8081))], now);
            assert_eq!(known.len(), 1, "cycle {cycle} must reuse the verdict");
            assert!(probe.is_empty(), "cycle {cycle} must not send a request");
        }
    }
    #[test]
    fn non_http_listeners_back_off_and_reused_ports_are_probed_afresh() {
        let mut memory = ProbeMemory::default();
        let t0 = Instant::now();
        let (_, probe) = memory.plan(vec![((), listener(7, 8081))], t0);
        memory.record(&probe[0].1, false, t0);
        let (_, probe) = memory.plan(vec![((), listener(7, 8081))], t0 + Duration::from_secs(2));
        assert!(probe.is_empty(), "the very next cycle is skipped");
        let (_, probe) = memory.plan(vec![((), listener(7, 8081))], t0 + Duration::from_secs(5));
        assert_eq!(probe.len(), 1, "retried after the initial backoff");
        memory.record(&probe[0].1, false, t0 + Duration::from_secs(5));
        let (_, probe) = memory.plan(vec![((), listener(7, 8081))], t0 + Duration::from_secs(10));
        assert!(probe.is_empty(), "backoff doubled");
        // A different process on the same port carries no verdict over.
        let (_, probe) = memory.plan(vec![((), listener(8, 8081))], t0 + Duration::from_secs(10));
        assert_eq!(probe.len(), 1);
        // The original socket, once absent, is forgotten and re-probed on return.
        memory.record(&probe[0].1, true, t0);
        let (known, probe) =
            memory.plan(vec![((), listener(7, 8081))], t0 + Duration::from_secs(11));
        assert!(known.is_empty());
        assert_eq!(probe.len(), 1);
    }
    #[test]
    fn backoff_is_capped() {
        let mut memory = ProbeMemory::default();
        let mut now = Instant::now();
        for _ in 0..20 {
            memory.record(&listener(7, 8081), false, now);
            now += NOT_HTTP_MAX_BACKOFF;
        }
        let (_, probe) = memory.plan(vec![((), listener(7, 8081))], now);
        assert_eq!(probe.len(), 1, "still retried once per max backoff");
    }
}
