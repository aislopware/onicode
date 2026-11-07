//! The view in a test window: a recording link stands in for the session, the inert keyboard
//! for AppKit's, a stand-in for the shortcut grab, and a test layer for the `VideoLayer`. No
//! real window opens, nothing takes focus, nothing plays, no capture device is touched, no
//! shortcut is taken from the system, and no engine is asked: the view under test has none, so
//! "Turn on" only records the pane.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
use onicode_desktop_link::{Pane, Seat};
use onicode_desktop_proto::input::{KeyAction, KeyCode, Mods, MouseButton as ProtoButton};
use onicode_desktop_proto::screen::{
    CaptureTarget, DisplayInfo, DisplayKey, DisplayShape, OpenAsk, Quality, ScreenFailure,
    ScreenInput, SourceState, VirtualDisplay,
};
use onicode_desktop_proto::{DisplayId, StreamId};
use tokio::sync::watch;

use super::glass::tests::{TestLayer, test_buffer, test_stamp};
use super::glass::{Layer, Picture};
use super::input::Placement;
use super::keys::{KeyKind, NativeKey};
use super::shortcuts::{Refused, Shortcuts, Taken};
use super::*;

/// What the view asked of the session.
#[derive(Clone, PartialEq, Debug)]
enum Call {
    Open(Open, u16),
    Resize(StreamId, DisplayShape),
    Close(StreamId),
    Input(StreamId, ScreenInput),
    Focused(StreamId, bool),
    Control(bool),
    Sound(bool),
    Curtain(bool),
    Shutdown,
}

#[derive(Default)]
struct Recorder {
    calls: RefCell<Vec<Call>>,
}

impl Recorder {
    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut self.calls.borrow_mut())
    }

    fn push(&self, call: Call) {
        self.calls.borrow_mut().push(call);
    }
}

impl Link for Recorder {
    fn state(&self) -> Option<watch::Receiver<Arc<SessionState>>> {
        None
    }

    fn open(&self, open: Open, quality: Quality) {
        self.push(Call::Open(open, quality.fps));
    }

    fn resize(&self, stream: StreamId, shape: DisplayShape) {
        self.push(Call::Resize(stream, shape));
    }

    fn close(&self, stream: StreamId) {
        self.push(Call::Close(stream));
    }

    fn input(&self, stream: StreamId, input: ScreenInput) {
        self.push(Call::Input(stream, input));
    }

    fn focused(&self, stream: StreamId, focused: bool) {
        self.push(Call::Focused(stream, focused));
    }

    fn take_control(&self, take: bool) {
        self.push(Call::Control(take));
    }

    fn set_sound(&self, on: bool) {
        self.push(Call::Sound(on));
    }

    fn curtain(&self, on: bool) {
        self.push(Call::Curtain(on));
    }

    fn screen(&self, _stream: StreamId) -> Option<ScreenHandle> {
        None
    }

    fn shutdown(&self) {
        self.push(Call::Shutdown);
    }
}

const STREAM: StreamId = StreamId(7);

/// The key the view under test has for the display a Mac makes for it.
const KEY: DisplayKey = DisplayKey([7; 16]);

/// A grab that takes what [`Taken`] takes when `refuse` is `None`, keeping a clone to offer
/// keys through as the monitor would.
#[derive(Default)]
struct Grab {
    refuse: Option<Refused>,
    taken: RefCell<Option<Taken>>,
}

impl Grab {
    /// Offer `key` as the monitor would; whether it was taken.
    fn offer(&self, key: NativeKey) -> bool {
        self.taken
            .borrow()
            .as_ref()
            .is_some_and(|taken| taken.offer(key))
    }
}

impl Shortcuts for Grab {
    fn grab(&self, wake: Box<dyn Fn()>) -> Result<Taken, Refused> {
        if let Some(refused) = self.refuse {
            return Err(refused);
        }
        let taken = Taken::new(wake, None);
        *self.taken.borrow_mut() = Some(taken.clone());
        Ok(taken)
    }
}

fn display(id: u32, hz: f32) -> DisplayInfo {
    DisplayInfo {
        id: DisplayId(id),
        w: 1512.0,
        h: 982.0,
        scale: 2.0,
        hz,
    }
}

