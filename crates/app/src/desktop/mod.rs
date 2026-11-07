//! A Mac's desktop in a window of its own: the picture, letterboxed on a dark stage, and this
//! Mac's keyboard, pointer and trackpad on it (slopty-ui's `screen.rs`, `screen/glass.rs`,
//! `screen/keyboard.rs`, `screen/health.rs`, `screen/controls.rs` and `workspace/desktop.rs`,
//! as one view; zeron's `crates/ui/src/desktop`, `git show f01736b3^:crates/ui/src/desktop`).
//!
//! It opens from a machine: the sidebar's Machines rows and Settings → Connections
//! ([`open_window`]). Opening is two steps. The view asks the Mac's engine for the desktop
//! (`OpenDesktop`, through the app's own engine, which forwards it to another Mac's), which
//! answers with where the Mac's `onicode-desktopd` listens; `DesktopClient::connect` then dials
//! desktopd directly over the tailnet and keeps the link up. The session's state
//! (`SessionState`) drives everything the view shows: the welcome, who holds the controls, the
//! grants the Mac lacks, the stream.
//!
//! The picture never takes a GPUI frame: each decoded picture goes from the decoder's thread to
//! a `VideoLayer` in a native host (`glass`); the view only places the layer. The Mac's
//! pointer is this Mac's system cursor, wearing the Mac's cursor picture. Everything else the
//! view draws is its own state, and every change to it notifies the view (retained mode): the
//! overlay's figures are read into the view once a second while it shows.
//!
//! It shows a display the Mac makes at the window's size, Retina, by default
//! ([`Open::Made`]), resized with the window once it settles; the display picker offers the
//! Mac's physical displays beside it. A Mac that cannot make one streams a physical display,
//! and the view says so.
//!
//! Only the device holding the controls ([`Seat`]) sends input: the seat badge says who holds
//! them, with a "Take control" button. While the window is key and holds them, the system's
//! shortcuts (⌘Tab, ⌘Space, Mission Control) go to the Mac too ([`shortcuts`]). A Mac whose
//! Screen Recording or Accessibility is off refuses the session; the view names the pane and
//! the Mac, with a "Turn on" button that opens the guide on that Mac's screen: here through
//! `onicode_grants::guide::show`, on another Mac through its engine, and desktopd is told
//! through the engine either way (`DesktopOpenGuide`) so it restarts once the grant is on. A
//! locked Mac, a refused open, refused input and a refused curtain are said in the window.
//!
//! While the window is key, the clipboard follows the Mac both ways ([`clipboard`]).

mod clipboard;
mod glass;
mod hud;
mod input;
mod keys;
mod shortcuts;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use gpui::composition::{NativeHost, NativeHostOptions};
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, ContentMask, Context, CursorImage, CursorImageId, CursorStyle,
    DevicePixels, DispatchPhase, Div, FocusHandle, Hsla, InteractiveGestures as _, KeyDownEvent,
    KeyUpEvent, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PinchEvent,
    Pixels, Role, RotateEvent, ScrollDelta, ScrollEvent, SharedString, SmartMagnifyEvent, Stateful,
    Subscription, SwipeEvent, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas,
    div, point, px, rgb, size,
};
use onicode_desktop_client::{
    CursorState, DesktopClient, DesktopSession, Join, Open, Phase, Presentable, ScreenHandle,
    SessionState,
};
use onicode_desktop_link::{HostGrants, Pane, Seat};
use onicode_desktop_proto::screen::{
    Chroma, CursorShape, CurtainState, DisplayInfo, DisplayKey, DisplayShape, NoVirtualDisplay,
    OpenAsk, Quality, ScreenFailure, ScreenInput, SourceState, VirtualDisplay,
};
use onicode_desktop_proto::{DisplayId, StreamId};
use onicode_protocol::EngineId;
use onicode_protocol::desktop::{DesktopGrants, DesktopOpenGuide, OpenDesktop};
use onicode_protocol::engine::MachineKind;
use tokio::sync::watch;

use self::glass::{Glass, Layer, Reported, Seam, Shape};
use self::input::{Input, MOMENTUM_GAP, Placement, Scroll, proto_button};
use self::keys::KeyPlatform;
use self::shortcuts::{Refused, Shortcuts, Taken};
use crate::engine_client::Engine;
use crate::settings::widgets::{primary_button, secondary_button};
use crate::theme::{content, floating_fill};

/// The stage under the picture: the bars a letterboxed picture leaves stay dark whatever the
/// theme, as a video player's do.
const STAGE: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.035,
    a: 1.0,
};

// Tailwind amber-400 and red-400, the app's warning and danger tones.
const AMBER: u32 = 0xfbbf24;
const RED: u32 = 0xf87171;

/// While this Mac's grants guide is open, its grants are read this often for it.
const GUIDE_POLL: Duration = Duration::from_secs(1);

/// The overlay's figures refresh this often while it is shown.
const HUD_EVERY: Duration = Duration::from_secs(1);

/// A window resized holds still this long before the display made at its size follows: a drag
/// of its edge asks the Mac once, at the size it ends on.
const RESIZE_SETTLE: Duration = Duration::from_millis(250);

/// The runtime the desktop links run on: their datagram routing, reassembly, reports and
/// redials, apart from the engine link's one worker.
static RUNTIME: LazyLock<Option<tokio::runtime::Runtime>> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("onicode-desktop")
        .enable_all()
        .build()
        .ok()
});

/// The app's one desktop viewer endpoint on the tailnet, bound on the first desktop opened.
static CLIENT: tokio::sync::OnceCell<Arc<DesktopClient>> = tokio::sync::OnceCell::const_new();

/// The Mac a view shows.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Target {
    /// What the person calls it.
    name: String,
    /// It is this Mac.
    local: bool,
}

/// This Mac as the Mac it views knows it: its id on the tailnet and the name the seat shows.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Viewer {
    id: String,
    name: String,
}

/// Opens a window on the desktop of the Mac whose engine is `engine`, named `name`.
pub(crate) fn open_window(engine: EngineId, name: String, cx: &mut App) {
    let own = crate::engine_client::local(cx);
    let this = own.machines_now().and_then(|machines| {
        machines
            .machines
            .into_iter()
            .find(|machine| matches!(machine.kind, MachineKind::This))
    });
    let local = this.as_ref().is_some_and(|this| this.engine == engine);
    // This device's key for the display a Mac makes for it, kept in its own data directory.
    let key = own.data_dir().and_then(|dir| {
        onicode_desktop_client::display::key(&dir)
            .inspect_err(|error| log::warn!("no display key: {error}"))
            .ok()
    });
    let viewer = Viewer {
        id: this.as_ref().map_or_else(
            || kept_device_id(&own).unwrap_or_else(|| "onicode-viewer".to_owned()),
            |this| this.engine.0.clone(),
        ),
        name: this.map_or_else(|| "Mac".to_owned(), |this| this.name),
    };
    let reach = if local { own } else { own.on(engine) };
    let target = Target { name, local };
    let title = SharedString::from(format!("{} — Desktop", target.name));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(1280.0), px(800.0)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..TitlebarOptions::default()
        }),
        window_min_size: Some(size(px(480.0), px(320.0))),
        ..WindowOptions::default()
    };
    let opened = cx.open_window(options, move |window, cx| {
        cx.new(|cx| DesktopView::new(target, reach, viewer, key, window, cx))
    });
    if let Err(error) = opened {
        log::warn!("no window for the desktop: {error}");
    }
}

/// The engine's id as it keeps it (`{data_dir}/device-id`), for a viewer whose engine has not
/// listed its machines yet.
fn kept_device_id(engine: &Engine) -> Option<String> {
    let dir = engine.data_dir()?;
    let id = std::fs::read_to_string(dir.join("device-id")).ok()?;
    let id = id.trim();
    (!id.is_empty()).then(|| id.to_owned())
}

/// What the view asks of the session. A seam: the app's is `DesktopSession`, tests record.
trait Link {
    /// The session's state as it changes; `None` for a link that has none (tests).
    fn state(&self) -> Option<watch::Receiver<Arc<SessionState>>>;
    /// Stream `open`: a physical display, or one made at this window's size.
    fn open(&self, open: Open, quality: Quality);
    /// The display made for `stream` is now this size, in pixels, at `scale`.
    fn resize(&self, stream: StreamId, shape: DisplayShape);
    /// Stop streaming `stream`.
    fn close(&self, stream: StreamId);
    /// Input for `stream`.
    fn input(&self, stream: StreamId, input: ScreenInput);
    /// Take the controls, or give them up.
    fn take_control(&self, take: bool);
    /// Hear the Mac's sound, or stop.
    fn set_sound(&self, on: bool);
    /// Draw the curtain over the Mac's own screens, or let it go.
    fn curtain(&self, on: bool);
    /// `stream`'s pictures and cursor.
    fn screen(&self, stream: StreamId) -> Option<ScreenHandle>;
    /// The window is key, or stopped being: the clipboard follows the Mac while it is.
    fn clipboard_focus(&self, _key: bool) {}
    /// End the session.
    fn shutdown(&self);
}

