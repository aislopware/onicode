//! A Mac's desktop on this iPhone or iPad: [`DesktopViewer`] over `onicode-desktop-client`.
//!
//! The viewer asks the Mac's engine for its desktop (`CoreClient`'s `open_desktop`), dials
//! desktopd, opens the Mac's main display and hands each decoded picture to the foreign
//! [`DesktopSink`] on the decoder's thread, as a retained `CVPixelBuffer` the Swift side wraps in
//! a `CMSampleBuffer` for its `AVSampleBufferDisplayLayer`. Touch comes back in view points and
//! leaves as Mac input in stream pixels, placed by `onicode_desktop_client::fit`: a trackpad
//! the size of the screen, the pointer moving as far as the finger.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use onicode_client as zc;
use onicode_desktop_client::fit::{Fit, coordinate, pixels};
use onicode_desktop_client::{
    DesktopClient, DesktopSession, Join, Open, Phase, Picture, Presentable, ScreenHandle,
    SessionState,
};
use onicode_desktop_link::{Pane, Seat};
use onicode_desktop_media::FrameStamp;
use onicode_desktop_proto::input::{KeyAction, KeyCode, Mods, MouseButton};
use onicode_desktop_proto::screen::{Quality, ScreenInput, ScrollPhase, SourceState, StreamId};
use tokio::task::JoinHandle;

use crate::client_ffi::{CoreClient, CoreResult};

/// Where the viewer's pictures, state and pointer go. Called on Rust threads: hop to the main
/// thread for anything UIKit.
#[uniffi::export(with_foreign)]
pub trait DesktopSink: Send + Sync {
    /// A picture: `pixel_buffer` is a `CVPixelBufferRef` retained once for the sink, which
    /// takes it (`Unmanaged<CVPixelBuffer>.fromOpaque(..).takeRetainedValue()`) every time.
    /// `token` goes back to [`DesktopViewer::presented`] once it is on the glass.
    fn on_picture(&self, pixel_buffer: u64, width: u32, height: u32, token: u64);
    /// A picture coded as two stripes, each retained once for the sink: `top`'s first
    /// `top_rows` rows, then `lower`'s rows from `lower_from` down, right under them.
    fn on_stripes(&self, top: u64, lower: u64, top_rows: u32, lower_from: u32, token: u64);
    /// The session changed.
    fn on_state(&self, state: DesktopStateInfo);
    /// The Mac's pointer moved or changed its picture.
    fn on_cursor(&self, cursor: DesktopCursor);
}

/// Where the session is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopPhase {
    /// Dialling, or joined and not yet welcomed.
    Connecting,
    /// Showing.
    Joined,
    /// The Mac's grants are off: [`DesktopStateInfo::missing`] names them.
    NotPermitted,
    /// The link dropped; [`DesktopStateInfo::retry_in_ms`] until the next dial.
    Reconnecting,
    /// Over: [`DesktopStateInfo::closed_reason`] says why.
    Closed,
}

/// A System Settings privacy pane on the Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopPane {
    /// Screen & System Audio Recording.
    ScreenRecording,
    /// Accessibility.
    Accessibility,
}

impl From<Pane> for DesktopPane {
    fn from(pane: Pane) -> Self {
        match pane {
            Pane::ScreenRecording => Self::ScreenRecording,
            Pane::Accessibility => Self::Accessibility,
        }
    }
}

impl From<DesktopPane> for Pane {
    fn from(pane: DesktopPane) -> Self {
        match pane {
            DesktopPane::ScreenRecording => Self::ScreenRecording,
            DesktopPane::Accessibility => Self::Accessibility,
        }
    }
}

