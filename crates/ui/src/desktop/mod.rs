//! A Mac's desktop in a window of its own: the picture, letterboxed on a dark stage, and this
//! Mac's keyboard, pointer and trackpad on it (slopty-ui's `screen.rs`, `screen/glass.rs`,
//! `screen/keyboard.rs`, `screen/health.rs`, `screen/controls.rs` and `workspace/desktop.rs`,
//! as one view).
//!
//! Opening is two steps. The view asks this Mac's engine for the desktop
//! (`methods::OPEN_DESKTOP`, forwarded to the target Mac's engine by `targetDeviceId`), which
//! answers with where the Mac's `onicode-desktopd` listens; `DesktopClient::connect` then dials
//! desktopd directly over the tailnet and keeps the link up. The session's state
//! (`SessionState`) drives everything the view shows: the welcome, who holds the controls, the
//! grants the Mac lacks, the stream.
//!
//! The picture never takes a GPUI frame: each decoded picture goes from the decoder's thread to
//! a `VideoLayer` in a native host (`glass`); the view only places the layer. The Mac's
//! pointer is this Mac's system cursor, wearing the Mac's cursor picture.
//!
//! Only the device holding the controls ([`Seat`]) sends input: the seat badge says who holds
//! them, with a "Take control" button. A Mac whose Screen Recording or Accessibility is off
//! refuses the session; the view names the pane and the Mac, with a "Turn on" button that opens
//! the guide on that Mac's screen: here through `onicode_grants::guide::show`, on
//! another Mac through its engine (`methods::DESKTOP_OPEN_GUIDE`).

mod glass;
mod hud;
mod input;
mod keys;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::composition::{NativeHost, NativeHostOptions};
use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, App, Bounds, ContentMask, Context, CursorImage, CursorImageId, CursorStyle,
    DevicePixels, DispatchPhase, Entity, FocusHandle, Hsla, InteractiveGestures as _, KeyDownEvent,
    KeyUpEvent, ModifiersChangedEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PinchEvent,
    Pixels, RotateEvent, ScrollDelta, ScrollEvent, SharedString, SmartMagnifyEvent, Subscription,
    SwipeEvent, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div, point, px,
    size,
};
use gpui_tokio::Tokio;
use onicode_desktop_client::{
    CursorState, DesktopClient, DesktopEndpoint, DesktopSession, Join, Open, Phase, Presentable,
    ScreenHandle, SessionState,
};
use onicode_desktop_link::broker::{GrantsReply, OpenGuideParams};
use onicode_desktop_link::{HostGrants, Pane, Seat};
use onicode_desktop_proto::screen::{
    Chroma, CursorShape, CurtainState, DisplayInfo, Quality, ScreenInput,
};
use onicode_desktop_proto::{DisplayId, StreamId};
use onicode_rpc::methods;
use tokio::sync::watch;

use self::glass::{Glass, Layer, Reported, Seam, Shape};
use self::input::{Input, MOMENTUM_GAP, Placement, Scroll, proto_button};
use self::keys::KeyPlatform;
use crate::settings::widgets::{self, ActionTone};
use crate::state::AppState;
use crate::theme::Theme;

/// The stage under the picture: the bars a letterboxed picture leaves stay dark whatever the
/// theme, as a video player's do.
const STAGE: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 0.035,
    a: 1.0,
};

/// While this Mac's grants guide is open, its grants are read this often for it.
const GUIDE_POLL: Duration = Duration::from_secs(1);

/// The overlay's figures refresh this often while it is shown.
const HUD_EVERY: Duration = Duration::from_secs(1);

/// The app's one desktop viewer endpoint on the tailnet, bound on the first desktop opened.
static CLIENT: tokio::sync::OnceCell<Arc<DesktopClient>> = tokio::sync::OnceCell::const_new();

/// The Mac a view shows.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Target {
    /// Its device id.
    pub device_id: String,
    /// What the person calls it.
    pub name: String,
    /// It is this Mac.
    pub local: bool,
}