impl Link for DesktopSession {
    fn state(&self) -> Option<watch::Receiver<Arc<SessionState>>> {
        Some(DesktopSession::state(self))
    }

    fn open(&self, open: Open, quality: Quality) {
        DesktopSession::open(self, open, quality);
    }

    fn resize(&self, stream: StreamId, shape: DisplayShape) {
        DesktopSession::resize(self, stream, shape.width, shape.height, Some(shape.scale));
    }

    fn close(&self, stream: StreamId) {
        DesktopSession::close(self, stream);
    }

    fn input(&self, stream: StreamId, input: ScreenInput) {
        DesktopSession::input(self, stream, input);
    }

    fn take_control(&self, take: bool) {
        DesktopSession::take_control(self, take);
    }

    fn set_sound(&self, on: bool) {
        DesktopSession::set_sound(self, on);
    }

    fn curtain(&self, on: bool) {
        DesktopSession::curtain(self, on);
    }

    fn screen(&self, stream: StreamId) -> Option<ScreenHandle> {
        DesktopSession::screen(self, stream)
    }

    fn clipboard_focus(&self, key: bool) {
        DesktopSession::clipboard_focus(self, key);
    }

    fn shutdown(&self) {
        DesktopSession::shutdown(self);
    }
}

/// Where the session is, as the view shows it.
#[derive(Clone, PartialEq, Debug, Default)]
enum Stage {
    /// Dialling desktopd.
    #[default]
    Connecting,
    /// Welcomed.
    Joined,
    /// The Mac refuses: these panes are off there.
    NotPermitted(Vec<Pane>),
    /// The link dropped; the next dial is this far off.
    Reconnecting(Duration),
    /// The session ended.
    Closed(String),
}

/// The stream the view shows.
#[derive(Clone, Copy, PartialEq, Debug)]
struct StreamSnap {
    /// Its id.
    stream: StreamId,
    /// Its size in pixels: the coordinates input goes in.
    size: (u32, u32),
    /// Bits a second the Mac sends.
    rate_bps: u64,
    /// Whether its display draws: a locked Mac's, or one at the login window, does not.
    source: SourceState,
    /// For a display asked made: the one made, or the physical one shown instead and why.
    made: Option<VirtualDisplay>,
}

/// What the view reads of the session's state, read in one place.
#[derive(Clone, PartialEq, Debug, Default)]
struct Snapshot {
    stage: Stage,
    /// What the Mac calls itself.
    host: String,
    displays: Vec<DisplayInfo>,
    seat: Seat,
    /// What the Mac may do, as desktopd last read it.
    grants: HostGrants,
    /// The first stream: the view shows one display.
    stream: Option<StreamSnap>,
    /// The curtain is drawn over the Mac's own screens.
    curtain: bool,
    /// The Mac would not draw the curtain, in its words.
    curtain_refused: Option<String>,
    /// The newest open the Mac refused, and why.
    failure: Option<(OpenAsk, ScreenFailure)>,
    /// The Mac refused this device's input: it does not hold the controls.
    input_refused: bool,
    /// Whether the Mac may read its clipboard unasked, as it said while following this window.
    paste: Option<onicode_clipboard::PasteAccess>,
    /// The newest copy that stays on its Mac past the clipboard's caps.
    clipboard_kept: Option<onicode_desktop_client::KeptCopy>,
}

impl Snapshot {
    fn of(state: &SessionState) -> Self {
        let stage = match &state.phase {
            Phase::Connecting => Stage::Connecting,
            Phase::Joined => Stage::Joined,
            Phase::NotPermitted { missing } => Stage::NotPermitted(missing.clone()),
            Phase::Reconnecting { retry_in } => Stage::Reconnecting(*retry_in),
            Phase::Closed { reason } => Stage::Closed(reason.clone()),
        };
        Self {
            stage,
            host: state.host.clone(),
            displays: state.displays.clone(),
            seat: state.seat.clone(),
            grants: state.grants,
            stream: state.streams.first().map(|view| StreamSnap {
                stream: view.stream,
                size: (view.width, view.height),
                rate_bps: u64::from(view.rate_bps),
                source: view.source,
                made: view.made,
            }),
            curtain: matches!(state.curtain, Some(CurtainState::Up { .. })),
            curtain_refused: match &state.curtain {
                Some(CurtainState::Refused { why }) => Some(why.clone()),
                _ => None,
            },
            failure: state.failure.clone(),
            input_refused: state.input_refused,
            paste: state.paste,
            clipboard_kept: state.clipboard_kept,
        }
    }
}

/// How far opening has got.
#[derive(Clone, PartialEq, Debug)]
enum Opening {
    /// Asking the Mac's engine, binding, dialling.
    Asking,
    /// It failed, in the person's words.
    Failed(SharedString),
    /// The session is up (or keeps trying on its own).
    Linked,
}

/// What the view shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Choice {
    /// A display the Mac makes at this window's size.
    Made,
    /// One of the Mac's physical displays.
    Display(DisplayId),
}

/// Makes the layer a stream's pictures go up on, in a native host.
type MakeLayer = Rc<dyn Fn(&NativeHost, Reported) -> gpui::Result<Box<dyn Layer>>>;

/// The app's layers: a `VideoLayer` in the host.
fn video_layer(host: &NativeHost, reported: Reported) -> gpui::Result<Box<dyn Layer>> {
    use gpui_apple::fast::video_layer::{VideoLayer, VideoLayerOptions};
    let layer = VideoLayer::attach(host, VideoLayerOptions::default())?;
    layer.on_presented(Some(reported));
    Ok(Box::new(layer))
}

/// A native host in the window `window` the view draws in.
struct Host {
    window: AnyWindowHandle,
    native: NativeHost,
    /// A layer was made for it (or refused).
    tried: bool,
}

/// The Mac's cursor as this Mac's system cursor wears it.
struct Pointer {
    id: CursorImageId,
    /// The picture's serial last set; `None` before the first.
    serial: Option<u64>,
    /// The Mac sent a picture.
    has_image: bool,
    /// The Mac's cursor is on the streamed display.
    visible: bool,
}

/// The stream on screen.
struct Screen {
    stream: StreamId,
    size: (u32, u32),
    handle: Option<ScreenHandle>,
    glass: Arc<Glass>,
    /// The newest picture's shape; `None` before the first.
    shape: Option<Shape>,
    host: Option<Host>,
    lower: Option<Host>,
    /// Where the stripes meet in the layout the last paint placed the layers for.
    laid_out: Rc<Cell<Option<Option<Seam>>>>,
    pointer: Pointer,
    _shapes: Task<()>,
    _cursor: Option<Task<()>>,
}

impl Drop for Screen {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.set_present(None);
        }
        self.glass.detach();
    }
}

/// The overlay's line as last read: its figures, and the captures whose stripes tore.
#[derive(Clone, PartialEq, Debug, Default)]
struct HudLine {
    figures: Vec<hud::Figure>,
    torn: u64,
}

/// What a view is made of, before it opens anything.
struct Bare {
    target: Target,
    engine: Option<Engine>,
    viewer: Viewer,
    key: Option<DisplayKey>,
    keyboard: Rc<dyn KeyPlatform>,
    shortcuts: Rc<dyn Shortcuts>,
    make_layer: MakeLayer,
}