/// What the view shows of the session.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DesktopStateInfo {
    /// Where it is.
    pub phase: DesktopPhase,
    /// What the Mac calls itself.
    pub host: String,
    /// The panes off on the Mac, while [`DesktopPhase::NotPermitted`].
    pub missing: Vec<DesktopPane>,
    /// Milliseconds until the next dial, while [`DesktopPhase::Reconnecting`].
    pub retry_in_ms: u64,
    /// Why it closed, while [`DesktopPhase::Closed`].
    pub closed_reason: String,
    /// Who holds the controls; `None` while nobody does.
    pub controller: Option<String>,
    /// This device holds them.
    pub you_control: bool,
    /// Devices linked, this one included.
    pub clients: u32,
    /// The Mac refused input from this device: it does not hold the controls.
    pub input_refused: bool,
    /// The picture's size in pixels; zero while no display is open.
    pub width: u32,
    /// Its height.
    pub height: u32,
    /// The display draws (a locked Mac or a sleeping display does not).
    pub live: bool,
    /// The focused field on the Mac takes a password.
    pub secure_field: bool,
    /// Why the display could not be opened, when it could not.
    pub failure: Option<String>,
}

/// The Mac's pointer, in the view's points.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct DesktopCursor {
    /// X, in view points.
    pub x: f32,
    /// Y, in view points.
    pub y: f32,
    /// Over the display.
    pub visible: bool,
    /// Moves when the picture changes: redraw it only then.
    pub shape_serial: u64,
    /// The picture, `None` for the system arrow.
    pub shape: Option<DesktopCursorShape>,
}

/// A pointer's picture.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DesktopCursorShape {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// The hot spot, in pixels.
    pub hot_x: u16,
    /// The hot spot, in pixels.
    pub hot_y: u16,
    /// Premultiplied BGRA, row-major.
    pub bgra: Vec<u8>,
    /// Pixels per point.
    pub scale: u8,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopButton {
    /// The primary button: a tap.
    Left,
    /// The secondary button: a two-finger tap.
    Right,
}

/// A gesture's phase, as `UIGestureRecognizer` has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopGesturePhase {
    /// Not a gesture (a momentum step).
    None,
    /// Began.
    Began,
    /// Changed.
    Changed,
    /// Ended.
    Ended,
    /// Cancelled.
    Cancelled,
}

impl From<DesktopGesturePhase> for ScrollPhase {
    fn from(phase: DesktopGesturePhase) -> Self {
        match phase {
            DesktopGesturePhase::None => Self::None,
            DesktopGesturePhase::Began => Self::Began,
            DesktopGesturePhase::Changed => Self::Changed,
            DesktopGesturePhase::Ended => Self::Ended,
            DesktopGesturePhase::Cancelled => Self::Cancelled,
        }
    }
}

/// A key the on-screen keyboard has that is not text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DesktopKey {
    /// ⌫.
    Backspace,
    /// ⏎.
    Enter,
    /// ⇥.
    Tab,
    /// ⎋.
    Escape,
    /// ←.
    Left,
    /// →.
    Right,
    /// ↑.
    Up,
    /// ↓.
    Down,
    /// ⌦.
    Delete,
    /// Space, with modifiers (⌘-space).
    Space,
}

impl From<DesktopKey> for KeyCode {
    fn from(key: DesktopKey) -> Self {
        match key {
            DesktopKey::Backspace => Self::Backspace,
            DesktopKey::Enter => Self::Enter,
            DesktopKey::Tab => Self::Tab,
            DesktopKey::Escape => Self::Escape,
            DesktopKey::Left => Self::ArrowLeft,
            DesktopKey::Right => Self::ArrowRight,
            DesktopKey::Up => Self::ArrowUp,
            DesktopKey::Down => Self::ArrowDown,
            DesktopKey::Delete => Self::Delete,
            DesktopKey::Space => Self::Space,
        }
    }
}

/// This app's one desktop endpoint, bound on first use.
static DESKTOP: tokio::sync::OnceCell<Arc<DesktopClient>> = tokio::sync::OnceCell::const_new();

