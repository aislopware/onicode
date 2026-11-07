//! A session with a Mac's desktop: joining, the controls, the grants, the displays opened, and
//! input, kept up across link drops.
//!
//! [`DesktopSession`] is a handle on a task that owns the link. It dials, joins
//! (`Up::Join`), and turns desktopd's frames into one [`SessionState`] the views watch; a
//! stream desktopd opens gets a [`ScreenHandle`] ([`DesktopSession::screen`]). When the link
//! drops the task redials with backoff ([`Phase::Reconnecting`]) and, joined again, opens the
//! displays that were open: stream ids are the link's, so the views pick the new handles up
//! from the state.
//!
//! Input is numbered (`onicode_desktop_link::order`), sent on the control stream and copied as
//! a datagram ([`onicode_desktop_link::copy::CHANNEL`]), whichever arrives first applied. Pointer
//! moves queued behind each other go as the newest alone.
//!
//! The state is a pure fold (`Book`) of desktopd's frames, tested on its own.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use onicode_desktop_link::broker::DesktopEndpoint;
use onicode_desktop_link::order::Numberer;
use onicode_desktop_link::{Clip, Down, HostGrants, Pane, Seat, Up, WIRE};
use onicode_desktop_proto::DisplayId;
use onicode_desktop_proto::clipboard::PasteAccess;
use onicode_desktop_proto::screen::{
    CaptureTarget, CursorShape, CurtainState, DisplayInfo, DisplayKey, DisplayShape, OpenAsk,
    Quality, ScreenEvent, ScreenFailure, ScreenInput, ScreenRequest, SourceState, StreamId,
    VideoCodec, VirtualDisplay,
};
use onicode_net::Link;
use onicode_net::redial::Redial;
use parking_lot::Mutex;
use tokio::sync::{mpsc, watch};

use crate::decode::{PlatformDecoder, VideoDecode};
use crate::router::ScreenRouter;
use crate::screen::{self, ScreenHandle};
use crate::sound::{self, Sound};
use crate::{DesktopClient, DesktopError, link};

/// Who this device is to the Mac, and what it wants on joining.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Join {
    /// What the person calls this device (`MacBook Pro`, `iPhone`): the seat names it.
    pub name: String,
    /// Take the controls on joining.
    pub control: bool,
    /// Hear the Mac's sound.
    pub sound: bool,
}

/// What to show.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Open {
    /// One of the Mac's displays, whole.
    Display(DisplayId),
    /// A display the Mac makes for this device at `shape`, kept under this device's `key`
    /// ([`crate::display::key`]); the Mac streams a physical one instead when it cannot.
    Made {
        /// This device's key.
        key: DisplayKey,
        /// Its size, scale and refresh.
        shape: DisplayShape,
    },
}

impl Open {
    /// Whether desktopd's `asked` names this.
    fn is(&self, asked: &OpenAsk) -> bool {
        match (self, asked) {
            (Self::Display(id), OpenAsk::Target(CaptureTarget::Display(asked))) => id == asked,
            (Self::Made { key, .. }, OpenAsk::Made(asked)) => key == asked,
            _ => false,
        }
    }