/// A Mac's desktop.
struct DesktopView {
    target: Target,
    /// The Mac's engine, reached through the app's own; `None` under test.
    engine: Option<Engine>,
    viewer: Viewer,
    link: Option<Rc<dyn Link>>,
    opening: Opening,
    snap: Snapshot,
    /// This device's key for the display a Mac makes for it; `None` when it has none, and the
    /// view shows physical displays only.
    key: Option<DisplayKey>,
    /// What was asked of the session: it keeps it open across drops and opens it again on
    /// each welcome, so it is asked once per choice.
    asked: Option<Choice>,
    /// What the person picked: the display made at the window's size until they pick.
    chosen: Choice,
    /// The display this window wants made: its content in pixels at its backing scale.
    shape: Option<DisplayShape>,
    /// The shape the made display was opened at: the session opens it at this again on each
    /// welcome.
    opened_shape: Option<DisplayShape>,
    /// The shape the made display was last asked for.
    sent_shape: Option<DisplayShape>,
    /// A Mac that could not make a display at the window's size, in the person's words.
    fell_back: Option<SharedString>,
    screen: Option<Screen>,
    input: Input,
    keyboard: Rc<dyn KeyPlatform>,
    make_layer: MakeLayer,
    focus: FocusHandle,
    /// The stage's bounds in the window, as last laid out. Read only by input, never drawn.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The wire's input clock starts here.
    epoch: Instant,
    /// The view has the keyboard.
    focused: bool,
    /// The window is key.
    active: bool,
    /// What takes the system's shortcuts for the Mac.
    shortcuts: Rc<dyn Shortcuts>,
    /// The system's shortcuts taken while the window is key and holds the controls.
    grab: Option<Taken>,
    /// Why they stayed this Mac's, the last time the view tried.
    grab_refused: Option<Refused>,
    /// The overlay's line while it shows.
    hud: Option<HudLine>,
    sound: bool,
    /// The pane whose guide the person opened, until it is on.
    guide: Option<Pane>,
    guide_error: Option<SharedString>,
    momentum_end: Option<Task<()>>,
    hud_tick: Option<Task<()>>,
    state_task: Option<Task<()>>,
    open_task: Option<Task<()>>,
    guide_task: Option<Task<()>>,
    told_task: Option<Task<()>>,
    resize_task: Option<Task<()>>,
    taken_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl DesktopView {
    /// A view of `target`'s desktop, whose engine is `engine`, that opens it at once as
    /// `viewer`, asking for a display made under `key`.
    fn new(
        target: Target,
        engine: Engine,
        viewer: Viewer,
        key: Option<DisplayKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::bare(
            Bare {
                target,
                engine: Some(engine),
                viewer,
                key,
                keyboard: Rc::new(keys::System),
                shortcuts: Rc::new(shortcuts::System),
                make_layer: Rc::new(video_layer),
            },
            window,
            cx,
        );
        view.connect(cx);
        view
    }

    /// A view on `link`, reading `keyboard`, taking shortcuts through `shortcuts` and putting
    /// pictures up through `make_layer` (tests).
    #[cfg(test)]
    fn linked_for_test(
        target: Target,
        link: Rc<dyn Link>,
        keyboard: Rc<dyn KeyPlatform>,
        shortcuts: Rc<dyn Shortcuts>,
        make_layer: MakeLayer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let viewer = Viewer {
            id: "here-id".into(),
            name: "here".into(),
        };
        let bare = Bare {
            target,
            engine: None,
            viewer,
            key: Some(DisplayKey([7; 16])),
            keyboard,
            shortcuts,
            make_layer,
        };
        let mut view = Self::bare(bare, window, cx);
        view.linked(Ok(link), cx);
        view
    }

    fn bare(bare: Bare, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let Bare {
            target,
            engine,
            viewer,
            key,
            keyboard,
            shortcuts,
            make_layer,
        } = bare;
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus_in(&focus, window, |view, _window, cx| {
                view.focus_changed(true, cx);
            }),
            cx.on_blur(&focus, window, |view, _window, cx| {
                view.focus_changed(false, cx);
            }),
            cx.observe_window_activation(window, |view, window, cx| {
                view.active = window.is_window_active();
                if !view.active {
                    view.let_go(cx);
                }
                view.update_grab(cx);
                view.follow_clipboard();
            }),
            cx.observe_window_bounds(window, |view, window, cx| {
                if let Some(shape) = window_shape(window, cx) {
                    view.reshape(shape, cx);
                }
            }),
        ];
        window.focus(&focus, cx);
        let shape = window_shape(window, cx);
        Self {
            target,
            engine,
            viewer,
            link: None,
            opening: Opening::Asking,
            snap: Snapshot::default(),
            key,
            asked: None,
            chosen: Choice::Made,
            shape,
            opened_shape: None,
            sent_shape: None,
            fell_back: None,
            screen: None,
            input: Input::default(),
            keyboard,
            make_layer,
            focus,
            bounds: Rc::new(Cell::new(Bounds::default())),
            epoch: Instant::now(),
            focused: false,
            active: window.is_window_active(),
            shortcuts,
            grab: None,
            grab_refused: None,
            hud: None,
            sound: true,
            guide: None,
            guide_error: None,
            momentum_end: None,
            hud_tick: None,
            state_task: None,
            open_task: None,
            guide_task: None,
            told_task: None,
            resize_task: None,
            taken_task: None,
            _subscriptions: subscriptions,
        }
    }

    // Opening.