/// The view of a Mac called "studio" in a test window, on a recording link.
fn view<'a>(
    cx: &'a mut TestAppContext,
    layer: &TestLayer,
) -> (Entity<DesktopView>, Rc<Recorder>, &'a mut VisualTestContext) {
    view_grabbing(cx, layer, Rc::new(shortcuts::Inert))
}

/// [`view`], its shortcuts grabbed through `shortcuts`.
fn view_grabbing<'a>(
    cx: &'a mut TestAppContext,
    layer: &TestLayer,
    shortcuts: Rc<dyn Shortcuts>,
) -> (Entity<DesktopView>, Rc<Recorder>, &'a mut VisualTestContext) {
    let recorder = Rc::new(Recorder::default());
    let link: Rc<dyn Link> = recorder.clone();
    let layer = layer.clone();
    let make: MakeLayer =
        Rc::new(move |_host, _reported| Ok(Box::new(layer.clone()) as Box<dyn Layer>));
    let (view, cx) = cx.add_window_view(|window, cx| {
        let target = Target {
            name: "studio".into(),
            local: false,
        };
        DesktopView::linked_for_test(
            target,
            link,
            Rc::new(keys::Inert),
            shortcuts,
            make,
            window,
            cx,
        )
    });
    cx.run_until_parked();
    (view, recorder, cx)
}

/// A welcomed session showing a 200 × 200 stream, the controls held by `controller`.
fn joined(you: bool) -> Snapshot {
    Snapshot {
        stage: Stage::Joined,
        host: "studio".into(),
        displays: vec![display(1, 120.0)],
        seat: Seat {
            controller: Some(if you { "here" } else { "iPad" }.to_owned()),
            you,
            clients: 2,
        },
        stream: Some(StreamSnap {
            stream: STREAM,
            size: (200, 200),
            rate_bps: 20_000_000,
            source: SourceState::Live,
            made: None,
        }),
        ..Snapshot::default()
    }
}

/// [`joined`], showing the display made for this window.
fn joined_made(you: bool) -> Snapshot {
    let mut snap = joined(you);
    if let Some(stream) = snap.stream.as_mut() {
        stream.made = Some(VirtualDisplay::Made(DisplayId(9)));
    }
    snap
}

/// The opens among `calls`.
fn opens(calls: &[Call]) -> Vec<&Call> {
    calls
        .iter()
        .filter(|call| matches!(call, Call::Open(..)))
        .collect()
}

/// The display the view wants made now.
fn shape_of(view: &Entity<DesktopView>, cx: &mut VisualTestContext) -> DisplayShape {
    view.read_with(cx, |view, _| view.shape)
        .expect("the window has a size")
}

fn observe(view: &Entity<DesktopView>, cx: &mut VisualTestContext, snap: Snapshot) {
    view.update(cx, |view, cx| view.observe(snap, cx));
    cx.run_until_parked();
}

fn inputs(calls: Vec<Call>) -> Vec<ScreenInput> {
    calls
        .into_iter()
        .filter_map(|call| match call {
            Call::Input(STREAM, input) if !matches!(input, ScreenInput::Gestures { .. }) => {
                Some(input)
            }
            _ => None,
        })
        .collect()
}

/// A Mac whose grants are off names each pane off and the Mac, with a "Turn on" button for
/// each; the button opens that pane's guide, and the note says where it opened.
#[gpui::test]
fn a_refusal_names_the_pane_and_the_mac(cx: &mut TestAppContext) {
    let (view, _recorder, cx) = view(cx, &TestLayer::default());
    observe(
        &view,
        cx,
        Snapshot {
            stage: Stage::NotPermitted(vec![Pane::ScreenRecording, Pane::Accessibility]),
            host: "studio".into(),
            ..Snapshot::default()
        },
    );
    assert_eq!(
        missing_line(Pane::ScreenRecording, "studio"),
        "Screen Recording is off on studio"
    );
    assert!(cx.debug_bounds("desktop-turn-on-Accessibility").is_some());
    let button = cx
        .debug_bounds("desktop-turn-on-ScreenRecording")
        .expect("a Turn on button");
    cx.simulate_click(button.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, _| view.guide),
        Some(Pane::ScreenRecording)
    );
    assert!(guide_note(Pane::ScreenRecording, "studio", false).contains("open on studio"));
    // The grants come on: the session is welcomed and the panel goes.
    observe(&view, cx, joined(true));
    assert_eq!(view.read_with(cx, |view, _| view.guide), None);
}