    /// Whether `other` asks for the same display: the same physical one, or one made under the
    /// same key whatever its shape.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Display(a), Self::Display(b)) => a == b,
            (Self::Made { key: a, .. }, Self::Made { key: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// Where the session is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Dialling, or joined and not yet welcomed.
    Connecting,
    /// Welcomed: displays may be opened.
    Joined,
    /// The Mac may not stream: these System Settings panes are off there. The link stays; the
    /// session joins again once they are on.
    NotPermitted {
        /// What is off.
        missing: Vec<Pane>,
    },
    /// The link dropped; the next dial is in `retry_in`.
    Reconnecting {
        /// How long until it.
        retry_in: Duration,
    },
    /// Over for good: shut down, or a Mac this app cannot speak to.
    Closed {
        /// Why, for the person.
        reason: String,
    },
}

/// One stream desktopd has open for this device.
#[derive(Clone, PartialEq, Debug)]
pub struct StreamView {
    /// The stream ([`DesktopSession::screen`]).
    pub stream: StreamId,
    /// The display it shows.
    pub display: Option<DisplayId>,
    /// Its size in pixels.
    pub width: u32,
    /// Its height in pixels.
    pub height: u32,
    /// Pixels per point on the Mac.
    pub scale: f32,
    /// Whether the display draws (a locked Mac or a sleeping display does not).
    pub source: SourceState,
    /// The bitrate desktopd's rate control settled on.
    pub rate_bps: u32,
    /// For [`Open::Made`], the display made or why a physical one is streamed instead.
    pub made: Option<VirtualDisplay>,
}

/// Everything a view shows of a session.
#[derive(Clone, PartialEq, Debug)]
pub struct SessionState {
    /// Where the session is.
    pub phase: Phase,
    /// What the Mac calls itself.
    pub host: String,
    /// The Mac's displays.
    pub displays: Vec<DisplayInfo>,
    /// What desktopd may do there.
    pub grants: HostGrants,
    /// Who holds the controls.
    pub seat: Seat,
    /// The streams open now.
    pub streams: Vec<StreamView>,
    /// The privacy curtain, once desktopd has said.
    pub curtain: Option<CurtainState>,
    /// The newest open desktopd refused, and why, until the next open is asked.
    pub failure: Option<(OpenAsk, ScreenFailure)>,
    /// desktopd refused this device's input since it last held the controls.
    pub input_refused: bool,
    /// Whether the Mac may read its pasteboard unasked (Paste from Other Apps for Onicode
    /// Desktop), as it last said while its clipboard followed this window; `None` before.
    pub paste: Option<PasteAccess>,
}

impl SessionState {
    fn new(endpoint: &DesktopEndpoint) -> Self {
        Self {
            phase: Phase::Connecting,
            host: endpoint.host.clone(),
            displays: endpoint.displays.clone(),
            grants: endpoint.grants,
            seat: Seat::default(),
            streams: Vec::new(),
            curtain: None,
            failure: None,
            input_refused: false,
            paste: None,
        }
    }

    fn view_mut(&mut self, stream: StreamId) -> Option<&mut StreamView> {
        self.streams.iter_mut().find(|view| view.stream == stream)
    }
}

/// What a frame from desktopd asks of the task beyond the state.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Effect {
    /// Welcomed: open what was wanted.
    Reopen,
    /// A stream opened: start receiving it.
    Spawn { stream: StreamId, codec: VideoCodec },
    /// A stream closed.
    Drop(StreamId),
    /// The pointer's picture over a stream changed.
    Cursor {
        stream: StreamId,
        shape: Option<CursorShape>,
    },
    /// Whether a stream's display draws.
    Source { stream: StreamId, live: bool },
    /// The Mac speaks another session wire: stop.
    WrongWire(u32),
    /// The Mac's clipboard said something: its copies.
    Clip(Clip),
}

/// A display wanted open, and the stream it is open on over this link.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Want {
    pub(crate) open: Open,
    pub(crate) quality: Quality,
    pub(crate) stream: Option<StreamId>,
}

/// The session's model: the state the views see, what is wanted open, and the displays made
/// that their `Opened` has not come for yet. A pure fold of desktopd's frames and the views'
/// asks.
#[derive(Debug)]
pub(crate) struct Book {
    pub(crate) state: SessionState,
    pub(crate) wanted: Vec<Want>,
    /// `ScreenEvent::Display` comes before its `Opened`.
    made: HashMap<StreamId, (DisplayKey, VirtualDisplay)>,
}

impl Book {
    pub(crate) fn new(endpoint: &DesktopEndpoint) -> Self {
        Self {
            state: SessionState::new(endpoint),
            wanted: Vec::new(),
            made: HashMap::new(),
        }
    }

    /// The link dropped: nothing is open over it any more.
    pub(crate) fn unlinked(&mut self) {
        for want in &mut self.wanted {
            want.stream = None;
        }
        self.made.clear();
        self.state.streams.clear();
        self.state.curtain = None;
        self.state.paste = None;
    }