    /// Ask the Mac's engine for its desktop and dial it.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let Some(runtime) = RUNTIME.as_ref() else {
            self.opening = Opening::Failed("The desktop's thread could not start.".into());
            cx.notify();
            return;
        };
        let runtime = runtime.handle().clone();
        // This Mac's own desktop is for looking at: taking its controls would chase its own
        // pointer.
        let join = Join {
            name: self.viewer.name.clone(),
            control: !self.target.local,
            sound: self.sound && !self.target.local,
        };
        let me = self.viewer.id.clone();
        let local = self.target.local;
        let ask = engine.call::<OpenDesktop>(None, OpenDesktop {});
        self.opening = Opening::Asking;
        cx.notify();
        self.open_task = Some(cx.spawn(async move |this, cx| {
            let opened = async move {
                let endpoint = ask
                    .await
                    .map_err(|error| format!("The Mac did not open its desktop: {error}"))?;
                runtime
                    .spawn(async move {
                        let client = CLIENT
                            .get_or_try_init(|| DesktopClient::bind(&me))
                            .await
                            .map_err(|error| format!("This Mac is not on the tailnet: {error}"))?;
                        let session = client.connect(endpoint, join);
                        let here = tokio::runtime::Handle::current();
                        clipboard::attach(&session, &here, &me, local);
                        Ok::<_, String>(session)
                    })
                    .await
                    .map_err(|error| error.to_string())?
            };
            let result = opened.await.map(|session| Rc::new(session) as Rc<dyn Link>);
            let _gone = this.update(cx, |view, cx| view.linked(result, cx));
        }));
    }

    /// The session is up, or opening failed.
    fn linked(&mut self, result: Result<Rc<dyn Link>, String>, cx: &mut Context<Self>) {
        self.open_task = None;
        match result {
            Ok(link) => {
                self.opening = Opening::Linked;
                if let Some(mut state) = link.state() {
                    self.state_task = Some(cx.spawn(async move |this, cx| {
                        loop {
                            let snap = {
                                let state = state.borrow_and_update();
                                Snapshot::of(&state)
                            };
                            if this.update(cx, |view, cx| view.observe(snap, cx)).is_err() {
                                break;
                            }
                            if state.changed().await.is_err() {
                                break;
                            }
                        }
                    }));
                }
                self.link = Some(link);
                // A new session starts unfocused: tell it where the window stands.
                self.follow_clipboard();
            }
            Err(error) => self.opening = Opening::Failed(error.into()),
        }
        cx.notify();
    }

    /// Open again after a failure or a closed session.
    fn retry(&mut self, cx: &mut Context<Self>) {
        self.shutdown();
        self.snap = Snapshot::default();
        self.connect(cx);
    }

    fn shutdown(&mut self) {
        self.screen = None;
        self.state_task = None;
        self.asked = None;
        self.opened_shape = None;
        self.sent_shape = None;
        self.resize_task = None;
        self.grab = None;
        self.taken_task = None;
        if let Some(link) = self.link.take() {
            link.shutdown();
        }
    }

    /// The session's state changed.
    ///
    /// What was asked stays asked across drops and refusals: the session opens it again on each
    /// welcome, so asking again would open it twice.
    fn observe(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        match &snap.stage {
            Stage::NotPermitted(missing) => {
                if self.guide.is_some_and(|pane| !missing.contains(&pane)) {
                    self.guide = None;
                }
            }
            _ => {
                self.guide = None;
                self.guide_error = None;
            }
        }
        if self.target.local
            && onicode_grants::guide::is_open()
            && let Some(pane) = self.guide
        {
            onicode_grants::guide::report(!snap.grants.missing().contains(&pane));
        }
        // A display the Mac could not make at the window's size: its main display instead.
        if self.asked == Some(Choice::Made)
            && snap.failure != self.snap.failure
            && let Some((OpenAsk::Made(_), why)) = &snap.failure
            && let Some(main) = snap.displays.first()
        {
            let host = if snap.host.is_empty() {
                &self.target.name
            } else {
                &snap.host
            };
            self.fell_back = Some(SharedString::from(format!(
                "{host} could not make a display this size ({}), so this shows its main display.",
                failure_reason(why)
            )));
            self.chosen = Choice::Display(main.id);
            self.asked = None;
        }
        if snap.stage == Stage::Joined && snap.stream.is_none() && self.asked.is_none() {
            self.open_display(&snap.displays);
        }
        match (
            snap.stream,
            self.screen.as_ref().map(|screen| screen.stream),
        ) {
            (Some(stream), Some(showing)) if stream.stream == showing => {
                if let Some(screen) = self.screen.as_mut() {
                    screen.size = stream.size;
                }
            }
            (Some(stream), _) => self.attach_screen(stream, cx),
            (None, Some(_)) => self.screen = None,
            (None, None) => {}
        }
        let lost_controls = self.snap.seat.you && !snap.seat.you;
        let took_controls = !self.snap.seat.you && snap.seat.you;
        self.snap = snap;
        if lost_controls {
            // Nothing held goes any more; what was held is the new controller's to let go.
            let time_us = self.time_us();
            self.input.let_go(time_us);
            self.input.take();
        }
        if took_controls {
            self.gestures();
        }
        self.resize_made();
        self.update_grab(cx);
        cx.notify();
    }

    /// Stream what the person picked: a display made at the window's size until they pick a
    /// physical one, or the first physical one when this device has no key for one.
    fn open_display(&mut self, displays: &[DisplayInfo]) {
        let Some(link) = &self.link else {
            return;
        };
        if self.chosen == Choice::Made
            && let (Some(key), Some(shape)) = (self.key, self.shape)
        {
            link.open(Open::Made { key, shape }, made_quality(shape));
            self.asked = Some(Choice::Made);
            self.opened_shape = Some(shape);
            self.sent_shape = Some(shape);
            return;
        }
        let picked = match self.chosen {
            Choice::Display(id) => displays.iter().find(|d| d.id == id),
            Choice::Made => None,
        };
        let Some(display) = picked.or_else(|| displays.first()) else {
            return;
        };
        link.open(Open::Display(display.id), quality_for(display));
        self.asked = Some(Choice::Display(display.id));
    }

    /// Show `choice` instead.
    fn choose_display(&mut self, choice: Choice, cx: &mut Context<Self>) {
        if self.asked == Some(choice) {
            return;
        }
        self.chosen = choice;
        self.fell_back = None;
        if let (Some(link), Some(screen)) = (&self.link, &self.screen) {
            link.close(screen.stream);
        }
        self.screen = None;
        self.asked = None;
        let displays = self.snap.displays.clone();
        self.open_display(&displays);
        cx.notify();
    }

    /// Ask again for a display the Mac refused to open.
    fn reopen(&mut self, cx: &mut Context<Self>) {
        self.asked = None;
        let displays = self.snap.displays.clone();
        self.open_display(&displays);
        cx.notify();
    }

    /// The window's size or screen changed: the display made at its size follows once it holds
    /// still for [`RESIZE_SETTLE`] on the executor's clock.
    fn reshape(&mut self, shape: DisplayShape, cx: &mut Context<Self>) {
        if self.shape == Some(shape) {
            return;
        }
        self.shape = Some(shape);
        self.resize_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_SETTLE).await;
            let _gone = this.update(cx, |view, _cx| {
                view.resize_task = None;
                view.resize_made();
            });
        }));
    }

    /// Ask the Mac to make the display shown the window's size, when it is one made for this
    /// device and the size has moved on from what it was asked for.
    fn resize_made(&mut self) {
        if self.resize_task.is_some() || self.asked != Some(Choice::Made) {
            return;
        }
        let (Some(link), Some(screen), Some(shape)) = (&self.link, &self.screen, self.shape) else {
            return;
        };
        let made = self.snap.stream.and_then(|stream| stream.made);
        if !matches!(made, Some(VirtualDisplay::Made(_))) || self.sent_shape == Some(shape) {
            return;
        }
        link.resize(screen.stream, shape);
        self.sent_shape = Some(shape);
    }

    /// Show `stream`: its pictures to a glass of its own, its cursor to the system cursor.
    fn attach_screen(&mut self, stream: StreamSnap, cx: &mut Context<Self>) {
        self.screen = None;
        // A made display opens at the shape it was opened at, on a redial too.
        self.sent_shape = self.opened_shape;
        let glass = Glass::new();
        let handle = self
            .link
            .as_ref()
            .and_then(|link| link.screen(stream.stream));
        glass.report_to(handle.clone());
        if let Some(handle) = &handle {
            let to = Arc::clone(&glass);
            handle.set_present(Some(Arc::new(move |frame: &Presentable| {
                to.offer_frame(frame);
            })));
        }
        let mut shapes = glass.shapes();
        let id = stream.stream;
        let shapes_task = cx.spawn(async move |this, cx| {
            while shapes.changed().await.is_ok() {
                let shape = *shapes.borrow_and_update();
                if this
                    .update(cx, |view, cx| view.shaped(id, shape, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let cursor_task = handle.as_ref().map(|handle| {
            let mut cursor = handle.cursor();
            cx.spawn(async move |this, cx| {
                loop {
                    let state = cursor.borrow_and_update().clone();
                    if this
                        .update(cx, |view, cx| view.cursor_moved(id, &state, cx))
                        .is_err()
                    {
                        break;
                    }
                    if cursor.changed().await.is_err() {
                        break;
                    }
                }
            })
        });
        self.screen = Some(Screen {
            stream: stream.stream,
            size: stream.size,
            handle,
            glass,
            shape: None,
            host: None,
            lower: None,
            laid_out: Rc::new(Cell::new(None)),
            pointer: Pointer {
                id: next_pointer_id(),
                serial: None,
                has_image: false,
                visible: true,
            },
            _shapes: shapes_task,
            _cursor: cursor_task,
        });
        self.gestures();
    }

    /// Trackpad gestures go to the Mac's apps, so a scroll comes with the gesture a trackpad's
    /// does there.
    fn gestures(&mut self) {
        if let (Some(link), Some(screen)) = (&self.link, &self.screen)
            && self.snap.seat.you
        {
            link.input(screen.stream, ScreenInput::Gestures { remote: true });
        }
    }

    /// The pictures' shape changed: the layers are placed for it in the next frame.
    fn shaped(&mut self, stream: StreamId, shape: Option<Shape>, cx: &mut Context<Self>) {
        if let Some(screen) = self.screen.as_mut().filter(|s| s.stream == stream) {
            screen.shape = shape;
            cx.notify();
        }
    }

    /// The Mac's cursor changed: its picture becomes this Mac's system cursor over the picture.
    fn cursor_moved(&mut self, stream: StreamId, state: &CursorState, cx: &mut Context<Self>) {
        let Some(screen) = self.screen.as_mut().filter(|s| s.stream == stream) else {
            return;
        };
        let serial = state.shape_serial;
        let mut changed = screen.pointer.visible != state.visible;
        screen.pointer.visible = state.visible;
        if screen.pointer.serial != Some(serial) {
            screen.pointer.serial = Some(serial);
            let image = state.shape.as_deref().and_then(system_picture);
            changed |= screen.pointer.has_image != image.is_some();
            screen.pointer.has_image = image.is_some();
            cx.set_cursor_image(screen.pointer.id, image);
        }
        if changed {
            cx.notify();
        }
    }

    // The controls.

    fn take_control(&mut self, take: bool, cx: &mut Context<Self>) {
        if let Some(link) = &self.link {
            link.take_control(take);
        }
        cx.notify();
    }

    fn toggle_sound(&mut self, cx: &mut Context<Self>) {
        self.sound = !self.sound;
        if let Some(link) = &self.link {
            link.set_sound(self.sound);
        }
        cx.notify();
    }

    fn toggle_curtain(&mut self, cx: &mut Context<Self>) {
        if let Some(link) = &self.link {
            link.curtain(!self.snap.curtain);
        }
        cx.notify();
    }

    /// Shows or hides the overlay. While it shows, its figures are read into the view once a
    /// second, and the view is told only when they changed.
    fn toggle_hud(&mut self, cx: &mut Context<Self>) {
        if self.hud.take().is_some() {
            self.hud_tick = None;
            cx.notify();
            return;
        }
        self.hud = Some(self.read_hud());
        self.hud_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(HUD_EVERY).await;
                let read = this.update(cx, |view, cx| {
                    let line = view.read_hud();
                    if view.hud.as_ref() != Some(&line) {
                        view.hud = Some(line);
                        cx.notify();
                    }
                });
                if read.is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// The overlay's figures now; none before a stream.
    fn read_hud(&self) -> HudLine {
        let Some(screen) = self.screen.as_ref() else {
            return HudLine::default();
        };
        let stats = screen.handle.as_ref().map(ScreenHandle::stats);
        let fps = match self.asked {
            Some(Choice::Made) => self
                .opened_shape
                .map_or(60, |shape| made_quality(shape).fps),
            Some(Choice::Display(id)) => self
                .snap
                .displays
                .iter()
                .find(|d| d.id == id)
                .map_or(60, |d| quality_for(d).fps),
            None => 60,
        };
        let input = hud::HudInput {
            target_fps: fps,
            paint: screen.glass.rate(),
            pacing: screen.glass.pacing(),
            capture: screen.glass.glass().capture,
            replaced: screen.glass.replaced(),
            mbps: hud::mbps(self.snap.stream.map_or(0, |s| s.rate_bps)),
            chroma: screen.shape.map(|shape| shape.chroma),
            loss: stats.as_ref().map(hud::Loss::of).unwrap_or_default(),
            // The fastest round trip of the clock probes.
            rtt: stats.as_ref().and_then(|s| s.clock).map(|clock| clock.rtt),
        };
        HudLine {
            figures: hud::summary(&input),
            torn: screen.glass.seams().split,
        }
    }

    /// The person asked to turn `pane` on: the guide opens on the Mac's own screen, for whoever
    /// sits at it. This Mac's opens here; another's through its engine.
    fn turn_on(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.guide = Some(pane);
        self.guide_error = None;
        if self.target.local
            && let Some(helper) = local_helper_bundle()
        {
            match onicode_grants::guide::show(grants_privilege(pane), &helper) {
                Ok(()) => {
                    self.follow_guide(pane, cx);
                    self.tell_guide(pane, cx);
                }
                Err(error) => {
                    log::warn!("the grants guide did not open: {error:?}");
                    self.guide_error = Some("The guide did not open.".into());
                }
            }
            cx.notify();
            return;
        }
        // Another Mac, or this one in a development build with no helper bundle: the Mac's
        // engine opens System Settings beside desktopd.
        let Some(engine) = self.engine.clone() else {
            cx.notify();
            return;
        };
        let opened = engine.call::<DesktopOpenGuide>(None, DesktopOpenGuide { pane });
        self.guide_task = Some(cx.spawn(async move |this, cx| {
            let result = opened.await;
            let _gone = this.update(cx, |view, cx| {
                if let Err(error) = result {
                    log::warn!("the Mac did not open the grants guide: {error}");
                    view.guide_error = Some("The Mac did not open the guide.".into());
                }
                view.guide_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Tell desktopd, through this Mac's engine, that `pane`'s guide is open here, so it
    /// restarts once the grant is on (macOS reads a grant at launch).
    fn tell_guide(&mut self, pane: Pane, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let told = engine.call::<DesktopOpenGuide>(None, DesktopOpenGuide { pane });
        self.told_task = Some(cx.spawn(async move |this, cx| {
            if let Err(error) = told.await {
                log::warn!("desktopd was not told of the grants guide: {error}");
            }
            let _gone = this.update(cx, |view, _cx| view.told_task = None);
        }));
    }

    /// While this Mac's guide is open, read desktopd's grants through the engine and hand each
    /// reading of `pane` to the guide, which walks to "granted" when it comes on. The session's
    /// own grants go to it too ([`Self::observe`]); this covers a desktopd that is restarting.
    fn follow_guide(&mut self, pane: Pane, cx: &mut Context<Self>) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        self.guide_task = Some(cx.spawn(async move |this, cx| {
            while onicode_grants::guide::is_open() {
                if let Ok(reply) = engine.call::<DesktopGrants>(None, DesktopGrants {}).await {
                    onicode_grants::guide::report(!reply.grants.missing().contains(&pane));
                }
                cx.background_executor().timer(GUIDE_POLL).await;
            }
            let _gone = this.update(cx, |view, _cx| view.guide_task = None);
        }));
    }

    // Input.

    /// Take the system's shortcuts while the window is key, has the keyboard and holds the
    /// controls of another Mac's welcomed session; give them back otherwise.
    fn update_grab(&mut self, cx: &mut Context<Self>) {
        let wanted = self.active
            && self.focused
            && self.snap.seat.you
            && !self.target.local
            && self.snap.stage == Stage::Joined;
        if !wanted {
            self.grab = None;
            self.taken_task = None;
            self.grab_refused = None;
            return;
        }
        if self.grab.is_some() {
            return;
        }
        let (wake, mut woken) = tokio::sync::mpsc::unbounded_channel::<()>();
        let wake: Box<dyn Fn()> = Box::new(move || {
            let _closed = wake.send(());
        });
        match self.shortcuts.grab(wake) {
            Ok(taken) => {
                self.grab = Some(taken);
                if self.grab_refused.take().is_some() {
                    cx.notify();
                }
                self.taken_task = Some(cx.spawn(async move |this, cx| {
                    while woken.recv().await.is_some() {
                        if this.update(cx, |view, _cx| view.drain_taken()).is_err() {
                            break;
                        }
                    }
                }));
            }
            Err(refused) => {
                if self.grab_refused != Some(refused) {
                    self.grab_refused = Some(refused);
                    cx.notify();
                }
            }
        }
    }

    /// Send the shortcuts taken since the last time.
    fn drain_taken(&mut self) {
        let Some(grab) = &self.grab else {
            return;
        };
        for key in grab.drain() {
            self.input.taken_key(key);
        }
        self.flush();
    }

    /// Send what the input made, when this device holds the controls.
    fn flush(&mut self) {
        let inputs = self.input.take();
        let (Some(link), Some(screen)) = (&self.link, &self.screen) else {
            return;
        };
        if !self.snap.seat.you {
            return;
        }
        for input in inputs {
            link.input(screen.stream, input);
        }
    }

    /// Where the picture is now; `None` before a stream or a layout.
    fn placement(&self) -> Option<Placement> {
        let screen = self.screen.as_ref()?;
        let size = screen.shape.map_or(screen.size, |shape| shape.size);
        let bounds = self.bounds.get();
        (size.0 > 0 && size.1 > 0 && bounds.size.width > px(0.0) && bounds.size.height > px(0.0))
            .then(|| Placement::new(bounds, size))
    }

    /// Now on the wire's input clock: microseconds since the view began, the low 32 bits.
    fn time_us(&self) -> u32 {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the wire's low 32 bits by design"
        )]
        let us = Instant::now()
            .saturating_duration_since(self.epoch)
            .as_micros() as u32;
        us
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        self.focused = focused;
        if focused {
            self.input.focused(self.keyboard.caps_lock());
            self.flush();
        } else {
            self.let_go(cx);
        }
        self.update_grab(cx);
        self.follow_clipboard();
    }

    /// The clipboard follows the Mac while the window is key and the view has the keyboard.
    fn follow_clipboard(&self) {
        if let Some(link) = &self.link {
            link.clipboard_focus(self.active && self.focused);
        }
    }

    /// Release on the Mac whatever is held.
    fn let_go(&mut self, cx: &mut Context<Self>) {
        self.momentum_end = None;
        let time_us = self.time_us();
        self.input.let_go(time_us);
        self.flush();
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let native = self.keyboard.native();
        if self
            .input
            .key_pressed(native, &event.keystroke, event.is_held)
        {
            self.flush();
            cx.stop_propagation();
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let native = self.keyboard.native();
        if self.input.key_released(native, &event.keystroke) {
            self.flush();
            cx.stop_propagation();
        }
    }

    fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let native = self.keyboard.native();
        self.input
            .modifiers_moved(native, event.modifiers, event.capslock.on);
        self.flush();
        cx.stop_propagation();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        let Some(at) = self.placement().and_then(|p| p.inside(event.position)) else {
            return;
        };
        self.input.button_down(
            proto_button(event.button),
            at,
            event.click_count,
            keys::mods(event.modifiers),
        );
        self.flush();
        cx.stop_propagation();
    }

    /// Anywhere in the window: a drag goes on at the picture's edge.
    fn mouse_moved(&mut self, event: &MouseMoveEvent) {
        let Some(placed) = self.placement() else {
            return;
        };
        let at = if self.input.dragging() {
            Some(placed.edge(event.position))
        } else {
            placed.inside(event.position)
        };
        if let Some(at) = at {
            self.input.moved(at);
            self.flush();
        }
    }

    /// Anywhere in the window: a button pressed on the picture goes up wherever it is let go.
    fn mouse_up(&mut self, event: &MouseUpEvent) {
        let Some(placed) = self.placement() else {
            return;
        };
        let at = placed.edge(event.position);
        if self.input.button_up(
            proto_button(event.button),
            at,
            event.click_count,
            keys::mods(event.modifiers),
        ) {
            self.flush();
        }
    }

    fn scroll(&mut self, event: &ScrollEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(placed) = self.placement() else {
            return;
        };
        let (dx, dy, precise) = match event.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y), true),
            ScrollDelta::Lines(l) => (l.x, l.y, false),
        };
        // Beside the picture a scroll goes on at its edge, so a gesture that drifts off it
        // still ends where it began.
        let at = placed.edge(event.position);
        let mods = keys::mods(event.modifiers);
        self.input.scroll(Scroll {
            dx,
            dy,
            precise,
            touch: event.touch_phase,
            momentum: event.momentum_phase,
            at,
            mods,
            time_us: self.time_us(),
        });
        self.momentum_end = self.input.coasting().then(|| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(MOMENTUM_GAP).await;
                let _gone = this.update(cx, |view, _cx| {
                    let time_us = view.time_us();
                    if view.input.end_momentum(at, mods, time_us) {
                        view.flush();
                    }
                });
            })
        });
        self.flush();
        cx.stop_propagation();
    }

    fn pinch(&mut self, event: &PinchEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(placed) = self.placement() else {
            return;
        };
        let time_us = self.time_us();
        if self.input.pinch(
            event.delta,
            event.phase,
            placed.inside(event.position),
            placed.edge(event.position),
            time_us,
        ) {
            self.flush();
            cx.stop_propagation();
        }
    }

    fn rotate(&mut self, event: &RotateEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(placed) = self.placement() else {
            return;
        };
        let time_us = self.time_us();
        self.input.rotate(
            event.rotation,
            event.phase,
            placed.edge(event.position),
            time_us,
        );
        self.flush();
        cx.stop_propagation();
    }

    fn smart_magnify(
        &mut self,
        event: &SmartMagnifyEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(at) = self.placement().and_then(|p| p.inside(event.position)) else {
            return;
        };
        self.input.smart_magnify(at);
        self.flush();
        cx.stop_propagation();
    }

    fn swipe(&mut self, event: &SwipeEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.placement().and_then(|p| p.inside(event.position)) else {
            return;
        };
        if self.input.swipe(event.delta.x, event.delta.y, at) {
            self.flush();
            cx.stop_propagation();
        }
    }

    // The picture.

    /// Keep native hosts in the window the view draws in, and the pictures' layers on them once
    /// there is a picture: a striped picture gets a second host and layer for its lower stripe,
    /// stacked under the first.
    fn place_layers(&mut self, window: &mut Window, cx: &mut App) {
        let here = window.window_handle();
        let make = Rc::clone(&self.make_layer);
        let Some(screen) = self.screen.as_mut() else {
            return;
        };
        let Some(shape) = screen.shape else {
            return;
        };
        if screen.host.as_ref().is_none_or(|host| host.window != here) {
            screen.glass.detach();
            screen.host = new_host(here, "desktop-picture", window, cx);
            screen.lower = None;
        }
        if let Some(host) = screen.host.as_mut()
            && !host.tried
        {
            host.tried = true;
            let (generation, reported) = screen.glass.reports();
            match make(&host.native, reported) {
                Ok(layer) => screen.glass.attach(layer, generation),
                Err(error) => log::warn!("no video layer for the picture: {error}"),
            }
        }
        if shape.seam.is_none() {
            return;
        }
        if screen.lower.is_none() {
            screen.lower = new_host(here, "desktop-picture-lower", window, cx);
        }
        if let Some(lower) = screen.lower.as_mut()
            && !lower.tried
        {
            lower.tried = true;
            let (generation, reported) = screen.glass.lower_reports();
            match make(&lower.native, reported) {
                Ok(layer) => screen.glass.attach_lower(layer, generation),
                Err(error) => log::warn!("no video layer for the lower stripe: {error}"),
            }
        }
    }

    /// The stage's own element: it records where the stage is, hears the pointer anywhere in
    /// the window (a drag that leaves the picture), and places the layers where the picture
    /// goes.
    fn picture(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let bounds_cell = Rc::clone(&self.bounds);
        let view = cx.entity();
        let layers = self.screen.as_ref().and_then(|screen| {
            Some((
                screen.shape?,
                screen.host.as_ref()?.native.clone(),
                screen.lower.as_ref().map(|host| host.native.clone()),
                Arc::clone(&screen.glass),
                Rc::clone(&screen.laid_out),
            ))
        });
        canvas(
            move |bounds, _window, _cx| bounds_cell.set(bounds),
            move |bounds, (), window, _cx| {
                let moved = view.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _window, cx| {
                    if phase == DispatchPhase::Bubble {
                        moved.update(cx, |view, _cx| view.mouse_moved(event));
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
                    if phase == DispatchPhase::Bubble {
                        view.update(cx, |view, _cx| view.mouse_up(event));
                    }
                });
                let Some((shape, native, lower, glass, laid_out)) = layers else {
                    return;
                };
                let at = Placement::new(bounds, shape.size).picture();
                // Pictures of another seam wait until the layers are placed for them.
                if laid_out.get() != Some(shape.seam) {
                    laid_out.set(Some(shape.seam));
                    glass.place(shape.seam);
                }
                let corners = gpui::Corners::default();
                let Some([top, low]) = stripe_places(at, shape) else {
                    window.paint_native(&native, at, corners, None);
                    return;
                };
                window.with_content_mask(Some(ContentMask { bounds: top.shown }), |window| {
                    window.paint_native(&native, top.layer, corners, None);
                });
                if let Some(lower) = &lower {
                    window.with_content_mask(Some(ContentMask { bounds: low.shown }), |window| {
                        window.paint_native(lower, low.layer, corners, None);
                    });
                }
            },
        )
        .absolute()
        .inset_0()
    }

    /// The system cursor over the stage: the Mac's cursor picture while this device controls
    /// the Mac and the cursor is on this display, none while the Mac hides it.
    fn cursor_style(&self) -> CursorStyle {
        let Some(screen) = &self.screen else {
            return CursorStyle::Arrow;
        };
        if !self.snap.seat.you {
            CursorStyle::Arrow
        } else if !screen.pointer.visible {
            CursorStyle::None
        } else if screen.pointer.has_image {
            CursorStyle::Image(screen.pointer.id)
        } else {
            CursorStyle::Arrow
        }
    }

    // The overlays.

    /// Who holds the controls, and the way to take them.
    fn seat_badge(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let seat = &self.snap.seat;
        let others = seat.clients.saturating_sub(1);
        let who = if seat.you {
            "You have control".to_owned()
        } else {
            seat.controller.as_deref().map_or_else(
                || "Nobody has control".to_owned(),
                |name| format!("{name} has control"),
            )
        };
        let who = if others > 0 {
            format!("{who} · {others} more watching")
        } else {
            who
        };
        pill("desktop-seat")
            .debug_selector(|| "desktop-seat".to_owned())
            .child(div().child(SharedString::from(who)))
            .when(!seat.you && !self.target.local, |badge| {
                badge.child(
                    primary_button("desktop-take-control", "Take control", false)
                        .debug_selector(|| "desktop-take-control".to_owned())
                        .child("Take control")
                        .on_click(cx.listener(|view, _, _, cx| view.take_control(true, cx))),
                )
            })
            .when(seat.you, |badge| {
                badge.child(
                    secondary_button("desktop-release-control", "Release", false, false)
                        .child("Release")
                        .on_click(cx.listener(|view, _, _, cx| view.take_control(false, cx))),
                )
            })
    }

    /// The display picker and the toggles.
    fn controls(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let showing = self.asked;
        // The display made at the window's size first, then the Mac's own.
        let made = self.key.map(|_| (Choice::Made, "Fit window".to_owned()));
        let choices: Vec<(Choice, String)> = made
            .into_iter()
            .chain(self.snap.displays.iter().enumerate().map(|(n, d)| {
                (
                    Choice::Display(d.id),
                    format!("Display {}", n.saturating_add(1)),
                )
            }))
            .collect();
        let many = choices.len() > 1;
        let mut bar = pill("desktop-controls");
        if many {
            for (n, (choice, label)) in choices.into_iter().enumerate() {
                bar = bar.child(
                    toggle(("desktop-display", n), label, showing == Some(choice)).on_click(
                        cx.listener(move |view, _, _, cx| view.choose_display(choice, cx)),
                    ),
                );
            }
        }
        bar.child(
            toggle("desktop-sound", "Sound", self.sound)
                .on_click(cx.listener(|view, _, _, cx| view.toggle_sound(cx))),
        )
        .when(self.snap.seat.you, |bar| {
            bar.child(
                toggle("desktop-curtain", "Curtain", self.snap.curtain)
                    .on_click(cx.listener(|view, _, _, cx| view.toggle_curtain(cx))),
            )
        })
        .child(
            toggle("desktop-stats", "Stats", self.hud.is_some())
                .on_click(cx.listener(|view, _, _, cx| view.toggle_hud(cx))),
        )
    }

    /// The overlay's figures, when shown and a stream is up.
    fn hud_line(&self) -> Option<impl IntoElement + use<>> {
        let line = self.hud.as_ref()?;
        self.screen.as_ref()?;
        let mut pill = pill("desktop-hud")
            .debug_selector(|| "desktop-hud".to_owned())
            .font_family(crate::fonts::mono())
            .role(Role::Status);
        for figure in &line.figures {
            let color = if figure.warn {
                rgb(AMBER).into()
            } else {
                content(0.85)
            };
            pill = pill.child(
                div()
                    .text_color(color)
                    .child(SharedString::from(figure.text.clone())),
            );
        }
        if line.torn > 0 {
            pill = pill.child(
                div()
                    .text_color(rgb(AMBER))
                    .child(SharedString::from(format!("{} torn", line.torn))),
            );
        }
        Some(pill)
    }

    /// What the Mac would not do, each in a line under the controls: make a display this size,
    /// take this device's input, draw the curtain; and the shortcuts that stayed here.
    fn banners(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let host = self.host_name();
        let banner = |id: &'static str, text: String| {
            pill(id)
                .debug_selector(move || id.to_owned())
                .role(Role::Status)
                .text_color(rgb(AMBER))
                .child(div().child(SharedString::from(text)))
        };
        let mut banners = Vec::new();
        let made_instead = match self.snap.stream.and_then(|stream| stream.made) {
            Some(VirtualDisplay::Physical { why, .. }) if self.asked == Some(Choice::Made) => {
                Some(format!(
                    "{host} could not make a display this size ({}), so this shows its main display.",
                    no_display_reason(why)
                ))
            }
            _ => self.fell_back.as_ref().map(ToString::to_string),
        };
        if let Some(text) = made_instead {
            banners.push(banner("desktop-made-fallback", text).into_any_element());
        }
        if self.snap.input_refused {
            banners.push(
                banner(
                    "desktop-input-refused",
                    format!("{host} refused your input: another device has control."),
                )
                .into_any_element(),
            );
        }
        if let Some(why) = &self.snap.curtain_refused {
            banners.push(
                banner(
                    "desktop-curtain-refused",
                    format!("{host} did not draw the curtain: {why}"),
                )
                .into_any_element(),
            );
        }
        if let Some(text) = clipboard::paste_banner(self.snap.paste, &host) {
            banners.push(
                banner("desktop-paste-access", text)
                    .child(
                        secondary_button("desktop-paste-turn-on", "Open Settings", false, false)
                            .child("Open Settings")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.turn_on(Pane::PasteFromOtherApps, cx);
                            })),
                    )
                    .into_any_element(),
            );
        }
        if let Some(text) = clipboard::kept_banner(self.snap.clipboard_kept, &host) {
            banners.push(banner("desktop-clipboard-kept", text).into_any_element());
        }
        if self.grab_refused == Some(Refused::NeedsAccessibility) {
            banners.push(
                banner(
                    "desktop-shortcuts-refused",
                    "⌘Tab, ⌘Space and Mission Control stay on this Mac until Onicode has \
                     Accessibility."
                        .to_owned(),
                )
                .child(
                    secondary_button("desktop-shortcuts-turn-on", "Turn on", false, false)
                        .child("Turn on")
                        .on_click(cx.listener(|_view, _, _, _cx| {
                            onicode_grants::open_settings(onicode_grants::Privilege::Accessibility);
                        })),
                )
                .into_any_element(),
            );
        }
        banners
    }

    /// What the Mac calls itself, or what this device calls it before it says.
    fn host_name(&self) -> String {
        if self.snap.host.is_empty() {
            self.target.name.clone()
        } else {
            self.snap.host.clone()
        }
    }

    /// What stands in for the picture while there is none: opening, a refusal, a drop.
    fn notice(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let host = self.host_name();
        // A physical display the Mac would not open, and why.
        let refused = match &self.snap.failure {
            Some((OpenAsk::Target(_), why)) if self.snap.stream.is_none() => {
                Some(failure_reason(why))
            }
            _ => None,
        };
        // A display that does not draw: the Mac is locked, or at the login window.
        let dark = self
            .snap
            .stream
            .map(|stream| stream.source)
            .filter(|source| matches!(source, SourceState::Locked | SourceState::Away));
        let card = || {
            div()
                .id("desktop-notice")
                .debug_selector(|| "desktop-notice".to_owned())
                .flex()
                .flex_col()
                .gap(px(10.0))
                .max_w(px(440.0))
                .p(px(20.0))
                .rounded_xl()
                .bg(floating_fill())
                .border_1()
                .border_color(content(0.1))
                .text_color(content(1.))
                .text_size(px(13.0))
                .occlude()
        };
        let muted = |text: String| {
            div()
                .text_color(content(0.5))
                .child(SharedString::from(text))
        };
        let card = match (&self.opening, &self.snap.stage) {
            (Opening::Failed(error), _) => card()
                .role(Role::Alert)
                .child(div().child(SharedString::from(format!(
                    "{}'s desktop did not open",
                    self.target.name
                ))))
                .child(muted(error.to_string()))
                .child(retry_button(cx)),
            (Opening::Asking, _) | (Opening::Linked, Stage::Connecting) => card()
                .role(Role::Status)
                .child(div().child(SharedString::from(format!(
                    "Connecting to {}…",
                    self.target.name
                )))),
            (Opening::Linked, Stage::Reconnecting(retry_in)) => card()
                .role(Role::Status)
                .child(div().child(SharedString::from(format!("{host} dropped off"))))
                .child(muted(format!(
                    "Trying again in {} s.",
                    retry_in.as_secs().max(1)
                ))),
            (Opening::Linked, Stage::Closed(reason)) => card()
                .role(Role::Alert)
                .child(div().child(SharedString::from(format!("{host}'s desktop closed"))))
                .child(muted(reason.clone()))
                .child(retry_button(cx)),
            (Opening::Linked, Stage::NotPermitted(missing)) => {
                let mut card = card().role(Role::Alert);
                for &pane in missing {
                    let asked = self.guide == Some(pane);
                    let label = format!("Turn on {} on {host}", pane_name(pane));
                    card = card
                        .child(div().child(SharedString::from(missing_line(pane, &host))))
                        .child(muted(if asked {
                            guide_note(pane, &host, self.target.local)
                        } else {
                            pane_purpose(pane).to_owned()
                        }))
                        .child(
                            div().flex().child(
                                primary_button(
                                    SharedString::from(format!("desktop-turn-on-{pane:?}")),
                                    label,
                                    false,
                                )
                                .debug_selector(move || format!("desktop-turn-on-{pane:?}"))
                                .child("Turn on")
                                .on_click(
                                    cx.listener(move |view, _, _, cx| view.turn_on(pane, cx)),
                                ),
                            ),
                        );
                }
                if let Some(error) = &self.guide_error {
                    card = card.child(div().text_color(rgb(RED)).child(error.clone()));
                }
                card
            }
            (Opening::Linked, Stage::Joined) if refused.is_some() => card()
                .role(Role::Alert)
                .child(div().child(SharedString::from(format!(
                    "{host} did not open the display"
                ))))
                .children(refused.map(muted))
                .child(
                    div().flex().child(
                        secondary_button("desktop-reopen", "Try again", false, false)
                            .debug_selector(|| "desktop-reopen".to_owned())
                            .child("Try again")
                            .on_click(cx.listener(|view, _, _, cx| view.reopen(cx))),
                    ),
                ),
            (Opening::Linked, Stage::Joined) if dark.is_some() => card()
                .role(Role::Status)
                .child(
                    div().child(SharedString::from(if dark == Some(SourceState::Locked) {
                        format!("{host} is locked")
                    } else {
                        format!("{host} is at the login window")
                    })),
                )
                .child(muted(
                    "Its screen shows here again once someone signs in at it.".to_owned(),
                )),
            (Opening::Linked, Stage::Joined)
                if self.screen.as_ref().is_none_or(|s| s.shape.is_none()) =>
            {
                card()
                    .role(Role::Status)
                    .child(div().child(SharedString::from(format!("Waiting for {host}'s screen…"))))
            }
            (Opening::Linked, Stage::Joined) => return None,
        };
        Some(card.into_any_element())
    }
}