/// Welcomed, the view opens a display made at its window's content size in pixels, at its
/// backing scale; picking a physical one closes the one shown and opens it at its refresh.
#[gpui::test]
fn a_welcome_opens_a_display_the_windows_size(cx: &mut TestAppContext) {
    let (view, recorder, cx) = view(cx, &TestLayer::default());
    let mut snap = joined(false);
    snap.displays = vec![display(1, 120.0), display(2, 60.0)];
    snap.stream = None;
    observe(&view, cx, snap.clone());
    let shape = shape_of(&view, cx);
    let (points, scale, zoom) = cx.update(|window, _| {
        (
            window.viewport_size(),
            window.scale_factor(),
            window.content_zoom(),
        )
    });
    assert!((f64::from(shape.width) - f64::from(f32::from(points.width) * scale)).abs() < 1.0);
    assert!((f64::from(shape.height) - f64::from(f32::from(points.height) * scale)).abs() < 1.0);
    assert!((shape.scale - scale / zoom).abs() < f32::EPSILON);
    assert_eq!(
        recorder.take(),
        [Call::Open(
            Open::Made { key: KEY, shape },
            made_quality(shape).fps
        )]
    );
    observe(&view, cx, snap.clone());
    assert!(recorder.take().is_empty(), "asked once");
    snap.stream = joined_made(false).stream;
    observe(&view, cx, snap);
    assert!(recorder.take().is_empty(), "made at its size already");
    view.update(cx, |view, cx| {
        view.choose_display(Choice::Display(DisplayId(2)), cx);
    });
    assert_eq!(
        recorder.take(),
        [
            Call::Close(STREAM),
            Call::Open(Open::Display(DisplayId(2)), 60)
        ]
    );
}

/// Drops and redials open nothing more: the session opens again what it had on each welcome,
/// so the view asks once per display however often it reconnects.
#[gpui::test]
fn a_reconnect_opens_each_display_once(cx: &mut TestAppContext) {
    let (view, recorder, cx) = view(cx, &TestLayer::default());
    let mut welcomed = joined_made(true);
    welcomed.stream = None;
    let showing = joined_made(true);
    let dropped = Snapshot {
        stage: Stage::Reconnecting(Duration::from_secs(1)),
        host: "studio".into(),
        ..Snapshot::default()
    };
    observe(&view, cx, welcomed.clone());
    observe(&view, cx, showing.clone());
    assert_eq!(opens(&recorder.take()).len(), 1);
    for round in 0..3 {
        observe(&view, cx, dropped.clone());
        observe(&view, cx, Snapshot::default());
        observe(&view, cx, welcomed.clone());
        observe(&view, cx, showing.clone());
        let calls = recorder.take();
        assert!(opens(&calls).is_empty(), "redial {round}: {calls:?}");
    }
}

/// A window resized asks for its display at the size it ends on, once it holds still for
/// [`RESIZE_SETTLE`] on the executor's clock.
#[gpui::test]
fn a_resized_window_resizes_its_display_once_it_settles(cx: &mut TestAppContext) {
    let (view, recorder, cx) = view(cx, &TestLayer::default());
    let mut snap = joined_made(true);
    snap.stream = None;
    observe(&view, cx, snap);
    observe(&view, cx, joined_made(true));
    recorder.take();
    cx.simulate_resize(size(px(900.0), px(600.0)));
    cx.run_until_parked();
    cx.executor().advance_clock(RESIZE_SETTLE / 2);
    cx.simulate_resize(size(px(1000.0), px(700.0)));
    cx.run_until_parked();
    cx.executor()
        .advance_clock(RESIZE_SETTLE.saturating_sub(Duration::from_millis(1)));
    cx.run_until_parked();
    assert!(recorder.take().is_empty(), "still moving");
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    let shape = shape_of(&view, cx);
    assert_eq!((shape.width, shape.height), (2000, 1400));
    assert_eq!(recorder.take(), [Call::Resize(STREAM, shape)]);
}

