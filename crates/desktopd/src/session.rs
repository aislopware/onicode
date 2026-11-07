//! Clients on desktopd's own QUIC endpoint: one task per link.
//!
//! desktopd binds a [`PeerEndpoint`] on its tailnet addresses at [`PORT`]; the tailnet's whois
//! admits each caller before its handshake, as it does the engine's. A link's control stream
//! carries the envelope of `onicode_desktop_link` ([`Up`] in, [`Down`] out), and its datagrams
//! the media ([`MEDIA`] out, [`FEEDBACK`] in) and the copies of numbered input
//! ([`copy::CHANNEL`] in).
//!
//! A link starts with [`Up::Join`]. While a grant is off, desktopd answers
//! [`Down::NotPermitted`] naming what is missing and keeps the link, telling the client each
//! change ([`Down::Grants`]); once they are all on it welcomes the client ([`Down::Welcome`]).
//! Every stream the client opens is its own capture and encode ([`Pipeline`]). Only the
//! controller's input reaches the Mac ([`crate::seat`]); another client's first input after it
//! lost or never held the controls is answered with one [`Down::NotController`].
//!
//! The Mac's displays are kept awake while any client is joined ([`Awake`]). The controller may
//! draw the curtain ([`crate::curtain`]); every joined client hears where it stands, and before
//! a drawn curtain shows, every running capture of every client resolves its filter again so the
//! shield is left out of the picture ([`Pipeline::refilter`]).

use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use onicode_desktop_link::ctl::DesktopStatus;
use onicode_desktop_link::order::{InputOrder, Numbered};
use onicode_desktop_link::{CAP_DESKTOP, Down, HostGrants, PORT, Up, WIRE, copy};
use onicode_desktop_media::wire::Feedback;
use onicode_desktop_media::wire::channel::{FEEDBACK, MEDIA};
use onicode_desktop_proto::StreamId;
use onicode_desktop_proto::screen::{
    CurtainState, DisplayKey, OpenAsk, ScreenEvent, ScreenFailure, ScreenRequest, Stripe,
    VirtualDisplay,
};
use onicode_net::{Link, NetError, Role, Session, StreamTx, close};
use onicode_peer::{PeerConfig, PeerEndpoint, PeerSession, Sockets};
use onicode_tailnet::Tailnet;
use parking_lot::Mutex;
use tokio::sync::{mpsc, watch};

use crate::curtain::{Curtain, Moved};
use crate::pipeline::{DatagramSink, Opening, PathSample, Pipeline, Refused, StreamEvent};
use crate::platform::{Awake, Platform};
use crate::seat::{ClientId, Seats};
use crate::sized::{self, Asked, Displays, Factory, Sized};
use crate::sound::Sound;

/// How long a link has to say [`Up::Join`] before it is closed.
pub const JOIN_WITHIN: Duration = Duration::from_secs(10);

/// How long a drawn curtain stays unseen while every running capture resolves its filter again
/// to leave it out: one enumeration and one filter update take well under this.
pub const SHOW_CURTAIN_AFTER: Duration = Duration::from_millis(250);

/// Bind desktopd's endpoint on this node's tailnet addresses at [`PORT`].
///
/// Its hello carries [`CAP_DESKTOP`], which a client's dial looks for to know it reached a
/// desktop and not an engine.
///
/// # Errors
///
/// The endpoint's: Tailscale not running, or the port taken.
pub async fn bind(
    tailnet: Arc<dyn Tailnet>,
    sockets: &dyn Sockets,
    device_id: String,
    seed: Option<u64>,
) -> Result<Arc<PeerEndpoint>, onicode_peer::PeerError> {
    let config = PeerConfig {
        port: PORT,
        seed,
        caps: vec![CAP_DESKTOP.to_owned()],
        ..PeerConfig::new(device_id, Role::Engine)
    };
    PeerEndpoint::bind(tailnet, sockets, config).await
}

/// The link's datagrams, as a stream's sink.
#[derive(Clone, Debug)]
pub struct LinkSink(pub Link);