/// Open a window on the desktop of the Mac `device_id`, named `name`.
pub fn open_window(state: Entity<AppState>, device_id: String, name: String, cx: &mut App) {
    let local = state.read(cx).local_device_id.as_deref() == Some(device_id.as_str());
    let target = Target {
        device_id,
        name,
        local,
    };
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
        cx.new(|cx| DesktopView::new(target, Some(state), window, cx))
    });
    if let Err(error) = opened {
        tracing::warn!(%error, "no window for the desktop");
    }
}

/// What the view asks of the session. A seam: the app's is `DesktopSession`, tests record.
pub(crate) trait Link {
    /// The session's state as it changes; `None` for a link that has none (tests).
    fn state(&self) -> Option<watch::Receiver<Arc<SessionState>>>;
    /// Stream `display`.
    fn open(&self, display: DisplayId, quality: Quality);
    /// Stop streaming `stream`.
    fn close(&self, stream: StreamId);
    /// Input for `stream`.
    fn input(&self, stream: StreamId, input: ScreenInput);
    /// `stream`'s view has the keyboard, or lost it.
    fn focused(&self, stream: StreamId, focused: bool);
    /// Take the controls, or give them up.
    fn take_control(&self, take: bool);
    /// Hear the Mac's sound, or stop.
    fn set_sound(&self, on: bool);
    /// Draw the curtain over the Mac's own screens, or let it go.
    fn curtain(&self, on: bool);
    /// `stream`'s pictures and cursor.
    fn screen(&self, stream: StreamId) -> Option<ScreenHandle>;
    /// End the session.
    fn shutdown(&self);
}

impl Link for DesktopSession {
    fn state(&self) -> Option<watch::Receiver<Arc<SessionState>>> {
        Some(DesktopSession::state(self))
    }

    fn open(&self, display: DisplayId, quality: Quality) {
        DesktopSession::open(self, Open::Display(display), quality);
    }

    fn close(&self, stream: StreamId) {
        DesktopSession::close(self, stream);
    }

    fn input(&self, stream: StreamId, input: ScreenInput) {
        DesktopSession::input(self, stream, input);
    }

    fn focused(&self, stream: StreamId, focused: bool) {
        DesktopSession::focused(self, stream, focused);
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

    fn shutdown(&self) {
        DesktopSession::shutdown(self);
    }
}

/// Where the session is, as the view shows it.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) enum Stage {
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
pub(crate) struct StreamSnap {
    /// Its id.
    pub stream: StreamId,
    /// Its size in pixels: the coordinates input goes in.
    pub size: (u32, u32),
    /// Bits a second the Mac sends.
    pub rate_bps: u64,
}

/// What the view reads of the session's state, read in one place.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct Snapshot {
    pub stage: Stage,
    /// What the Mac calls itself.
    pub host: String,
    pub displays: Vec<DisplayInfo>,
    pub seat: Seat,
    /// What the Mac may do, as desktopd last read it.
    pub grants: HostGrants,
    /// The first stream: the view shows one display.
    pub stream: Option<StreamSnap>,
    /// The curtain is drawn over the Mac's own screens.
    pub curtain: bool,
}