    /// A view asked for `open`: the request to send, or `None` when it is wanted already. A
    /// display asked for twice is asked of desktopd once: the want it has is open over this
    /// link or opens again on the next welcome, and a second would be a second capture.
    pub(crate) fn open(&mut self, open: Open, quality: Quality) -> Option<ScreenRequest> {
        self.state.failure = None;
        if let Some(want) = self.wanted.iter_mut().find(|want| want.open.same(&open)) {
            want.open = open;
            want.quality = quality;
            return None;
        }
        self.wanted.push(Want {
            open,
            quality,
            stream: None,
        });
        Some(request(open, quality))
    }

    /// A view let `stream` go.
    pub(crate) fn close(&mut self, stream: StreamId) {
        self.wanted.retain(|want| want.stream != Some(stream));
    }

    /// Fold one frame in.
    pub(crate) fn down(&mut self, down: Down) -> Vec<Effect> {
        let state = &mut self.state;
        match down {
            Down::Welcome {
                wire,
                host,
                displays,
                grants,
                seat,
            } => {
                if wire != WIRE {
                    return vec![Effect::WrongWire(wire)];
                }
                state.phase = Phase::Joined;
                state.host = host;
                state.displays = displays;
                state.grants = grants;
                state.input_refused &= !seat.you;
                state.seat = seat;
                vec![Effect::Reopen]
            }
            Down::NotPermitted { missing } => {
                state.phase = Phase::NotPermitted { missing };
                Vec::new()
            }
            Down::Grants(grants) => {
                state.grants = grants;
                Vec::new()
            }
            Down::Seat(seat) => {
                state.input_refused &= !seat.you;
                state.seat = seat;
                Vec::new()
            }
            Down::NotController => {
                state.input_refused = true;
                Vec::new()
            }
            Down::Clip(Clip::Access(access)) => {
                state.paste = Some(access);
                Vec::new()
            }
            Down::Clip(clip) => vec![Effect::Clip(clip)],
            Down::Screen(event) => self.screen(event),
        }
    }

    fn screen(&mut self, event: ScreenEvent) -> Vec<Effect> {
        let state = &mut self.state;
        match event {
            ScreenEvent::Listing { displays } => state.displays = displays,
            ScreenEvent::Opened {
                stream,
                target,
                codec,
                width,
                height,
                scale,
                stripes: _,
            } => {
                let made = self.made.remove(&stream);
                let CaptureTarget::Display(display) = target;
                let want = self.wanted.iter_mut().find(|want| {
                    want.stream.is_none()
                        && match (&want.open, made) {
                            (Open::Made { key, .. }, Some((made_key, _))) => *key == made_key,
                            (Open::Display(id), None) => *id == display,
                            _ => false,
                        }
                });
                if let Some(want) = want {
                    want.stream = Some(stream);
                }
                state.streams.retain(|view| view.stream != stream);
                state.streams.push(StreamView {
                    stream,
                    display: Some(display),
                    width,
                    height,
                    scale,
                    source: SourceState::Live,
                    rate_bps: 0,
                    made: made.map(|(_, display)| display),
                });
                return vec![Effect::Spawn { stream, codec }];
            }
            ScreenEvent::Closed { stream, reason } => {
                tracing::debug!(stream = stream.0, %reason, "desktopd closed a stream");
                state.streams.retain(|view| view.stream != stream);
                self.wanted.retain(|want| want.stream != Some(stream));
                return vec![Effect::Drop(stream)];
            }
            ScreenEvent::Geometry {
                stream,
                width,
                height,
                stripes: _,
            } => {
                if let Some(view) = state.view_mut(stream) {
                    view.width = width;
                    view.height = height;
                }
            }
            ScreenEvent::Cursor { stream, shape } => {
                return vec![Effect::Cursor { stream, shape }];
            }
            ScreenEvent::Source {
                stream,
                state: source,
            } => {
                if let Some(view) = state.view_mut(stream) {
                    view.source = source;
                }
                return vec![Effect::Source {
                    stream,
                    live: source == SourceState::Live,
                }];
            }
            ScreenEvent::Rate {
                stream, target_bps, ..
            } => {
                if let Some(view) = state.view_mut(stream) {
                    view.rate_bps = target_bps;
                }
            }
            ScreenEvent::Display {
                stream,
                key,
                display,
            } => {
                if let Some(view) = state.view_mut(stream) {
                    view.made = Some(display);
                } else {
                    self.made.insert(stream, (key, display));
                }
            }
            ScreenEvent::OpenFailed { asked, why } => {
                if let Some(at) = self
                    .wanted
                    .iter()
                    .position(|want| want.stream.is_none() && want.open.is(&asked))
                {
                    self.wanted.remove(at);
                }
                state.failure = Some((asked, why));
            }
            ScreenEvent::ListFailed { why } => {
                tracing::debug!(?why, "desktopd could not list its displays");
            }
            ScreenEvent::Curtain(curtain) => state.curtain = Some(curtain),
        }
        Vec::new()
    }
}