impl Drop for DesktopView {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl Render for DesktopView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.place_layers(window, cx);
        let notice = self.notice(cx);
        let live = self.link.is_some() && self.snap.stage == Stage::Joined;
        let top = live.then(|| {
            div()
                .absolute()
                .top(px(10.0))
                .left(px(10.0))
                .right(px(10.0))
                .flex()
                .flex_row()
                .items_start()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(self.controls(cx))
                        .children(self.hud_line())
                        .children(self.banners(cx)),
                )
                .child(self.seat_badge(cx))
        });
        div()
            .id("desktop-stage")
            .debug_selector(|| "desktop-stage".to_owned())
            .key_context("DesktopView")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(STAGE)
            .font_family(crate::fonts::UI)
            .cursor(self.cursor_style())
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_any_mouse_down(cx.listener(Self::mouse_down))
            .on_scroll(cx.listener(Self::scroll))
            .on_pinch(cx.listener(Self::pinch))
            .on_rotate(cx.listener(Self::rotate))
            .on_smart_magnify(cx.listener(Self::smart_magnify))
            .on_swipe(cx.listener(Self::swipe))
            .child(self.picture(cx))
            .when_some(notice, |stage, notice| {
                stage.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(notice),
                )
            })
            .children(top)
    }
}

/// An overlay pill: the app's floating surface over the dark stage, holding the pointer off
/// the picture.
fn pill(id: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .flex_wrap()
        .items_center()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(4.0))
        .rounded(px(10.0))
        .bg(floating_fill())
        .border_1()
        .border_color(content(0.1))
        .text_color(content(0.85))
        .text_size(px(12.0))
        .occlude()
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
}