impl DatagramSink for LinkSink {
    fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
        for datagram in datagrams {
            self.0.datagram(MEDIA, datagram).map_err(|e| match e {
                NetError::DatagramTooLarge(_) => Refused::TooLarge,
                _gone => Refused::Closed,
            })?;
        }
        Ok(())
    }

    fn max_size(&self) -> Option<usize> {
        // The channel byte goes in front of each.
        self.0
            .connection()
            .max_datagram_size()
            .map(|max| max.saturating_sub(1))
    }

    fn held(&self) -> usize {
        onicode_net::transport::DATAGRAM_BUFFER
            .saturating_sub(self.0.connection().datagram_send_buffer_space())
    }

    fn cwnd(&self) -> u64 {
        cwnd(&self.0)
    }

    fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

/// The path's congestion window, bytes; `0` before one is known.
fn cwnd(link: &Link) -> u64 {
    link.connection()
        .congestion_state(noq::PathId::ZERO)
        .map_or(0, |cc| cc.metrics().congestion_window)
}

/// What the transport knows of the path, for the rate.
fn path_sample(link: &Link) -> Option<PathSample> {
    let rtt = link.rtt()?;
    Some(PathSample {
        rtt,
        cwnd: cwnd(link),
    })
}

/// desktopd's clients, their seat, the curtain and the displays kept awake for them.
pub struct Host<P: Platform, F: Factory = sized::Cg> {
    name: String,
    grants: watch::Receiver<HostGrants>,
    seats: Mutex<Seats>,
    seat_moved: watch::Sender<u64>,
    next_client: AtomicU64,
    linked: AtomicU32,
    links: Mutex<HashMap<ClientId, Link>>,
    made: Option<Displays<F>>,
    curtain: Curtain<P::Drapes>,
    /// Bumped whenever the curtain is drawn or lifted: every joined client hears where it stands.
    curtain_moved: watch::Sender<u64>,
    /// Bumped when the windows every capture leaves out changed: each joined client's streams
    /// resolve their filters again.
    refilter: watch::Sender<u64>,
    awake: Mutex<Awake>,
    /// The Mac's clipboard, followed by the owner's focused viewer ([`Self::set_clipboard`]);
    /// unset, the clipboard stays out of every session (tests, drawn displays).
    clipboard: std::sync::OnceLock<onicode_clipboard::Hub>,
    platform: PhantomData<fn() -> P>,
}

/// A joined client's share of keeping the displays awake, let go when it is dropped.
struct Joined<'a>(&'a Mutex<Awake>);

impl Drop for Joined<'_> {
    fn drop(&mut self) {
        self.0.lock().leave();
    }
}

fn bump(n: &mut u64) {
    *n = n.wrapping_add(1);
}