/// The request that opens `open`.
fn request(open: Open, quality: Quality) -> ScreenRequest {
    match open {
        Open::Display(id) => ScreenRequest::Open {
            target: CaptureTarget::Display(id),
            quality,
        },
        Open::Made { key, shape } => ScreenRequest::OpenDisplay {
            key,
            shape,
            quality,
        },
    }
}

/// What a view asks of the session's task.
#[derive(Debug)]
enum Command {
    Open(Open, Quality),
    Close(StreamId),
    Input(StreamId, ScreenInput),
    SetQuality(StreamId, Quality),
    Resize {
        stream: StreamId,
        width: u32,
        height: u32,
        scale: Option<f32>,
    },
    Focused(StreamId, bool),
    Control(bool),
    Sound(bool),
    Curtain(bool),
    Clipboard(Option<onicode_clipboard::Hub>),
    ClipboardFocus(bool),
    Shutdown,
}

/// A session with a Mac's desktop, decoding with `D`. Clones share it; it ends with
/// [`Self::shutdown`] or when the last clone is dropped.
pub struct DesktopSession<D: VideoDecode = PlatformDecoder> {
    shared: Arc<Shared<D>>,
}

impl<D: VideoDecode> Clone for DesktopSession<D> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// The streams' handles, by stream.
type Screens<D> = Arc<Mutex<HashMap<StreamId, ScreenHandle<<D as VideoDecode>::Image>>>>;

struct Shared<D: VideoDecode> {
    commands: mpsc::UnboundedSender<Command>,
    state: watch::Receiver<Arc<SessionState>>,
    screens: Screens<D>,
}

impl<D: VideoDecode> Drop for Shared<D> {
    /// The task closes the link itself on the command: aborting it would drop the link without
    /// a word to desktopd.
    fn drop(&mut self) {
        let _ended = self.commands.send(Command::Shutdown);
    }
}

impl<D: VideoDecode> std::fmt::Debug for DesktopSession<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopSession")
            .field("state", &*self.shared.state.borrow())
            .finish_non_exhaustive()
    }
}

impl<D: VideoDecode> DesktopSession<D> {
    pub(crate) fn start(client: Arc<DesktopClient>, endpoint: DesktopEndpoint, join: Join) -> Self {
        let (commands, rx) = mpsc::unbounded_channel();
        let book = Book::new(&endpoint);
        let (state_tx, state) = watch::channel(Arc::new(book.state.clone()));
        let screens = Arc::new(Mutex::new(HashMap::new()));
        let runtime = tokio::runtime::Handle::current();
        let task = Task::<D> {
            client,
            endpoint,
            join,
            book,
            commands: rx,
            state: state_tx,
            screens: Arc::clone(&screens),
            runtime: runtime.clone(),
            clipboard: None,
            clipboard_focused: false,
        };
        drop(runtime.spawn(task.run()));
        Self {
            shared: Arc::new(Shared {
                commands,
                state,
                screens,
            }),
        }
    }

    fn send(&self, command: Command) {
        let _ended = self.shared.commands.send(command);
    }

    /// The session's state; `changed().await` wakes on every change.
    #[must_use]
    pub fn state(&self) -> watch::Receiver<Arc<SessionState>> {
        self.shared.state.clone()
    }

    /// Open `open` at `quality`. Kept wanted across link drops until closed or refused, and
    /// opened again by the session itself on each welcome, so a view asks once; the stream
    /// shows in [`SessionState::streams`] once desktopd opens it. Asking again for a display
    /// already wanted asks desktopd nothing.
    pub fn open(&self, open: Open, quality: Quality) {
        self.send(Command::Open(open, quality));
    }