/// Pictures handed to the sink whose glass has not been reported, newest last.
const STAMPS: usize = 16;

/// A Mac's desktop, shown.
#[derive(uniffi::Object)]
pub struct DesktopViewer {
    client: zc::Client,
    device_id: String,
    name: String,
    sink: Arc<dyn DesktopSink>,
    inner: Arc<Inner>,
}

struct Inner {
    view: Mutex<View>,
    stamps: Mutex<VecDeque<(u64, FrameStamp)>>,
    next_token: AtomicU64,
    epoch: Instant,
}

/// The viewer's state, under one lock.
#[derive(Default)]
struct View {
    session: Option<DesktopSession>,
    watcher: Option<JoinHandle<()>>,
    cursor: Option<JoinHandle<()>>,
    handle: Option<ScreenHandle>,
    stream: Option<StreamId>,
    /// The view's size in points.
    size: (f32, f32),
    /// The stream's size in pixels, and its pixels per Mac point.
    stream_size: (u32, u32),
    stream_scale: f32,
    /// Where the pointer is, in stream pixels.
    pointer: (f32, f32),
}

impl View {
    fn fit(&self) -> Option<Fit> {
        Fit::new(self.size, self.stream_size)
    }
}

impl Inner {
    fn view(&self) -> std::sync::MutexGuard<'_, View> {
        self.view.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stamp(&self, stamp: FrameStamp) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let mut stamps = self.stamps.lock().unwrap_or_else(PoisonError::into_inner);
        if stamps.len() >= STAMPS {
            stamps.pop_front();
        }
        stamps.push_back((token, stamp));
        token
    }

    /// Microseconds since the viewer started, for a gesture's timestamps.
    fn time_us(&self) -> u32 {
        u32::try_from(self.epoch.elapsed().as_micros() % u128::from(u32::MAX)).unwrap_or(0)
    }
}

/// `image` retained once more, as the address the sink takes it by.
#[cfg(target_vendor = "apple")]
fn retained(image: &Picture) -> u64 {
    let raw = image.clone().into_raw();
    u64::try_from(raw.addr().get()).unwrap_or(0)
}

/// No picture on a platform without a decoder.
#[cfg(not(target_vendor = "apple"))]
fn retained(image: &Picture) -> u64 {
    match *image {}
}