impl<P: Platform, F: Factory> std::fmt::Debug for Host<P, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("name", &self.name)
            .field("linked", &self.linked.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// A note for a client's task from its streams.
enum Note {
    /// A stream's event.
    Stream(StreamId, StreamEvent),
    /// A made display's stream is to show another display.
    Switch(StreamId, VirtualDisplay),
}

/// Told when the display made for the client changes size: width, height and scale.
type SizedHook = Box<dyn Fn(u32, u32, Option<f32>) + Send + Sync>;
/// A stream a client opened, and its made display's sizing.
struct Open<P: Platform> {
    pipeline: Pipeline<P>,
    /// The key of the display made for the client.
    key: Option<DisplayKey>,
    sized: Option<SizedHook>,
}

/// One client's state, in its task.
struct Client<P: Platform> {
    id: ClientId,
    owner: bool,
    link: Link,
    tx: Arc<StreamTx>,
    streams: HashMap<StreamId, Open<P>>,
    next_stream: u32,
    order: InputOrder,
    sound: Sound<P>,
    /// It was told it does not hold the controls, since it last lost them.
    told_not_controller: bool,
    notes: mpsc::UnboundedSender<Note>,
    /// The clipboard on this link: the owner's devices only.
    clip: Option<Arc<onicode_clipboard::Session>>,
}

impl<P: Platform> Client<P> {
    /// Send `down`; `false` when the link is gone.
    fn say(&self, down: &Down) -> bool {
        self.tx
            .send_frame(&onicode_desktop_link::encode(down))
            .is_ok()
    }

    fn screen(&self, event: ScreenEvent) -> bool {
        self.say(&Down::Screen(event))
    }

    /// The stream and stripe a media id names.
    fn stream(&self, media: StreamId) -> Option<(&Pipeline<P>, usize)> {
        let (stream, index) = Stripe::stream_of(media);
        self.streams
            .get(&stream)
            .map(|open| (&open.pipeline, index))
    }

    fn opening(
        &mut self,
        target: onicode_desktop_proto::screen::CaptureTarget,
        quality: onicode_desktop_proto::screen::Quality,
    ) -> Opening {
        self.next_stream = self.next_stream.wrapping_add(1).max(1);
        let id = StreamId(self.next_stream);
        let notes = self.notes.clone();
        Opening {
            id,
            target,
            quality,
            sink: Arc::new(LinkSink(self.link.clone())),
            events: Arc::new(move |event: StreamEvent| {
                let _gone = notes.send(Note::Stream(id, event));
            }),
        }
    }

    async fn close_all(&mut self) {
        for (_id, open) in self.streams.drain() {
            open.pipeline.close().await;
        }
    }
}

impl<P: Platform, F: Factory> Host<P, F> {
    /// desktopd as `name`, its grants as `grants` says, with the displays made for clients
    /// where there are (`made`).
    pub fn new(
        name: String,
        grants: watch::Receiver<HostGrants>,
        made: Option<Displays<F>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            grants,
            seats: Mutex::new(Seats::default()),
            seat_moved: watch::Sender::new(0),
            next_client: AtomicU64::new(0),
            linked: AtomicU32::new(0),
            links: Mutex::new(HashMap::new()),
            made,
            curtain: Curtain::default(),
            curtain_moved: watch::Sender::new(0),
            refilter: watch::Sender::new(0),
            awake: Mutex::new(Awake::default()),
            clipboard: std::sync::OnceLock::new(),
            platform: PhantomData,
        })
    }

    /// Follow the Mac's clipboard in the owner's sessions while their window has focus, from
    /// now on; a second call is ignored.
    pub fn set_clipboard(&self, hub: onicode_clipboard::Hub) {
        let _set = self.clipboard.set(hub);
    }

    /// The clipboard on `link` with the owner's `device`, its clips sent on `tx`; `None` for
    /// another's device, or with no clipboard set. Serves the device's fetches until the link
    /// closes.
    fn clipboard_session(
        &self,
        owner: bool,
        device: &str,
        link: &Link,
        tx: &Arc<StreamTx>,
    ) -> Option<Arc<onicode_clipboard::Session>> {
        let hub = self.clipboard.get().filter(|_| owner)?;
        let say_on = Arc::clone(tx);
        let line = onicode_clipboard::link::LinkLine::new(
            link.clone(),
            Box::new(move |clip| {
                say_on
                    .send_frame(&onicode_desktop_link::encode(&Down::Clip(clip)))
                    .is_ok()
            }),
        );
        let session = Arc::new(hub.session(device.to_owned(), line));
        tokio::spawn(onicode_clipboard::link::serve_streams(
            link.clone(),
            Arc::downgrade(&session),
        ));
        Some(session)
    }

    /// Clients linked now.
    #[must_use]
    pub fn clients(&self) -> u32 {
        self.linked.load(Ordering::Relaxed)
    }

    /// The grants as of now.
    #[must_use]
    pub fn grants(&self) -> HostGrants {
        *self.grants.borrow()
    }

    /// What desktopd is and may do, for the engine ([`crate::ctl`]).
    #[must_use]
    pub fn status(&self, addrs: Vec<SocketAddr>) -> DesktopStatus {
        let grants = self.grants();
        DesktopStatus {
            pid: std::process::id(),
            host: self.name.clone(),
            addrs,
            wire: WIRE,
            grants,
            displays: if grants.screen_recording {
                Pipeline::<P>::displays()
            } else {
                Vec::new()
            },
            bundle: crate::bundle(),
            clients: self.clients(),
        }
    }

    /// Close every link and lift the curtain, before desktopd exits to be started again. (A
    /// desktopd that dies without this lifts it too: the window server removes a dead process's
    /// windows.)
    pub fn close_all(&self) {
        let _lifted = self.curtain.lift();
        for (_id, link) in self.links.lock().drain() {
            link.close(close::NORMAL, b"desktopd restarts");
        }
    }

    /// Where the curtain stands.
    #[must_use]
    pub fn curtain(&self) -> CurtainState {
        self.curtain.state()
    }

    /// Viewers joined now: the displays are kept awake while there is one.
    #[must_use]
    pub fn joined(&self) -> u32 {
        self.awake.lock().joined()
    }

    /// Serve every session `sessions` hands over, each on a task of its own, until it ends.
    pub async fn serve(self: Arc<Self>, mut sessions: mpsc::Receiver<PeerSession>) {
        while let Some(session) = sessions.recv().await {
            tokio::spawn(Arc::clone(&self).client(session));
        }
    }

    fn seat_moved(&self) {
        self.seat_moved.send_modify(bump);
    }

    /// The curtain moved: every joined client hears where it stands, and one just drawn shows
    /// once every running capture has had [`SHOW_CURTAIN_AFTER`] to leave it out. Once it shows,
    /// the captures resolve their filters once more, for an enumeration that listed only windows
    /// on screen.
    fn curtain_moved(self: &Arc<Self>, moved: Moved) {
        match moved {
            Moved::Same => return,
            Moved::Raised { generation } => {
                self.refilter.send_modify(bump);
                let host = Arc::clone(self);
                tokio::spawn(async move {
                    tokio::time::sleep(SHOW_CURTAIN_AFTER).await;
                    if host.curtain.show(generation) {
                        host.refilter.send_modify(bump);
                    }
                });
            }
            Moved::Lowered => {}
        }
        self.curtain_moved.send_modify(bump);
    }

    /// Serve one link until it closes.
    pub async fn client(self: Arc<Self>, session: PeerSession) {
        let PeerSession { session, admitted } = session;
        let Session {
            link,
            peer,
            control_tx,
            mut control_rx,
            ..
        } = session;
        let id = self
            .next_client
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.linked.fetch_add(1, Ordering::Relaxed);
        self.links.lock().insert(id, link.clone());
        tracing::info!(client = id, device = %peer.device_id, owner = admitted.grant.owner, "a client linked");
        let joined = tokio::time::timeout(JOIN_WITHIN, control_rx.frame()).await;
        let join = match joined {
            Ok(Ok(Some(frame))) => onicode_desktop_link::decode::<Up>(&frame).ok(),
            _silent => None,
        };
        let served = match join {
            Some(Up::Join {
                wire,
                name,
                control,
                sound,
            }) if wire == WIRE => {
                let (notes, noted) = mpsc::unbounded_channel();
                let tx = Arc::new(control_tx);
                let clip =
                    self.clipboard_session(admitted.grant.owner, &peer.device_id, &link, &tx);
                let client = Client::<P> {
                    id,
                    owner: admitted.grant.owner,
                    link: link.clone(),
                    tx,
                    streams: HashMap::new(),
                    next_stream: 0,
                    order: InputOrder::default(),
                    sound: Sound::new(Arc::new(LinkSink(link.clone()))),
                    told_not_controller: false,
                    notes,
                    clip,
                };
                client.sound.set(sound);
                Arc::clone(&self)
                    .serve_joined(client, &mut control_rx, noted, (name, control))
                    .await;
                true
            }
            Some(Up::Join { wire, .. }) => {
                tracing::info!(client = id, wire, "a client of another wire");
                link.close(
                    close::WRONG_BUILD,
                    format!("desktop wire {WIRE}").as_bytes(),
                );
                false
            }
            _other => {
                link.close(close::PROTOCOL, b"no join");
                false
            }
        };
        if served {
            // Everyone hears the count, and the controls freed when this one held them.
            let _freed = self.seats.lock().leave(id);
            self.seat_moved();
        }
        let moved = self.curtain.went(id);
        self.curtain_moved(moved);
        self.links.lock().remove(&id);
        self.linked.fetch_sub(1, Ordering::Relaxed);
        tracing::info!(client = id, "a client left");
    }

    /// The grants gate, then the joined client's requests, feedback and input.
    async fn serve_joined(
        self: Arc<Self>,
        mut client: Client<P>,
        control_rx: &mut onicode_net::StreamRx,
        mut noted: mpsc::UnboundedReceiver<Note>,
        (name, control): (String, bool),
    ) {
        let mut grants = self.grants.clone();
        if !self.gate(&client, control_rx, &mut grants).await {
            return;
        }
        if self
            .seats
            .lock()
            .join(client.id, name, client.owner, control)
        {
            client.told_not_controller = false;
        }
        self.seat_moved();
        let mut seat = self.seat_moved.subscribe();
        seat.mark_unchanged();
        let mut curtain = self.curtain_moved.subscribe();
        curtain.mark_unchanged();
        let mut refilter = self.refilter.subscribe();
        refilter.mark_unchanged();
        let welcome = Down::Welcome {
            wire: WIRE,
            host: self.name.clone(),
            displays: Pipeline::<P>::displays(),
            grants: *grants.borrow_and_update(),
            seat: self.seats.lock().view(client.id),
        };
        if !client.say(&welcome) {
            return;
        }
        self.awake.lock().join(P::keep_awake);
        let _joined = Joined(&self.awake);
        let drawn = self.curtain.state();
        if drawn != CurtainState::Down && !client.screen(ScreenEvent::Curtain(drawn)) {
            return;
        }
        let mut controlled = self.seats.lock().controls(client.id);
        loop {
            let going = tokio::select! {
                biased;
                frame = control_rx.frame() => match frame {
                    Ok(Some(frame)) => match onicode_desktop_link::decode::<Up>(&frame) {
                        Ok(up) => self.up(&mut client, up).await,
                        Err(e) => {
                            tracing::debug!(client = client.id, error = %e, "a bad frame");
                            true
                        }
                    },
                    Ok(None) | Err(_) => false,
                },
                moved = seat.changed() => {
                    if moved.is_err() {
                        false
                    } else {
                        let view = self.seats.lock().view(client.id);
                        if controlled && !view.you {
                            for open in client.streams.values() {
                                open.pipeline.release_input();
                            }
                            // The curtain is the controller's: it lifts with the controls.
                            let moved = self.curtain.went(client.id);
                            self.curtain_moved(moved);
                        }
                        if view.you != controlled {
                            client.told_not_controller = false;
                        }
                        controlled = view.you;
                        client.say(&Down::Seat(view))
                    }
                },
                changed = grants.changed() => {
                    if changed.is_err() {
                        false
                    } else {
                        let now = *grants.borrow_and_update();
                        client.say(&Down::Grants(now))
                    }
                },
                moved = curtain.changed() => {
                    moved.is_ok() && client.screen(ScreenEvent::Curtain(self.curtain.state()))
                },
                asked = refilter.changed() => {
                    if asked.is_ok() {
                        Self::refilter(client.id, &client.streams).await;
                    }
                    asked.is_ok()
                },
                note = noted.recv() => match note {
                    Some(note) => self.note(&mut client, note).await,
                    None => false,
                },
                datagram = client.link.recv_datagram() => match datagram {
                    Ok((channel, payload)) => {
                        self.datagram(&mut client, channel, &payload, crate::instant()).await;
                        true
                    }
                    Err(_) => false,
                },
            };
            if !going {
                break;
            }
        }
        client.close_all().await;
    }

    /// Have each of client `id`'s `streams` resolve its display's filter again: the windows
    /// every capture leaves out changed.
    async fn refilter(id: ClientId, streams: &HashMap<StreamId, Open<P>>) {
        for (stream, open) in streams {
            if let Err(e) = open.pipeline.refilter().await {
                tracing::warn!(client = id, %stream, error = %e, "a capture kept its old filter");
            }
        }
    }

    /// Hold the client at [`Down::NotPermitted`] until every grant is on; `false` when the link
    /// went first.
    async fn gate(
        &self,
        client: &Client<P>,
        control_rx: &mut onicode_net::StreamRx,
        grants: &mut watch::Receiver<HostGrants>,
    ) -> bool {
        let now = *grants.borrow_and_update();
        if now.complete() {
            return true;
        }
        tracing::info!(client = client.id, missing = ?now.missing(), "a client waits for the grants");
        if !client.say(&Down::NotPermitted {
            missing: now.missing(),
        }) {
            return false;
        }
        loop {
            tokio::select! {
                biased;
                changed = grants.changed() => {
                    if changed.is_err() {
                        return false;
                    }
                    let now = *grants.borrow_and_update();
                    if !client.say(&Down::Grants(now)) {
                        return false;
                    }
                    if now.complete() {
                        return true;
                    }
                }
                frame = control_rx.frame() => {
                    if !matches!(frame, Ok(Some(_))) {
                        return false;
                    }
                }
            }
        }
    }

    /// One frame from the client; `false` to end the link.
    async fn up(self: &Arc<Self>, client: &mut Client<P>, up: Up) -> bool {
        match up {
            Up::Join { .. } => true,
            Up::Screen(request) => self.request(client, request).await,
            Up::Numbered(numbered) => self.numbered(client, numbered).await,
            Up::Control { take } => {
                match self.seats.lock().take(client.id, take) {
                    Ok(true) => self.seat_moved(),
                    Ok(false) => {}
                    Err(why) => tracing::info!(client = client.id, %why, "the controls refused"),
                }
                let view = self.seats.lock().view(client.id);
                client.say(&Down::Seat(view))
            }
            Up::Sound { on } => {
                client.sound.set(on);
                true
            }
            Up::Clip(clip) => {
                if let Some(session) = &client.clip {
                    session.heard(clip);
                }
                true
            }
        }
    }

    /// A numbered request, from the stream or a datagram copy: applied once, in order.
    async fn numbered(self: &Arc<Self>, client: &mut Client<P>, numbered: Numbered) -> bool {
        if !client.order.take(&numbered) {
            return true;
        }
        self.request(client, numbered.request).await
    }

    /// One screen request; `false` to end the link.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per request, read top to bottom"
    )]
    async fn request(self: &Arc<Self>, client: &mut Client<P>, request: ScreenRequest) -> bool {
        match request {
            ScreenRequest::List => {
                let event = if self.grants().screen_recording {
                    ScreenEvent::Listing {
                        displays: Pipeline::<P>::displays(),
                    }
                } else {
                    ScreenEvent::ListFailed {
                        why: ScreenFailure::NotPermitted,
                    }
                };
                client.screen(event)
            }
            ScreenRequest::Open { target, quality } => {
                let opening = client.opening(target, quality);
                let id = opening.id;
                match Pipeline::<P>::open(opening).await {
                    Ok((pipeline, opened)) => {
                        client.streams.insert(
                            id,
                            Open {
                                pipeline,
                                key: None,
                                sized: None,
                            },
                        );
                        client.screen(opened)
                    }
                    Err(e) => {
                        tracing::info!(client = client.id, ?target, error = %e, "a stream not opened");
                        client.screen(ScreenEvent::OpenFailed {
                            asked: OpenAsk::Target(target),
                            why: e.failure(),
                        })
                    }
                }
            }
            ScreenRequest::OpenDisplay {
                key,
                shape,
                quality,
            } => {
                let opening = client.opening(
                    onicode_desktop_proto::screen::CaptureTarget::Display(
                        onicode_desktop_proto::DisplayId(0),
                    ),
                    quality,
                );
                let id = opening.id;
                let asked = Asked {
                    key,
                    shape,
                    quality,
                };
                match sized::open::<P, F>(self.made.as_ref(), asked, opening).await {
                    Ok((pipeline, [display, opened], sized)) => {
                        let sized = sized.map(|sized| {
                            let Sized {
                                resize,
                                mut switches,
                                ..
                            } = sized;
                            let notes = client.notes.clone();
                            tokio::spawn(async move {
                                while let Some((_display, told)) = switches.recv().await {
                                    if notes.send(Note::Switch(id, told)).is_err() {
                                        break;
                                    }
                                }
                            });
                            resize
                        });
                        client.streams.insert(
                            id,
                            Open {
                                pipeline,
                                key: Some(key),
                                sized,
                            },
                        );
                        client.screen(display) && client.screen(opened)
                    }
                    Err(e) => client.screen(ScreenEvent::OpenFailed {
                        asked: OpenAsk::Made(key),
                        why: e.failure(),
                    }),
                }
            }
            ScreenRequest::Close(stream) => {
                if let Some(open) = client.streams.remove(&stream) {
                    open.pipeline.close().await;
                }
                client.screen(ScreenEvent::Closed {
                    stream,
                    reason: "closed".to_owned(),
                })
            }
            ScreenRequest::SetQuality { stream, quality } => {
                if let Some(open) = client.streams.get(&stream) {
                    open.pipeline.set_quality(quality);
                }
                true
            }
            ScreenRequest::Input { stream, input } => {
                if !self.seats.lock().controls(client.id) {
                    if client.told_not_controller {
                        return true;
                    }
                    client.told_not_controller = true;
                    return client.say(&Down::NotController);
                }
                if let Some(open) = client.streams.get(&stream)
                    && let Err(e) = open.pipeline.inject(&input)
                {
                    tracing::debug!(client = client.id, %stream, error = %e, "input refused");
                }
                true
            }
            ScreenRequest::Report { stream, report } => {
                if let Some((pipeline, index)) = client.stream(stream) {
                    let _decided = pipeline.report(index, &report, path_sample(&client.link));
                }
                true
            }
            ScreenRequest::Resize {
                stream,
                width,
                height,
                scale,
            } => {
                if let Some(resize) = client.streams.get(&stream).and_then(|o| o.sized.as_ref()) {
                    resize(width, height, scale);
                }
                true
            }
            ScreenRequest::Curtain { on } => {
                let refused = if !on {
                    None
                } else if !client.owner {
                    Some("only the owner's own devices draw the curtain")
                } else if !self.seats.lock().controls(client.id) {
                    Some("only the device in control draws the curtain")
                } else {
                    None
                };
                if let Some(why) = refused {
                    return client.screen(ScreenEvent::Curtain(CurtainState::Refused {
                        why: why.to_owned(),
                    }));
                }
                match self.curtain.hold(client.id, on) {
                    Ok(Moved::Same) => client.screen(ScreenEvent::Curtain(self.curtain.state())),
                    Ok(moved) => {
                        self.curtain_moved(moved);
                        true
                    }
                    Err(why) => client.screen(ScreenEvent::Curtain(CurtainState::Refused { why })),
                }
            }
        }
    }

    /// One datagram from the client.
    async fn datagram(
        self: &Arc<Self>,
        client: &mut Client<P>,
        channel: u8,
        payload: &[u8],
        arrived: std::time::Instant,
    ) {
        if channel == copy::CHANNEL {
            if let Some(numbered) = copy::decode(payload) {
                let _going = self.numbered(client, numbered).await;
            }
            return;
        }
        if channel != FEEDBACK {
            return;
        }
        let Some(feedback) = Feedback::decode(payload) else {
            return;
        };
        match feedback {
            Feedback::Nack {
                stream,
                frame,
                fragments,
            } => {
                if let Some((pipeline, index)) = client.stream(stream) {
                    pipeline.nack(index, frame, &fragments);
                }
            }
            Feedback::Refresh {
                stream,
                last_good_frame,
                keyframe,
            } => {
                if let Some((pipeline, index)) = client.stream(stream) {
                    pipeline.request_refresh(index, last_good_frame, keyframe);
                }
            }
            Feedback::Clock { stream, sent_us } => {
                if let Some((pipeline, _index)) = client.stream(stream) {
                    pipeline.echo_clock(sent_us, arrived);
                }
            }
            Feedback::Report { stream, report } => {
                if let Some((pipeline, index)) = client.stream(stream) {
                    let _decided = pipeline.report(index, &report, path_sample(&client.link));
                }
            }
        }
    }

    /// A note from one of the client's streams; `false` to end the link.
    async fn note(&self, client: &mut Client<P>, note: Note) -> bool {
        match note {
            Note::Stream(_stream, StreamEvent::Screen(event)) => client.screen(event),
            Note::Stream(stream, StreamEvent::Stopped(e)) => {
                if let Some(open) = client.streams.remove(&stream) {
                    open.pipeline.close().await;
                    return client.screen(ScreenEvent::Closed {
                        stream,
                        reason: e.to_string(),
                    });
                }
                true
            }
            Note::Switch(stream, told) => {
                let display = match told {
                    VirtualDisplay::Made(display) | VirtualDisplay::Physical { display, .. } => {
                        display
                    }
                };
                let Some(open) = client.streams.get(&stream) else {
                    return true;
                };
                if let Some(key) = open.key
                    && !client.screen(ScreenEvent::Display {
                        stream,
                        key,
                        display: told,
                    })
                {
                    return false;
                }
                match open.pipeline.switch_display(display).await {
                    Ok(geometry) => client.screen(geometry),
                    Err(e) => {
                        tracing::warn!(client = client.id, %stream, error = %e, "the stream could not follow its display");
                        true
                    }
                }
            }
        }
    }
}