    /// Close `stream`.
    pub fn close(&self, stream: StreamId) {
        self.send(Command::Close(stream));
    }

    /// Input to `stream`, in its pixels. Dropped while the link is down: stale input is worse
    /// than none.
    pub fn input(&self, stream: StreamId, input: ScreenInput) {
        self.send(Command::Input(stream, input));
    }

    /// Change `stream`'s quality.
    pub fn set_quality(&self, stream: StreamId, quality: Quality) {
        self.send(Command::SetQuality(stream, quality));
    }

    /// The view of `stream` is now `width` × `height` pixels, on a screen of `scale` (`None`:
    /// the screen did not change). A display made for this device follows.
    pub fn resize(&self, stream: StreamId, width: u32, height: u32, scale: Option<f32>) {
        self.send(Command::Resize {
            stream,
            width,
            height,
            scale,
        });
    }

    /// Whether the view of `stream` has the keyboard.
    pub fn focused(&self, stream: StreamId, focused: bool) {
        self.send(Command::Focused(stream, focused));
    }

    /// Take the controls, or give them up.
    pub fn take_control(&self, take: bool) {
        self.send(Command::Control(take));
    }

    /// Hear the Mac's sound here, or stop.
    pub fn set_sound(&self, on: bool) {
        self.send(Command::Sound(on));
    }

    /// Draw the privacy curtain over the Mac's own screen, or lift it.
    pub fn curtain(&self, on: bool) {
        self.send(Command::Curtain(on));
    }

    /// Follow this device's clipboard `hub` with the Mac's while the window has focus
    /// ([`Self::clipboard_focus`]); `None` keeps the clipboard out of the session.
    pub fn clipboard(&self, hub: Option<onicode_clipboard::Hub>) {
        self.send(Command::Clipboard(hub));
    }

    /// The window showing the Mac gained or lost focus: while it has it, the clipboard follows
    /// the session both ways. Kept across link drops.
    pub fn clipboard_focus(&self, focused: bool) {
        self.send(Command::ClipboardFocus(focused));
    }

    /// The stream `stream`'s pictures, while it is open.
    #[must_use]
    pub fn screen(&self, stream: StreamId) -> Option<ScreenHandle<D::Image>> {
        self.shared.screens.lock().get(&stream).cloned()
    }

    /// End the session: the link closes, and the state says [`Phase::Closed`].
    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
    }
}

/// How a link ended.
enum Ended {
    /// It dropped: dial again.
    Dropped,
    /// Over for good.
    Closed(String),
}

/// The session's task: owns the link, the router and the model.
struct Task<D: VideoDecode> {
    client: Arc<DesktopClient>,
    endpoint: DesktopEndpoint,
    join: Join,
    book: Book,
    commands: mpsc::UnboundedReceiver<Command>,
    state: watch::Sender<Arc<SessionState>>,
    screens: Screens<D>,
    runtime: tokio::runtime::Handle,
    /// This device's clipboard, when the session follows it.
    clipboard: Option<onicode_clipboard::Hub>,
    /// The window has focus: the clipboard follows the Mac while it does.
    clipboard_focused: bool,
}

/// One link's own state. Stream ids are the link's, so each link routes on a router of its
/// own: a view still holding a handle of the last link lets go of it there.
struct Linked {
    link: Link,
    router: ScreenRouter,
    control: Arc<onicode_net::StreamTx>,
    numberer: Numberer,
    sound: Option<Sound>,
    /// The clipboard over this link, with the Mac's device at its other end.
    clip: Option<Arc<onicode_clipboard::Session>>,
    /// The Mac's device id, from its hello.
    peer: String,
}