fn u32_of(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

#[uniffi::export]
impl DesktopViewer {
    /// A viewer of the Mac `device_id`, as this device's `name` (the seat shows it), showing
    /// on `sink`. Nothing happens until [`Self::start`].
    #[uniffi::constructor]
    pub fn new(
        client: Arc<CoreClient>,
        device_id: String,
        name: String,
        sink: Arc<dyn DesktopSink>,
    ) -> Arc<Self> {
        Arc::new(Self {
            client: client.client().clone(),
            device_id,
            name,
            sink,
            inner: Arc::new(Inner {
                view: Mutex::new(View::default()),
                stamps: Mutex::new(VecDeque::new()),
                next_token: AtomicU64::new(1),
                epoch: Instant::now(),
            }),
        })
    }

    /// Ask the Mac's engine for its desktop and link to it, taking the controls. Returns once
    /// the engine answered; the link comes up, and stays up, on its own.
    pub async fn start(&self) -> CoreResult<()> {
        let client = self.client.clone();
        let device_id = self.device_id.clone();
        let endpoint =
            zc::runtime::run(async move { client.open_desktop(&device_id).await }).await?;
        let own_id = self.client.device_id().to_owned();
        let join = Join {
            name: self.name.clone(),
            control: true,
            sound: false,
        };
        let (sink, inner) = (Arc::clone(&self.sink), Arc::clone(&self.inner));
        zc::runtime::run(async move {
            let desktop = DESKTOP
                .get_or_try_init(|| DesktopClient::bind(&own_id))
                .await
                .map_err(|e| zc::ClientError::HostUnavailable(e.to_string()))?;
            let session = desktop.connect(endpoint, join);
            let watcher = tokio::spawn(watch(session.clone(), sink, Arc::clone(&inner)));
            let mut view = inner.view();
            if let Some(old) = view.watcher.replace(watcher) {
                old.abort();
            }
            if let Some(old) = view.session.replace(session) {
                old.shutdown();
            }
            Ok(())
        })
        .await
        .map_err(Into::into)
    }

    /// End the session.
    pub fn stop(&self) {
        let mut view = self.inner.view();
        if let Some(session) = view.session.take() {
            session.shutdown();
        }
        for task in [view.watcher.take(), view.cursor.take()]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
        if let Some(handle) = view.handle.take() {
            handle.set_present(None);
        }
    }

    /// The view is `width` × `height` points now.
    pub fn set_view_size(&self, width: f32, height: f32) {
        self.inner.view().size = (width, height);
    }

    /// The picture `token` reached the display `ago_ns` nanoseconds ago (0: just now). Best
    /// effort: it feeds the capture-to-glass timings.
    pub fn presented(&self, token: u64, ago_ns: u64) {
        let stamp = {
            let mut stamps = self
                .inner
                .stamps
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let at = stamps.iter().position(|(t, _)| *t == token);
            at.and_then(|at| stamps.drain(..=at).next_back())
        };
        let Some((_, stamp)) = stamp else { return };
        let handle = self.inner.view().handle.clone();
        if let Some(handle) = handle {
            let at = Instant::now()
                .checked_sub(Duration::from_nanos(ago_ns))
                .unwrap_or_else(Instant::now);
            handle.presented(stamp, Some(at));
        }
    }

    /// A finger moved `dx`, `dy` points: the pointer moves as far over the picture.
    pub fn pointer_moved(&self, dx: f32, dy: f32) {
        let mut view = self.inner.view();
        let (Some(fit), Some(stream)) = (view.fit(), view.stream) else {
            return;
        };
        let (sx, sy) = fit.delta_to_stream(dx, dy);
        let (x, y) = fit.clamp(view.pointer.0 + sx, view.pointer.1 + sy);
        view.pointer = (x, y);
        let session = view.session.clone();
        drop(view);
        if let Some(session) = session {
            session.input(stream, ScreenInput::Move { x, y });
        }
    }

    /// Put the pointer under view point `x`, `y` (a pencil, a pointer device).
    pub fn pointer_at(&self, x: f32, y: f32) {
        let mut view = self.inner.view();
        let (Some(fit), Some(stream)) = (view.fit(), view.stream) else {
            return;
        };
        let (x, y) = fit.to_stream_clamped(x, y);
        view.pointer = (x, y);
        let session = view.session.clone();
        drop(view);
        if let Some(session) = session {
            session.input(stream, ScreenInput::Move { x, y });
        }
    }

    /// Press or release `button` where the pointer is; `clicks` counts a double-click's second.
    pub fn button(&self, button: DesktopButton, down: bool, clicks: u8, mods: u16) {
        let button = match button {
            DesktopButton::Left => MouseButton::Left,
            DesktopButton::Right => MouseButton::Right,
        };
        self.send(|(x, y), _| ScreenInput::Button {
            button,
            down,
            x,
            y,
            clicks: clicks.max(1),
            mods: Mods::from_bits_truncate(mods),
        });
    }

    /// A click where the pointer is: a tap.
    pub fn click(&self, button: DesktopButton, clicks: u8) {
        self.button(button, true, clicks, 0);
        self.button(button, false, clicks, 0);
    }

    /// Two fingers scrolled `dx`, `dy` points, in `phase` (or a momentum step in `momentum`).
    pub fn scroll(
        &self,
        dx: f32,
        dy: f32,
        phase: DesktopGesturePhase,
        momentum: DesktopGesturePhase,
    ) {
        let time_us = self.inner.time_us();
        self.send(|(x, y), (fit, stream_scale)| {
            let (sx, sy) = fit.delta_to_stream(dx, dy);
            let points = |v: f32| {
                if stream_scale > 0.0 {
                    v / stream_scale
                } else {
                    v
                }
            };
            ScreenInput::Scroll {
                dx: points(sx),
                dy: points(sy),
                precise: true,
                phase: phase.into(),
                momentum: momentum.into(),
                x,
                y,
                mods: Mods::empty(),
                time_us,
            }
        });
    }

    /// A pinch changed by `delta` (the scale's change, 0 at rest), in `phase`.
    pub fn magnify(&self, delta: f32, phase: DesktopGesturePhase) {
        let time_us = self.inner.time_us();
        self.send(|(x, y), _| ScreenInput::Magnify {
            delta,
            phase: phase.into(),
            x,
            y,
            time_us,
        });
    }

    /// Text typed on the keyboard.
    pub fn text(&self, text: String) {
        if text.is_empty() {
            return;
        }
        self.send(|_, _| ScreenInput::Text { text: text.clone() });
    }

    /// A key that is not text, pressed and released, with `mods` (`Mods` bits).
    pub fn key(&self, key: DesktopKey, mods: u16) {
        for action in [KeyAction::Press, KeyAction::Release] {
            self.send(|_, _| ScreenInput::Key {
                code: key.into(),
                action,
                mods: Mods::from_bits_truncate(mods),
                chord: None,
            });
        }
    }

    /// Take the controls, or give them up.
    pub fn take_control(&self, take: bool) {
        if let Some(session) = self.inner.view().session.clone() {
            session.take_control(take);
        }
    }

    /// Hear the Mac's sound here, or stop.
    pub fn set_sound(&self, on: bool) {
        if let Some(session) = self.inner.view().session.clone() {
            session.set_sound(on);
        }
    }

    /// Open the guide to `pane` on the Mac's own screen, for the person at it to turn it on.
    /// Never a prompt here or there: System Settings opens at the pane, and desktopd restarts
    /// once it is on.
    pub async fn open_guide(&self, pane: DesktopPane) -> CoreResult<()> {
        let client = self.client.clone();
        let device_id = self.device_id.clone();
        zc::runtime::run(async move { client.open_desktop_guide(&device_id, pane.into()).await })
            .await
            .map_err(Into::into)
    }
}

impl DesktopViewer {
    /// Send the input `make` builds from the pointer (stream pixels) and the fit.
    fn send(&self, make: impl FnOnce((f32, f32), (Fit, f32)) -> ScreenInput) {
        let view = self.inner.view();
        let (Some(fit), Some(stream), Some(session)) =
            (view.fit(), view.stream, view.session.clone())
        else {
            return;
        };
        let input = make(view.pointer, (fit, view.stream_scale));
        drop(view);
        session.input(stream, input);
    }
}

impl Drop for DesktopViewer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Follow the session: open the Mac's main display once joined, attach the sink to the stream
/// it opens, and tell the sink every change.
async fn watch(session: DesktopSession, sink: Arc<dyn DesktopSink>, inner: Arc<Inner>) {
    let mut states = session.state();
    let mut opened = false;
    loop {
        let state = Arc::clone(&*states.borrow_and_update());
        if !opened
            && state.phase == Phase::Joined
            && let Some(main) = state.displays.first()
        {
            session.open(Open::Display(main.id), Quality::default());
            opened = true;
        }
        let view = state.streams.first();
        let stream = view.map(|v| v.stream);
        let attach = {
            let mut guard = inner.view();
            if let Some(v) = view {
                guard.stream_size = (v.width, v.height);
                guard.stream_scale = v.scale;
            }
            (guard.stream != stream).then(|| {
                guard.stream = stream;
                if let Some(task) = guard.cursor.take() {
                    task.abort();
                }
                guard.handle = stream.and_then(|s| session.screen(s));
                guard.pointer =
                    view.map_or((0.0, 0.0), |v| (pixels(v.width / 2), pixels(v.height / 2)));
                guard.handle.clone()
            })
        };
        if let Some(Some(handle)) = attach {
            attach_sink(&handle, &sink, &inner);
        }
        sink.on_state(info(&state));
        if states.changed().await.is_err() {
            return;
        }
    }
}

/// Hand `handle`'s pictures and pointer to `sink`.
fn attach_sink(handle: &ScreenHandle, sink: &Arc<dyn DesktopSink>, inner: &Arc<Inner>) {
    let (pictures, stamps) = (Arc::clone(sink), Arc::clone(inner));
    handle.set_present(Some(Arc::new(move |picture: &Presentable| {
        let token = stamps.stamp(picture.stamp);
        let image = &picture.frame.image;
        match &picture.stripes {
            None => {
                let (width, height) = picture.size();
                pictures.on_picture(retained(image), u32_of(width), u32_of(height), token);
            }
            Some(stitched) => pictures.on_stripes(
                retained(image),
                retained(&stitched.lower.image),
                stitched.top_rows,
                stitched.lower_from,
                token,
            ),
        }
    })));
    let mut cursor = handle.cursor();
    let (sink, inner_c) = (Arc::clone(sink), Arc::clone(inner));
    let task = tokio::spawn(async move {
        loop {
            let c = cursor.borrow_and_update().clone();
            let at = (coordinate(c.x), coordinate(c.y));
            let fit = {
                let mut view = inner_c.view();
                // The Mac moved its pointer itself (an app warped it): follow it.
                view.pointer = at;
                view.fit()
            };
            if let Some(fit) = fit {
                let (x, y) = fit.to_view(at.0, at.1);
                sink.on_cursor(DesktopCursor {
                    x,
                    y,
                    visible: c.visible,
                    shape_serial: c.shape_serial,
                    shape: c.shape.as_deref().map(|s| DesktopCursorShape {
                        width: s.w,
                        height: s.h,
                        hot_x: s.hot_x,
                        hot_y: s.hot_y,
                        bgra: s.bgra.clone(),
                        scale: s.scale,
                    }),
                });
            }
            if cursor.changed().await.is_err() {
                return;
            }
        }
    });
    inner.view().cursor = Some(task);
}

fn info(state: &SessionState) -> DesktopStateInfo {
    let (phase, missing, retry_in_ms, closed_reason) = match &state.phase {
        Phase::Connecting => (DesktopPhase::Connecting, Vec::new(), 0, String::new()),
        Phase::Joined => (DesktopPhase::Joined, Vec::new(), 0, String::new()),
        Phase::NotPermitted { missing } => (
            DesktopPhase::NotPermitted,
            missing.iter().copied().map(Into::into).collect(),
            0,
            String::new(),
        ),
        Phase::Reconnecting { retry_in } => (
            DesktopPhase::Reconnecting,
            Vec::new(),
            u64::try_from(retry_in.as_millis()).unwrap_or(u64::MAX),
            String::new(),
        ),
        Phase::Closed { reason } => (DesktopPhase::Closed, Vec::new(), 0, reason.clone()),
    };
    let Seat {
        controller,
        you,
        clients,
    } = state.seat.clone();
    let view = state.streams.first();
    DesktopStateInfo {
        phase,
        host: state.host.clone(),
        missing,
        retry_in_ms,
        closed_reason,
        controller,
        you_control: you,
        clients,
        input_refused: state.input_refused,
        width: view.map_or(0, |v| v.width),
        height: view.map_or(0, |v| v.height),
        live: view.is_none_or(|v| v.source == SourceState::Live),
        secure_field: view.and_then(|v| v.field).is_some_and(|f| f.secure),
        failure: state.failure.as_ref().map(|(_, why)| format!("{why:?}")),
    }
}