/// A Mac that cannot make a display the window's size streams its main display, and the view
/// says so.
#[gpui::test]
fn a_display_the_mac_cannot_make_falls_back_to_its_main_one(cx: &mut TestAppContext) {
    let (view, recorder, cx) = view(cx, &TestLayer::default());
    let mut snap = joined(true);
    snap.displays = vec![display(1, 120.0), display(2, 60.0)];
    snap.stream = None;
    observe(&view, cx, snap.clone());
    recorder.take();
    snap.failure = Some((
        OpenAsk::Made(KEY),
        ScreenFailure::Failed("no virtual displays".into()),
    ));
    observe(&view, cx, snap.clone());
    assert_eq!(
        recorder.take(),
        [Call::Open(Open::Display(DisplayId(1)), 120)]
    );
    snap.failure = None;
    snap.stream = joined(true).stream;
    observe(&view, cx, snap);
    assert!(
        !recorder
            .take()
            .iter()
            .any(|call| matches!(call, Call::Open(..))),
        "the fallback is not asked for again once it streams"
    );
    assert!(cx.debug_bounds("desktop-made-fallback").is_some());
}

/// A physical display the Mac refused to open says why, with a way to ask again.
#[gpui::test]
fn a_refused_open_says_why_and_asks_again(cx: &mut TestAppContext) {
    let (view, recorder, cx) = view(cx, &TestLayer::default());
    let mut snap = joined(true);
    snap.stream = None;
    observe(&view, cx, snap.clone());
    view.update(cx, |view, cx| {
        view.choose_display(Choice::Display(DisplayId(1)), cx);
    });
    recorder.take();
    snap.failure = Some((
        OpenAsk::Target(CaptureTarget::Display(DisplayId(1))),
        ScreenFailure::Gone,
    ));
    observe(&view, cx, snap);
    let again = cx
        .debug_bounds("desktop-reopen")
        .expect("a Try again button");
    cx.simulate_click(again.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        recorder.take(),
        [Call::Open(Open::Display(DisplayId(1)), 120)]
    );
}