impl<D: VideoDecode> Task<D> {
    async fn run(mut self) {
        if self.endpoint.wire != WIRE {
            let reason = wrong_wire(self.endpoint.wire);
            return self.close(reason);
        }
        let mut redial = Redial::default();
        loop {
            self.set_phase(Phase::Connecting);
            match link::dial(&self.client, &self.endpoint).await {
                Ok(session) => {
                    redial.linked(tokio::time::Instant::now());
                    match self.linked(session).await {
                        Ended::Dropped => {}
                        Ended::Closed(reason) => return self.close(reason),
                    }
                }
                Err(e @ (DesktopError::NotADesktop(_) | DesktopError::WrongWire { .. })) => {
                    return self.close(e.to_string());
                }
                Err(e) => tracing::debug!(error = %e, "desktop unreachable"),
            }
            let retry_in = redial.next(tokio::time::Instant::now());
            self.set_phase(Phase::Reconnecting { retry_in });
            let wait = onicode_net::redial::wait(retry_in);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    biased;
                    woke = &mut wait => {
                        if woke {
                            redial.reset();
                        }
                        break;
                    }
                    command = self.commands.recv() => {
                        let Some(command) = command else { return self.close("closed".into()) };
                        if let Some(reason) = self.offline(command) {
                            return self.close(reason);
                        }
                    }
                }
            }
        }
    }

    fn publish(&self) {
        self.state.send_replace(Arc::new(self.book.state.clone()));
    }

    fn set_phase(&mut self, phase: Phase) {
        self.book.state.phase = phase;
        self.publish();
    }

    fn close(&mut self, reason: String) {
        self.book.unlinked();
        self.screens.lock().clear();
        self.set_phase(Phase::Closed { reason });
    }

    /// A command while there is no link: the model takes it, the Mac hears nothing. `Some`
    /// with a reason to end the session.
    fn offline(&mut self, command: Command) -> Option<String> {
        match command {
            Command::Open(open, quality) => {
                let _later = self.book.open(open, quality);
            }
            Command::Close(stream) => self.book.close(stream),
            Command::SetQuality(stream, quality) => self.want_quality(stream, quality),
            Command::Resize {
                stream,
                width,
                height,
                scale,
            } => self.want_shape(stream, width, height, scale),
            Command::Control(take) => self.join.control = take,
            Command::Sound(on) => self.join.sound = on,
            Command::Clipboard(hub) => self.clipboard = hub,
            Command::ClipboardFocus(focused) => self.clipboard_focused = focused,
            Command::Input(..) | Command::Focused(..) | Command::Curtain(_) => {}
            Command::Shutdown => return Some("shut down".into()),
        }
        None
    }

    fn want_quality(&mut self, stream: StreamId, quality: Quality) {
        if let Some(want) = self
            .book
            .wanted
            .iter_mut()
            .find(|w| w.stream == Some(stream))
        {
            want.quality = quality;
        }
    }

    fn want_shape(&mut self, stream: StreamId, width: u32, height: u32, scale: Option<f32>) {
        let Some(want) = self
            .book
            .wanted
            .iter_mut()
            .find(|w| w.stream == Some(stream))
        else {
            return;
        };
        if let Open::Made { shape, .. } = &mut want.open {
            shape.width = width;
            shape.height = height;
            if let Some(scale) = scale {
                shape.scale = scale;
            }
        }
    }

    /// Run one link until it drops or the session ends.
    async fn linked(&mut self, session: onicode_net::hello::Session) -> Ended {
        let onicode_net::hello::Session {
            link,
            peer,
            control_tx,
            control_rx,
            ..
        } = session;
        let (downs_tx, mut downs) = mpsc::unbounded_channel();
        let reader = self.runtime.spawn(link::read_control(control_rx, downs_tx));
        let router = ScreenRouter::new();
        let datagrams = self
            .runtime
            .spawn(link::read_datagrams(link.clone(), router.clone()));
        let mut linked = Linked {
            link: link.clone(),
            router,
            control: Arc::new(control_tx),
            numberer: Numberer::default(),
            sound: None,
            clip: None,
            peer: peer.device_id.clone(),
        };
        linked.clip = self.clip_session(&linked);
        let join = Up::Join {
            wire: WIRE,
            name: self.join.name.clone(),
            control: self.join.control,
            sound: self.join.sound,
        };
        let ended = if link::send(&linked.control, &join) {
            self.serve(&mut linked, &mut downs).await
        } else {
            Ended::Dropped
        };
        reader.abort();
        datagrams.abort();
        drop(linked.sound.take());
        if let Ended::Closed(_) = &ended {
            linked.link.close(onicode_net::close::NORMAL, b"");
        }
        self.screens.lock().clear();
        linked.router.clear();
        self.book.unlinked();
        self.publish();
        ended
    }

    async fn serve(
        &mut self,
        linked: &mut Linked,
        downs: &mut mpsc::UnboundedReceiver<Down>,
    ) -> Ended {
        let link = linked.link.clone();
        let closed = link.closed();
        tokio::pin!(closed);
        loop {
            tokio::select! {
                biased;
                _why = &mut closed => return Ended::Dropped,
                command = self.commands.recv() => {
                    let Some(command) = command else {
                        return Ended::Closed("closed".into());
                    };
                    let mut batch = vec![command];
                    while let Ok(command) = self.commands.try_recv() {
                        batch.push(command);
                    }
                    for command in coalesce(batch) {
                        if let Some(ended) = self.command(linked, command) {
                            return ended;
                        }
                    }
                    self.publish();
                }
                down = downs.recv() => {
                    let Some(down) = down else { return Ended::Dropped };
                    if let Some(ended) = self.down(linked, down) {
                        return ended;
                    }
                    // Whatever else came meanwhile, before one publish.
                    while let Ok(down) = downs.try_recv() {
                        if let Some(ended) = self.down(linked, down) {
                            return ended;
                        }
                    }
                    self.publish();
                }
            }
        }
    }

    /// Fold `down` in and act on what it asks. `Some` when the session ends.
    fn down(&mut self, linked: &mut Linked, down: Down) -> Option<Ended> {
        for effect in self.book.down(down) {
            match effect {
                Effect::WrongWire(theirs) => return Some(Ended::Closed(wrong_wire(theirs))),
                Effect::Reopen => {
                    let asks: Vec<ScreenRequest> = self
                        .book
                        .wanted
                        .iter()
                        .filter(|want| want.stream.is_none())
                        .map(|want| request(want.open, want.quality))
                        .collect();
                    for ask in asks {
                        if !link::send(&linked.control, &Up::Screen(ask)) {
                            return Some(Ended::Dropped);
                        }
                    }
                    if self.join.sound && linked.sound.is_none() {
                        linked.sound = Some(self.sound(linked));
                    }
                    if self.clipboard_focused
                        && let Some(clip) = &linked.clip
                        && !clip.focus(true)
                    {
                        return Some(Ended::Dropped);
                    }
                }
                Effect::Clip(clip) => {
                    if let Some(session) = &linked.clip {
                        session.heard(clip);
                    }
                }
                Effect::Spawn { stream, codec } => {
                    // The one it replaces lets go of its route before the new one takes it.
                    drop(self.screens.lock().remove(&stream));
                    let handle = screen::spawn_screen::<D>(
                        &self.runtime,
                        &linked.router,
                        stream,
                        codec,
                        link::uplink(&linked.link),
                    );
                    self.screens.lock().insert(stream, handle);
                }
                Effect::Drop(stream) => {
                    self.screens.lock().remove(&stream);
                }
                Effect::Cursor { stream, shape } => {
                    if let Some(handle) = self.screens.lock().get(&stream) {
                        handle.set_cursor_shape(shape);
                    }
                }
                Effect::Source { stream, live } => {
                    if let Some(handle) = self.screens.lock().get(&stream) {
                        handle.set_source_live(live);
                    }
                }
            }
        }
        None
    }

    /// The clipboard over `linked`, when the session follows one: its clips go up on the
    /// control stream, and the Mac's fetches are served until the link closes.
    fn clip_session(&self, linked: &Linked) -> Option<Arc<onicode_clipboard::Session>> {
        let hub = self.clipboard.as_ref()?;
        let control = Arc::clone(&linked.control);
        let line = onicode_clipboard::link::LinkLine::new(
            linked.link.clone(),
            Box::new(move |clip| link::send(&control, &Up::Clip(clip))),
        );
        let session = Arc::new(hub.session(linked.peer.clone(), line));
        drop(self.runtime.spawn(onicode_clipboard::link::serve_streams(
            linked.link.clone(),
            Arc::downgrade(&session),
        )));
        Some(session)
    }

    /// Tell the Mac whether the window has focus, once welcomed. `Some` when the link is gone.
    fn clipboard_focus(&self, linked: &Linked) -> Option<Ended> {
        if self.book.state.phase != Phase::Joined {
            return None;
        }
        let clip = linked.clip.as_ref()?;
        (!clip.focus(self.clipboard_focused)).then_some(Ended::Dropped)
    }

    fn sound(&self, linked: &Linked) -> Sound {
        Sound::spawn(
            &self.runtime,
            &linked.router,
            sound::platform_player,
            false,
            link::feedback(&linked.link),
        )
    }

    /// Act on a view's ask over the link. `Some` when the session ends or the link is gone.
    fn command(&mut self, linked: &mut Linked, command: Command) -> Option<Ended> {
        let up = match command {
            Command::Open(open, quality) => Up::Screen(self.book.open(open, quality)?),
            Command::Close(stream) => {
                self.book.close(stream);
                Up::Screen(ScreenRequest::Close(stream))
            }
            Command::Input(stream, input) => {
                return self.numbered(linked, ScreenRequest::Input { stream, input });
            }
            Command::SetQuality(stream, quality) => {
                self.want_quality(stream, quality);
                return self.numbered(linked, ScreenRequest::SetQuality { stream, quality });
            }
            Command::Resize {
                stream,
                width,
                height,
                scale,
            } => {
                self.want_shape(stream, width, height, scale);
                Up::Screen(ScreenRequest::Resize {
                    stream,
                    width,
                    height,
                    scale,
                })
            }
            Command::Focused(stream, focused) => {
                Up::Screen(ScreenRequest::Focused { stream, focused })
            }
            Command::Control(take) => {
                self.join.control = take;
                Up::Control { take }
            }
            Command::Sound(on) => {
                self.join.sound = on;
                if !on {
                    drop(linked.sound.take());
                } else if linked.sound.is_none() && self.book.state.phase == Phase::Joined {
                    linked.sound = Some(self.sound(linked));
                }
                Up::Sound { on }
            }
            Command::Curtain(on) => Up::Screen(ScreenRequest::Curtain { on }),
            Command::Clipboard(hub) => {
                self.clipboard = hub;
                if let Some(old) = linked.clip.take() {
                    let _told = old.focus(false);
                }
                linked.clip = self.clip_session(linked);
                return self.clipboard_focus(linked);
            }
            Command::ClipboardFocus(focused) => {
                if focused == self.clipboard_focused {
                    return None;
                }
                self.clipboard_focused = focused;
                return self.clipboard_focus(linked);
            }
            Command::Shutdown => return Some(Ended::Closed("shut down".into())),
        };
        (!link::send(&linked.control, &up)).then_some(Ended::Dropped)
    }

    /// Send a numbered request on the control stream, and a copy of it as a datagram.
    fn numbered(&self, linked: &mut Linked, request: ScreenRequest) -> Option<Ended> {
        if self.book.state.phase != Phase::Joined {
            return None;
        }
        let numbered = linked.numberer.number(request)?;
        linked.link.copy(
            onicode_desktop_link::copy::CHANNEL,
            &onicode_desktop_link::copy::encode(&numbered),
        );
        (!link::send(&linked.control, &Up::Numbered(numbered))).then_some(Ended::Dropped)
    }
}

fn wrong_wire(theirs: u32) -> String {
    DesktopError::WrongWire { ours: WIRE, theirs }.to_string()
}

/// `batch` with every pointer move dropped that a newer move of the same stream follows with
/// nothing else of that stream between: the newest position is the one that matters.
fn coalesce(batch: Vec<Command>) -> Vec<Command> {
    let mut moved_after: HashSet<StreamId> = HashSet::new();
    let mut kept: Vec<Command> = batch
        .into_iter()
        .rev()
        .filter(|command| match command {
            Command::Input(stream, ScreenInput::Move { .. }) => moved_after.insert(*stream),
            Command::Input(stream, _) => {
                moved_after.remove(stream);
                true
            }
            _ => true,
        })
        .collect();
    kept.reverse();
    kept
}

#[cfg(test)]
mod tests;