/// A toggle on the controls pill: the app's secondary button, lifted while `on`.
fn toggle(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    on: bool,
) -> Stateful<Div> {
    let label = label.into();
    secondary_button(id, label.clone(), false, false)
        .when(on, |button| button.bg(content(0.1)).text_color(content(1.)))
        .child(label)
}

fn retry_button(cx: &mut Context<DesktopView>) -> impl IntoElement + use<> {
    div().flex().child(
        secondary_button("desktop-retry", "Try again", false, false)
            .child("Try again")
            .on_click(cx.listener(|view, _, _, cx| view.retry(cx))),
    )
}

/// The quality a display is opened at: its own refresh, up to 120 frames a second, and full
/// colour asked for (the Mac sends it while its bitrate allows).
fn quality_for(display: &DisplayInfo) -> Quality {
    let hz = f64::from(display.hz);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 30..=120 first"
    )]
    let fps = hz.round().clamp(30.0, 120.0) as u16;
    Quality {
        fps,
        chroma: Chroma::Full,
        ..Quality::default()
    }
}

/// The quality a display made at the window's size is opened at: the window's screen's
/// refresh, up to 120 frames a second, 60 when it is not known, and full colour asked for.
fn made_quality(shape: DisplayShape) -> Quality {
    let fps = if shape.refresh_hz == 0 {
        60
    } else {
        shape.refresh_hz.clamp(30, 120)
    };
    Quality {
        fps,
        chroma: Chroma::Full,
        ..Quality::default()
    }
}

