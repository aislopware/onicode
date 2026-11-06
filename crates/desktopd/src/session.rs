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

use std::collections::HashMap;
use std::marker::PhantomData;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use onicode_desktop_link::ctl::DesktopStatus;
use onicode_desktop_link::order::{InputOrder, Numbered};
use onicode_desktop_link::{Down, HostGrants, PORT, Up, WIRE, copy};
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

use crate::curtain::Curtain;
use crate::pipeline::{DatagramSink, Opening, PathSample, Pipeline, Refused, StreamEvent};
use crate::platform::Platform;
use crate::seat::{ClientId, Seats};
use crate::sized::{self, Asked, Displays, Factory, Sized};
use crate::sound::Sound;

/// How long a link has to say [`Up::Join`] before it is closed.
pub const JOIN_WITHIN: Duration = Duration::from_secs(10);

/// Bind desktopd's endpoint on this node's tailnet addresses at [`PORT`].
///
/// The hello names no capability: `PeerConfig` sets none, so `CAP_DESKTOP` is not in it.
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

/// desktopd's clients, their seat and the curtain.
pub struct Host<P: Platform, F: Factory = sized::Cg> {
    name: String,
    grants: watch::Receiver<HostGrants>,
    seats: Mutex<Seats>,
    seat_moved: watch::Sender<u64>,
    next_client: AtomicU64,
    linked: AtomicU32,
    links: Mutex<HashMap<ClientId, Link>>,
    made: Option<Displays<F>>,
    curtain: Curtain,
    platform: PhantomData<fn() -> P>,
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
    tx: StreamTx,
    streams: HashMap<StreamId, Open<P>>,
    next_stream: u32,
    order: InputOrder,
    sound: Sound<P>,
    /// It was told it does not hold the controls, since it last lost them.
    told_not_controller: bool,
    notes: mpsc::UnboundedSender<Note>,
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
            sound: Some(self.sound.clock()),
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
            platform: PhantomData,
        })
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

    /// Close every link, before desktopd exits to be started again.
    pub fn close_all(&self) {
        for (_id, link) in self.links.lock().drain() {
            link.close(close::NORMAL, b"desktopd restarts");
        }
    }

    /// Serve every session `sessions` hands over, each on a task of its own, until it ends.
    pub async fn serve(self: Arc<Self>, mut sessions: mpsc::Receiver<PeerSession>) {
        while let Some(session) = sessions.recv().await {
            tokio::spawn(Arc::clone(&self).client(session));
        }
    }

    fn seat_moved(&self) {
        self.seat_moved.send_modify(|n| *n = n.wrapping_add(1));
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
                let client = Client::<P> {
                    id,
                    owner: admitted.grant.owner,
                    link: link.clone(),
                    tx: control_tx,
                    streams: HashMap::new(),
                    next_stream: 0,
                    order: InputOrder::default(),
                    sound: Sound::new(Arc::new(LinkSink(link.clone()))),
                    told_not_controller: false,
                    notes,
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
        self.curtain.went(id);
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
        let mut controlled = self.seats.lock().controls(client.id);
        loop {
            let going = tokio::select! {
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
                datagram = client.link.recv_datagram() => match datagram {
                    Ok((channel, payload)) => {
                        self.datagram(&mut client, channel, &payload, Instant::now()).await;
                        true
                    }
                    Err(_) => false,
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
                note = noted.recv() => match note {
                    Some(note) => self.note(&mut client, note).await,
                    None => false,
                },
            };
            if !going {
                break;
            }
        }
        client.close_all().await;
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
    async fn up(&self, client: &mut Client<P>, up: Up) -> bool {
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
        }
    }

    /// A numbered request, from the stream or a datagram copy: applied once, in order.
    async fn numbered(&self, client: &mut Client<P>, numbered: Numbered) -> bool {
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
    async fn request(&self, client: &mut Client<P>, request: ScreenRequest) -> bool {
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
            ScreenRequest::Focused { stream, focused } => {
                if let Some(open) = client.streams.get(&stream) {
                    open.pipeline.set_focused(focused);
                }
                true
            }
            ScreenRequest::SoundReport(report) => {
                client.sound.report(report);
                true
            }
            ScreenRequest::Curtain { on } => {
                let state = if client.owner {
                    self.curtain.hold(client.id, on)
                } else {
                    CurtainState::Refused {
                        why: "only the owner's own devices draw the curtain".to_owned(),
                    }
                };
                client.screen(ScreenEvent::Curtain(state))
            }
        }
    }

    /// One datagram from the client.
    async fn datagram(
        &self,
        client: &mut Client<P>,
        channel: u8,
        payload: &[u8],
        arrived: Instant,
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
            Feedback::Sound(report) => client.sound.report(report),
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