/// A locked Mac's picture gives way to a notice that it is locked.
#[gpui::test]
fn a_locked_mac_says_so(cx: &mut TestAppContext) {
    let (view, _recorder, cx) = view(cx, &TestLayer::default());
    let mut snap = joined(true);
    observe(&view, cx, snap.clone());
    view.update(cx, |view, _| {
        let glass = &view.screen.as_ref().expect("the stream shown").glass;
        let buffer = test_buffer(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, 64, 48);
        glass.offer(Picture::new(buffer), test_stamp(0));
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("desktop-notice").is_none(), "the picture");
    if let Some(stream) = snap.stream.as_mut() {
        stream.source = SourceState::Locked;
    }
    observe(&view, cx, snap);
    assert!(cx.debug_bounds("desktop-notice").is_some(), "locked");
}

/// Refused input and a refused curtain are said under the controls.
#[gpui::test]
fn refused_input_and_curtain_are_said(cx: &mut TestAppContext) {
    let (view, _recorder, cx) = view(cx, &TestLayer::default());
    let mut snap = joined(false);
    snap.input_refused = true;
    snap.curtain_refused = Some("another device holds the controls".into());
    observe(&view, cx, snap);
    assert!(cx.debug_bounds("desktop-input-refused").is_some());
    assert!(cx.debug_bounds("desktop-curtain-refused").is_some());
}

/// While the window is key, has the keyboard and holds the controls, the system's shortcuts go
/// to the Mac; losing the controls gives them back.
#[gpui::test]
fn the_systems_shortcuts_go_to_the_mac_while_holding_the_controls(cx: &mut TestAppContext) {
    let grab = Rc::new(Grab::default());
    let (view, recorder, cx) = view_grabbing(cx, &TestLayer::default(), grab.clone());
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    view.update(cx, |view, cx| view.focus_changed(true, cx));
    observe(&view, cx, joined(true));
    recorder.take();
    let cmd_tab = NativeKey {
        vk: 0x30,
        kind: KeyKind::Down,
        mods: Mods::SUPER,
    };
    assert!(grab.offer(cmd_tab), "⌘Tab is taken");
    cx.run_until_parked();
    assert_eq!(
        inputs(recorder.take()),
        [ScreenInput::Key {
            code: KeyCode::Tab,
            action: KeyAction::Press,
            mods: Mods::SUPER,
        }]
    );
    observe(&view, cx, joined(false));
    assert!(
        view.read_with(cx, |view, _| view.grab.is_none()),
        "given back"
    );
}

/// A Mac that has not trusted this app for Accessibility keeps its shortcuts, and the view says
/// so with a way to turn it on.
#[gpui::test]
fn shortcuts_kept_here_say_why(cx: &mut TestAppContext) {
    let grab = Rc::new(Grab {
        refuse: Some(Refused::NeedsAccessibility),
        ..Grab::default()
    });
    let (view, _recorder, cx) = view_grabbing(cx, &TestLayer::default(), grab);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    view.update(cx, |view, cx| view.focus_changed(true, cx));
    observe(&view, cx, joined(true));
    assert!(cx.debug_bounds("desktop-shortcuts-refused").is_some());
    observe(&view, cx, joined(false));
    assert!(cx.debug_bounds("desktop-shortcuts-refused").is_none());
}

/// Another device holds the controls: the badge offers them, and taking them asks the
/// session.
#[gpui::test]
fn the_seat_badge_takes_control(cx: &mut TestAppContext) {
    let (view, recorder, cx) = view(cx, &TestLayer::default());
    observe(&view, cx, joined(false));
    recorder.take();
    let button = cx
        .debug_bounds("desktop-take-control")
        .expect("Take control while another device has it");
    cx.simulate_click(button.center(), Modifiers::default());
    assert!(recorder.take().contains(&Call::Control(true)));
    observe(&view, cx, joined(true));
    assert!(
        cx.debug_bounds("desktop-take-control").is_none(),
        "held here: nothing to take"
    );
}

/// Input goes only from the device holding the controls; a click on the picture lands on the
/// stream pixel under it, letterboxed; losing the keyboard lets go of what is held.
#[gpui::test]
fn input_goes_with_the_controls_to_the_pixel_under_it(cx: &mut TestAppContext) {
    let (view, recorder, cx) = view(cx, &TestLayer::default());
    observe(&view, cx, joined(false));
    cx.simulate_keystrokes("a");
    assert!(inputs(recorder.take()).is_empty(), "not the controller");

    observe(&view, cx, joined(true));
    // A simulated keystroke is the key going down only; its release is its own event.
    cx.simulate_keystrokes("a");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("a").unwrap(),
    });
    assert_eq!(
        inputs(recorder.take()),
        [
            ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Press,
                mods: Mods::empty(),
            },
            ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Release,
                mods: Mods::empty(),
            },
        ]
    );

    let stage = cx.debug_bounds("desktop-stage").expect("the stage");
    let placed = Placement::new(stage, (200, 200));
    let picture = placed.picture();
    let at = picture.origin + gpui::point(px(5.0), picture.size.height * 0.75);
    let pixel = placed.inside(at).expect("on the picture");
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
    let sent = inputs(recorder.take());
    let Some(ScreenInput::Button {
        button: ProtoButton::Left,
        down: true,
        x,
        y,
        clicks: 1,
        ..
    }) = sent.last()
    else {
        panic!("a click: {sent:?}");
    };
    assert!((x - pixel.0).abs() < 0.01 && (y - pixel.1).abs() < 0.01);

    // The window going to the back takes the keyboard with it.
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    cx.deactivate_window();
    cx.run_until_parked();
    let sent = inputs(recorder.take());
    assert!(
        sent.iter()
            .any(|input| matches!(input, ScreenInput::Button { down: false, .. })),
        "the button let go when the window lost the keyboard: {sent:?}"
    );
}

/// A decoded picture goes to the test layer once the view placed one for it, and the waiting
/// notice gives way to it; the view's end shuts the session.
#[gpui::test]
fn a_picture_goes_up_on_the_layer(cx: &mut TestAppContext) {
    let layer = TestLayer::default();
    let (view, recorder, cx) = view(cx, &layer);
    observe(&view, cx, joined(true));
    assert!(
        cx.debug_bounds("desktop-notice").is_some(),
        "waiting for a picture"
    );
    view.update(cx, |view, _| {
        let glass = &view.screen.as_ref().expect("the stream shown").glass;
        let buffer = test_buffer(kCVPixelFormatType_420YpCbCr8BiPlanarFullRange, 64, 48);
        glass.offer(Picture::new(buffer), test_stamp(0));
    });
    cx.run_until_parked();
    assert_eq!(*layer.took.lock(), [(64, 48)]);
    assert!(cx.debug_bounds("desktop-notice").is_none());
    recorder.take();
    view.update(cx, |view, _| view.shutdown());
    assert!(recorder.take().contains(&Call::Shutdown));
}