/// The display this window wants made: its content in pixels at its backing scale, and its
/// screen's refresh; `None` while it has no size. The interface zoom shrinks the viewport by
/// what it grows the scale factor, so their product is the content in pixels whatever it is.
fn window_shape(window: &Window, cx: &App) -> Option<DisplayShape> {
    let size = window.viewport_size();
    let width = f32::from(size.width) * window.scale_factor();
    let height = f32::from(size.height) * window.scale_factor();
    let scale = window.scale_factor() / window.content_zoom();
    if width < 1.0 || height < 1.0 {
        return None;
    }
    let refresh = window.refresh_interval(cx).map_or(0.0, |every| {
        let secs = every.as_secs_f64();
        if secs > 0.0 {
            (1.0 / secs).round()
        } else {
            0.0
        }
    });
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "positive window sizes in pixels and a clamped refresh"
    )]
    let shape = DisplayShape {
        width: width.round() as u32,
        height: height.round() as u32,
        scale,
        refresh_hz: refresh.clamp(0.0, 1000.0) as u16,
    };
    Some(shape)
}

/// Why the Mac refused an open, in the person's words.
fn failure_reason(why: &ScreenFailure) -> String {
    match why {
        ScreenFailure::NotPermitted => "Screen Recording is off there".to_owned(),
        ScreenFailure::Gone => "the display is gone".to_owned(),
        ScreenFailure::Failed(words) => words.clone(),
    }
}