impl Snapshot {
    pub(crate) fn of(state: &SessionState) -> Self {
        let stage = match &state.phase {
            Phase::Connecting => Stage::Connecting,
            Phase::Joined => Stage::Joined,
            Phase::NotPermitted { missing } => Stage::NotPermitted(missing.clone()),
            Phase::Reconnecting { retry_in } => Stage::Reconnecting(*retry_in),
            Phase::Closed { reason } => Stage::Closed(reason.to_string()),
        };
        Self {
            stage,
            host: state.host.to_string(),
            displays: state.displays.clone(),
            seat: state.seat.clone(),
            grants: state.grants,
            stream: state.streams.first().map(|view| StreamSnap {
                stream: view.stream,
                size: (view.width, view.height),
                rate_bps: u64::from(view.rate_bps),
            }),
            curtain: matches!(state.curtain, Some(CurtainState::Up { .. })),
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

/// Makes the layer a stream's pictures go up on, in a native host.
pub(crate) type MakeLayer = Rc<dyn Fn(&NativeHost, Reported) -> anyhow::Result<Box<dyn Layer>>>;

/// The app's layers: a `VideoLayer` in the host.
fn video_layer(host: &NativeHost, reported: Reported) -> anyhow::Result<Box<dyn Layer>> {
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

/// A Mac's desktop.
pub struct DesktopView {
    target: Target,
    app_state: Option<Entity<AppState>>,
    link: Option<Rc<dyn Link>>,
    opening: Opening,
    snap: Snapshot,
    /// The display opened since the session was last welcomed.
    asked: Option<DisplayId>,
    /// The display the person picked; the first one until they pick.
    chosen: Option<DisplayId>,
    screen: Option<Screen>,
    input: Input,
    keyboard: Rc<dyn KeyPlatform>,
    make_layer: MakeLayer,
    focus: FocusHandle,
    /// The stage's bounds in the window, as last laid out.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The wire's input clock starts here.
    epoch: Instant,
    /// The view has the keyboard.
    focused: bool,
    hud: bool,
    sound: bool,
    /// The pane whose guide the person opened, until it is on.
    guide: Option<Pane>,
    guide_error: Option<SharedString>,
    momentum_end: Option<Task<()>>,
    hud_tick: Option<Task<()>>,
    state_task: Option<Task<()>>,
    open_task: Option<Task<()>>,
    guide_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl DesktopView {
    /// A view of `target`'s desktop that opens it at once.
    pub fn new(
        target: Target,
        app_state: Option<Entity<AppState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::bare(
            target,
            app_state,
            Rc::new(keys::System),
            Rc::new(video_layer),
            window,
            cx,
        );
        view.connect(cx);
        view
    }

    /// A view on `link`, reading `keyboard` and putting pictures up through `make_layer`
    /// (tests).
    #[cfg(test)]
    pub(crate) fn linked_for_test(
        target: Target,
        link: Rc<dyn Link>,
        keyboard: Rc<dyn KeyPlatform>,
        make_layer: MakeLayer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::bare(target, None, keyboard, make_layer, window, cx);
        view.linked(Ok(link), cx);
        view
    }

    fn bare(
        target: Target,
        app_state: Option<Entity<AppState>>,
        keyboard: Rc<dyn KeyPlatform>,
        make_layer: MakeLayer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus_in(&focus, window, |view, _window, cx| {
                view.focus_changed(true, cx)
            }),
            cx.on_blur(&focus, window, |view, _window, cx| {
                view.focus_changed(false, cx)
            }),
            cx.observe_window_activation(window, |view, window, cx| {
                let active = window.is_window_active();
                if !active {
                    view.let_go(cx);
                }
                view.follow_clipboard(active && view.focused, cx);
            }),
            cx.on_release(|view, cx| view.follow_clipboard(false, cx)),
        ];
        window.focus(&focus, cx);
        Self {
            target,
            app_state,
            link: None,
            opening: Opening::Asking,
            snap: Snapshot::default(),
            asked: None,
            chosen: None,
            screen: None,
            input: Input::default(),
            keyboard,
            make_layer,
            focus,
            bounds: Rc::new(Cell::new(Bounds::default())),
            epoch: Instant::now(),
            focused: false,
            hud: false,
            sound: true,
            guide: None,
            guide_error: None,
            momentum_end: None,
            hud_tick: None,
            state_task: None,
            open_task: None,
            guide_task: None,
            _subscriptions: subscriptions,
        }
    }

    // Opening.

    /// Ask the Mac's engine for its desktop and dial it.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let Some(app_state) = self.app_state.clone() else {
            return;
        };
        let (engine, me, my_name) = {
            let state = app_state.read(cx);
            let me = state.local_device_id.clone();
            let my_name = me
                .as_deref()
                .and_then(|id| state.device_name(id))
                .unwrap_or("Mac")
                .to_owned();
            (state.engine().cloned(), me, my_name)
        };
        let (Some(engine), Some(me)) = (engine, me) else {
            self.opening = Opening::Failed("Onicode's engine is not running yet.".into());
            cx.notify();
            return;
        };
        let params = if self.target.local {
            serde_json::json!({})
        } else {
            serde_json::json!({ "targetDeviceId": self.target.device_id })
        };
        // This Mac's own desktop is for looking at: taking its controls would chase its own
        // pointer.
        let join = Join {
            name: my_name,
            control: !self.target.local,
            sound: self.sound && !self.target.local,
        };
        self.opening = Opening::Asking;
        cx.notify();
        let open = Tokio::spawn(cx, async move {
            let reply = engine
                .client()
                .call(methods::OPEN_DESKTOP, params)
                .await
                .map_err(|error| format!("The Mac did not open its desktop: {error}"))?;
            let endpoint: DesktopEndpoint = serde_json::from_value(reply)
                .map_err(|error| format!("The Mac answered with something else: {error}"))?;
            let client = CLIENT
                .get_or_try_init(|| DesktopClient::bind(&me))
                .await
                .map_err(|error| format!("This Mac is not on the tailnet: {error}"))?;
            Ok::<_, String>(client.connect(endpoint, join))
        });
        self.open_task = Some(cx.spawn(async move |this, cx| {
            let result = open
                .await
                .map_err(|error| error.to_string())
                .and_then(|opened| opened)
                .map(|session| Rc::new(session) as Rc<dyn Link>);
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
        if let Some(link) = self.link.take() {
            link.shutdown();
        }
    }

    /// The session's state changed.
    pub(crate) fn observe(&mut self, snap: Snapshot, cx: &mut Context<Self>) {
        if snap.stage != Stage::Joined {
            self.asked = None;
        }
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
        cx.notify();
    }

    /// Stream the display the person picked, or the first.
    fn open_display(&mut self, displays: &[DisplayInfo]) {
        let Some(link) = &self.link else {
            return;
        };
        let display = self
            .chosen
            .and_then(|id| displays.iter().find(|d| d.id == id))
            .or_else(|| displays.first());
        let Some(display) = display else {
            return;
        };
        link.open(display.id, quality_for(display));
        self.asked = Some(display.id);
    }

    /// Show `display` instead.
    fn choose_display(&mut self, display: DisplayId, cx: &mut Context<Self>) {
        if self.asked == Some(display) {
            return;
        }
        self.chosen = Some(display);
        if let (Some(link), Some(screen)) = (&self.link, &self.screen) {
            link.close(screen.stream);
        }
        self.screen = None;
        self.asked = None;
        let displays = self.snap.displays.clone();
        self.open_display(&displays);
        cx.notify();
    }

    /// Show `stream`: its pictures to a glass of its own, its cursor to the system cursor.
    fn attach_screen(&mut self, stream: StreamSnap, cx: &mut Context<Self>) {
        self.screen = None;
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
        if let Some(link) = &self.link
            && self.focused
        {
            link.focused(stream.stream, true);
        }
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

    fn toggle_hud(&mut self, cx: &mut Context<Self>) {
        self.hud = !self.hud;
        self.hud_tick = self.hud.then(|| {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(HUD_EVERY).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            })
        });
        cx.notify();
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
                Ok(()) => self.follow_guide(pane, cx),
                Err(error) => {
                    tracing::warn!(?error, "the grants guide did not open");
                    self.guide_error = Some("The guide did not open.".into());
                }
            }
            cx.notify();
            return;
        }
        // Another Mac, or this one in a development build with no helper bundle: the Mac's
        // engine opens the guide beside desktopd.
        let Some(engine) = self
            .app_state
            .as_ref()
            .and_then(|state| state.read(cx).engine().cloned())
        else {
            cx.notify();
            return;
        };
        let mut params = serde_json::to_value(OpenGuideParams { pane }).unwrap_or_default();
        if !self.target.local
            && let Some(map) = params.as_object_mut()
        {
            map.insert(
                "targetDeviceId".to_owned(),
                self.target.device_id.clone().into(),
            );
        }
        self.guide_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .call(methods::DESKTOP_OPEN_GUIDE, params)
                .await;
            let _gone = this.update(cx, |view, cx| {
                if let Err(error) = result {
                    tracing::warn!(%error, "the Mac did not open the grants guide");
                    view.guide_error = Some("The Mac did not open the guide.".into());
                }
                view.guide_task = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// While this Mac's guide is open, read desktopd's grants through the engine and hand each
    /// reading of `pane` to the guide, which walks to "granted" when it comes on. The session's
    /// own grants go to it too ([`Self::observe`]); this covers a desktopd that is restarting.
    fn follow_guide(&mut self, pane: Pane, cx: &mut Context<Self>) {
        let Some(engine) = self
            .app_state
            .as_ref()
            .and_then(|state| state.read(cx).engine().cloned())
        else {
            return;
        };
        self.guide_task = Some(cx.spawn(async move |this, cx| {
            while onicode_grants::guide::is_open() {
                if let Ok(reply) = engine
                    .client()
                    .call(methods::DESKTOP_GRANTS, serde_json::json!({}))
                    .await
                    && let Ok(reply) = serde_json::from_value::<GrantsReply>(reply)
                {
                    onicode_grants::guide::report(!reply.grants.missing().contains(&pane));
                }
                cx.background_executor().timer(GUIDE_POLL).await;
            }
            let _gone = this.update(cx, |view, _cx| view.guide_task = None);
        }));
    }

    // Input.

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
        if let (Some(link), Some(screen)) = (&self.link, &self.screen) {
            link.focused(screen.stream, focused);
        }
        self.follow_clipboard(focused, cx);
    }

    /// While this viewer has focus the clipboard follows its Mac both ways; this Mac's own
    /// desktop needs no following.
    fn follow_clipboard(&self, focused: bool, cx: &mut App) {
        if !self.target.local {
            crate::settings::clipboard::follow(
                self.app_state.as_ref(),
                &self.target.device_id,
                focused,
                cx,
            );
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
                Err(error) => tracing::warn!(%error, "no video layer for the picture"),
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
                Err(error) => tracing::warn!(%error, "no video layer for the lower stripe"),
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
    fn seat_badge(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
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
        pill(theme, "desktop-seat")
            .debug_selector(|| "desktop-seat".to_owned())
            .child(div().child(SharedString::from(who)))
            .when(!seat.you && !self.target.local, |badge| {
                badge.child(
                    widgets::text_action(theme, ActionTone::Solid, "Take control")
                        .id("desktop-take-control")
                        .debug_selector(|| "desktop-take-control".to_owned())
                        .role(gpui::Role::Button)
                        .on_click(cx.listener(|view, _, _, cx| view.take_control(true, cx))),
                )
            })
            .when(seat.you, |badge| {
                badge.child(
                    widgets::text_action(theme, ActionTone::Quiet, "Release")
                        .id("desktop-release-control")
                        .role(gpui::Role::Button)
                        .on_click(cx.listener(|view, _, _, cx| view.take_control(false, cx))),
                )
            })
    }

    /// The display picker and the toggles.
    fn controls(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let showing = self.asked;
        let displays: Vec<(DisplayId, String)> = self
            .snap
            .displays
            .iter()
            .enumerate()
            .map(|(n, d)| (d.id, format!("Display {}", n.saturating_add(1))))
            .collect();
        let many = displays.len() > 1;
        let mut bar = pill(theme, "desktop-controls");
        if many {
            for (n, (id, label)) in displays.into_iter().enumerate() {
                let tone = if showing == Some(id) {
                    ActionTone::Filled
                } else {
                    ActionTone::Quiet
                };
                bar = bar.child(
                    widgets::text_action(theme, tone, label)
                        .id(("desktop-display", n))
                        .role(gpui::Role::Button)
                        .on_click(cx.listener(move |view, _, _, cx| view.choose_display(id, cx))),
                );
            }
        }
        let toggle = |id: &'static str, label: &'static str, on: bool| {
            let tone = if on {
                ActionTone::Filled
            } else {
                ActionTone::Quiet
            };
            widgets::text_action(theme, tone, label)
                .id(id)
                .role(gpui::Role::Button)
        };
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
            toggle("desktop-stats", "Stats", self.hud)
                .on_click(cx.listener(|view, _, _, cx| view.toggle_hud(cx))),
        )
    }

    /// The overlay's figures, when shown and a stream is up.
    fn hud_line(&self, theme: &Theme) -> Option<impl IntoElement + use<>> {
        if !self.hud {
            return None;
        }
        let screen = self.screen.as_ref()?;
        let stats = screen.handle.as_ref().map(ScreenHandle::stats);
        let fps = self
            .snap
            .displays
            .iter()
            .find(|d| Some(d.id) == self.asked)
            .map_or(60, |d| quality_for(d).fps);
        let input = hud::HudInput {
            target_fps: fps,
            paint: screen.glass.rate(),
            pacing: screen.glass.pacing(),
            capture: screen.glass.glass().capture,
            replaced: screen.glass.replaced(),
            mbps: hud::mbps(self.snap.stream.map_or(0, |s| s.rate_bps)),
            chroma: screen.shape.map(|shape| shape.chroma),
            loss: stats.as_ref().map(hud::Loss::of).unwrap_or_default(),
            // The receiver counters carry no round trip yet.
            rtt: None,
        };
        let seams = screen.glass.seams();
        let mut line = pill(theme, "desktop-hud")
            .debug_selector(|| "desktop-hud".to_owned())
            .font_family(theme.font_mono.clone())
            .role(gpui::Role::Status);
        for figure in hud::summary(&input) {
            let color = if figure.warn {
                theme.warning
            } else {
                theme.text
            };
            line = line.child(
                div()
                    .text_color(color)
                    .child(SharedString::from(figure.text)),
            );
        }
        if seams.split > 0 {
            line = line.child(
                div()
                    .text_color(theme.warning)
                    .child(SharedString::from(format!("{} torn", seams.split))),
            );
        }
        Some(line)
    }

    /// What stands in for the picture while there is none: opening, a refusal, a drop.
    fn notice(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let host = if self.snap.host.is_empty() {
            self.target.name.clone()
        } else {
            self.snap.host.clone()
        };
        let card = || {
            div()
                .id("desktop-notice")
                .debug_selector(|| "desktop-notice".to_owned())
                .flex()
                .flex_col()
                .gap(px(10.0))
                .max_w(px(440.0))
                .p(px(20.0))
                .rounded(px(12.0))
                .bg(theme.surface_dialog)
                .border_1()
                .border_color(theme.border)
                .text_color(theme.text)
                .text_size(crate::typography::ui_rems(13.0))
                .occlude()
        };
        let muted = |text: String| {
            div()
                .text_color(theme.text_muted)
                .child(SharedString::from(text))
        };
        let card = match (&self.opening, &self.snap.stage) {
            (Opening::Failed(error), _) => card()
                .role(gpui::Role::Alert)
                .child(div().child(SharedString::from(format!(
                    "{}'s desktop did not open",
                    self.target.name
                ))))
                .child(muted(error.to_string()))
                .child(retry_button(theme, cx)),
            (Opening::Asking, _) | (Opening::Linked, Stage::Connecting) => card()
                .role(gpui::Role::Status)
                .child(div().child(SharedString::from(format!(
                    "Connecting to {}…",
                    self.target.name
                )))),
            (Opening::Linked, Stage::Reconnecting(retry_in)) => card()
                .role(gpui::Role::Status)
                .child(div().child(SharedString::from(format!("{host} dropped off"))))
                .child(muted(format!(
                    "Trying again in {} s.",
                    retry_in.as_secs().max(1)
                ))),
            (Opening::Linked, Stage::Closed(reason)) => card()
                .role(gpui::Role::Alert)
                .child(div().child(SharedString::from(format!("{host}'s desktop closed"))))
                .child(muted(reason.clone()))
                .child(retry_button(theme, cx)),
            (Opening::Linked, Stage::NotPermitted(missing)) => {
                let mut card = card().role(gpui::Role::Alert);
                for &pane in missing {
                    let asked = self.guide == Some(pane);
                    card = card
                        .child(div().child(SharedString::from(missing_line(pane, &host))))
                        .child(muted(if asked {
                            guide_note(pane, &host, self.target.local)
                        } else {
                            pane_purpose(pane).to_owned()
                        }))
                        .child(
                            widgets::text_action(theme, ActionTone::Solid, "Turn on")
                                .id(SharedString::from(format!("desktop-turn-on-{pane:?}")))
                                .debug_selector(move || format!("desktop-turn-on-{pane:?}"))
                                .role(gpui::Role::Button)
                                .aria_label(format!("Turn on {} on {host}", pane_name(pane)))
                                .on_click(
                                    cx.listener(move |view, _, _, cx| view.turn_on(pane, cx)),
                                ),
                        );
                }
                if let Some(error) = &self.guide_error {
                    card = card.child(div().text_color(theme.danger).child(error.clone()));
                }
                card
            }
            (Opening::Linked, Stage::Joined)
                if self.screen.as_ref().is_none_or(|s| s.shape.is_none()) =>
            {
                card()
                    .role(gpui::Role::Status)
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
        let theme = Theme::of(cx).clone();
        let notice = self.notice(&theme, cx);
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
                        .child(self.controls(&theme, cx))
                        .children(self.hud_line(&theme)),
                )
                .child(self.seat_badge(&theme, cx))
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
            .font_family(theme.font_sans.clone())
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

/// An overlay pill: the onicode chrome over the dark stage, holding the pointer off the
/// picture.
fn pill(theme: &Theme, id: &'static str) -> gpui::Stateful<gpui::Div> {
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
        .bg(theme.surface_overlay)
        .border_1()
        .border_color(theme.border)
        .text_color(theme.text)
        .text_size(crate::typography::ui_rems(12.0))
        .occlude()
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
}

fn retry_button(theme: &Theme, cx: &mut Context<DesktopView>) -> impl IntoElement + use<> {
    widgets::text_action(theme, ActionTone::Filled, "Try again")
        .id("desktop-retry")
        .role(gpui::Role::Button)
        .on_click(cx.listener(|view, _, _, cx| view.retry(cx)))
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

/// What the panel names a pane.
#[must_use]
pub(crate) const fn pane_name(pane: Pane) -> &'static str {
    match pane {
        Pane::ScreenRecording => "Screen Recording",
        Pane::Accessibility => "Accessibility",
    }
}

/// The panel's line for a pane that is off on `host`.
#[must_use]
pub(crate) fn missing_line(pane: Pane, host: &str) -> String {
    format!("{} is off on {host}", pane_name(pane))
}

/// What a pane lets Onicode Desktop do.
const fn pane_purpose(pane: Pane) -> &'static str {
    match pane {
        Pane::ScreenRecording => "Onicode Desktop needs it to show the Mac's screen.",
        Pane::Accessibility => "Onicode Desktop needs it to take your keyboard and pointer.",
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
    }
}

/// The helper bundle inside the running app (`…/Onicode.app/Contents/Library/LoginItems/
/// Onicode Desktop.app`); `None` for a bare development binary.
pub(crate) fn local_helper_bundle() -> Option<PathBuf> {
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
        .inspect_err(|error| tracing::warn!(%error, "no native host for the picture's layer"))
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