/// Why the Mac streams a physical display where one at the window's size was asked for.
const fn no_display_reason(why: NoVirtualDisplay) -> &'static str {
    match why {
        NoVirtualDisplay::Unavailable => "it cannot make displays",
        NoVirtualDisplay::Refused => "macOS refused it",
        NoVirtualDisplay::Unsettled => "it never settled at that size",
        NoVirtualDisplay::Unlisted => "screen capture never listed it",
    }
}

/// What the panel names a pane.
const fn pane_name(pane: Pane) -> &'static str {
    match pane {
        Pane::ScreenRecording => "Screen Recording",
        Pane::Accessibility => "Accessibility",
        Pane::PasteFromOtherApps => "Paste from Other Apps",
    }
}

/// The panel's line for a pane that is off on `host`.
fn missing_line(pane: Pane, host: &str) -> String {
    format!("{} is off on {host}", pane_name(pane))
}

/// What a pane lets Onicode Desktop do.
const fn pane_purpose(pane: Pane) -> &'static str {
    match pane {
        Pane::ScreenRecording => "Onicode Desktop needs it to show the Mac's screen.",
        Pane::Accessibility => "Onicode Desktop needs it to take your keyboard and pointer.",
        Pane::PasteFromOtherApps => "Onicode Desktop needs it to send the Mac's copies unasked.",
    }
}

/// What the panel says once the guide is open.
fn guide_note(pane: Pane, host: &str, local: bool) -> String {
    if local {
        format!(
            "Drag Onicode Desktop into the {} list in System Settings and switch it on. This \
             window joins once it is on.",
            pane_name(pane)
        )
    } else {
        format!(
            "System Settings is open on {host}. Someone at that Mac switches Onicode Desktop on \
             in {}; this window joins once it is on.",
            pane_name(pane)
        )
    }
}

/// The privilege whose list a pane is.
const fn grants_privilege(pane: Pane) -> onicode_grants::Privilege {
    match pane {
        Pane::ScreenRecording => onicode_grants::Privilege::ScreenRecording,
        Pane::Accessibility => onicode_grants::Privilege::Accessibility,
        Pane::PasteFromOtherApps => onicode_grants::Privilege::Pasteboard,
    }
}

/// The helper bundle inside the running app (`…/Onicode.app/Contents/Library/LoginItems/
/// Onicode Desktop.app`); `None` for a bare development binary.
fn local_helper_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe
        .ancestors()
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))?;
    let helper = onicode_grants::bundle::helper_bundle(app);
    helper.exists().then_some(helper)
}

/// A native host for a layer of the picture in the window `here`.
fn new_host(
    here: AnyWindowHandle,
    label: &'static str,
    window: &mut Window,
    cx: &mut App,
) -> Option<Host> {
    let options = NativeHostOptions {
        opaque: true,
        interactive: false,
        label: Some(label.into()),
    };
    window
        .create_native_host(options, cx)
        .inspect_err(|error| log::warn!("no native host for the picture's layer: {error}"))
        .ok()
        .map(|native| Host {
            window: here,
            native,
            tried: false,
        })
}

/// Where a striped picture's stripe goes: its whole layer, and the rows of it shown.
struct Place {
    layer: Bounds<Pixels>,
    shown: Bounds<Pixels>,
}

/// Where a striped picture drawn at `at` puts its two layers, each clipped to its rows; `None`
/// for a picture of one piece.
fn stripe_places(at: Bounds<Pixels>, shape: Shape) -> Option<[Place; 2]> {
    let seam = shape.seam?;
    #[expect(clippy::cast_precision_loss, reason = "pixel counts")]
    let row = |rows: u32| px(rows as f32 * f32::from(at.size.height) / shape.size.1.max(1) as f32);
    let top_layer = Bounds {
        origin: at.origin,
        size: size(at.size.width, row(seam.top)),
    };
    let cut = at.origin.y + row(seam.top_rows);
    let top_shown = Bounds {
        origin: at.origin,
        size: size(at.size.width, cut - at.origin.y),
    };
    let lower_layer = Bounds {
        origin: point(at.origin.x, cut - row(seam.lower_from)),
        size: size(at.size.width, row(seam.lower)),
    };
    let lower_shown = Bounds {
        origin: point(at.origin.x, cut),
        size: size(at.size.width, at.origin.y + at.size.height - cut),
    };
    Some([
        Place {
            layer: top_layer,
            shown: top_shown,
        },
        Place {
            layer: lower_layer,
            shown: lower_shown,
        },
    ])
}

/// The Mac's cursor picture as a system cursor; `None` for one the system cannot wear.
fn system_picture(shape: &CursorShape) -> Option<CursorImage> {
    CursorImage::new(
        shape.bgra.clone(),
        size(
            DevicePixels(i32::from(shape.w)),
            DevicePixels(i32::from(shape.h)),
        ),
        point(
            DevicePixels(i32::from(shape.hot_x)),
            DevicePixels(i32::from(shape.hot_y)),
        ),
        f32::from(shape.scale.max(1)),
    )
    .ok()
}

/// A name for a view's system cursor picture, unique in the process.
fn next_pointer_id() -> CursorImageId {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);
    CursorImageId(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}
