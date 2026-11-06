//! The view in a test window: a recording link stands in for the session, the inert keyboard
//! for AppKit's, and a test layer for the `VideoLayer`. No real window opens, nothing takes
//! focus, nothing plays.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext};
use objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
use onicode_desktop_link::{Pane, Seat};
use onicode_desktop_proto::input::{KeyAction, KeyCode, Mods, MouseButton as ProtoButton};
use onicode_desktop_proto::screen::{DisplayInfo, Quality, ScreenInput};
use onicode_desktop_proto::{DisplayId, StreamId};
use tokio::sync::watch;

use super::glass::tests::{TestLayer, test_buffer, test_stamp};
use super::glass::{Layer, Picture};
use super::input::Placement;
use super::*;

/// What the view asked of the session.
#[derive(Clone, PartialEq, Debug)]
enum Call {
    Open(DisplayId, u16),
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

    fn open(&self, display: DisplayId, quality: Quality) {
        self.push(Call::Open(display, quality.fps));
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
    cx.update(|cx| cx.set_global(Theme::default()));
    let recorder = Rc::new(Recorder::default());
    let link: Rc<dyn Link> = recorder.clone();
    let layer = layer.clone();
    let make: MakeLayer =
        Rc::new(move |_host, _reported| Ok(Box::new(layer.clone()) as Box<dyn Layer>));
    let (view, cx) = cx.add_window_view(|window, cx| {
        let target = Target {
            device_id: "studio-id".into(),
            name: "studio".into(),
            local: false,
        };
        DesktopView::linked_for_test(target, link, Rc::new(keys::Inert), make, window, cx)
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
        }),
        ..Snapshot::default()
    }
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

/// Welcomed, the view opens the first display at its refresh; picking another closes the one
/// shown and opens it.
#[gpui::test]
fn a_welcome_opens_the_first_display(cx: &mut TestAppContext) {
    let (view, recorder, cx) = view(cx, &TestLayer::default());
    let mut snap = joined(false);
    snap.displays = vec![display(1, 120.0), display(2, 60.0)];
    snap.stream = None;
    observe(&view, cx, snap.clone());
    assert_eq!(recorder.take(), [Call::Open(DisplayId(1), 120)]);
    observe(&view, cx, snap.clone());
    assert!(recorder.take().is_empty(), "asked once");
    snap.stream = joined(false).stream;
    observe(&view, cx, snap);
    recorder.take();
    view.update(cx, |view, cx| view.choose_display(DisplayId(2), cx));
    assert_eq!(
        recorder.take(),
        [Call::Close(STREAM), Call::Open(DisplayId(2), 60)]
    );
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
                chord: None,
            },
            ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Release,
                mods: Mods::empty(),
                chord: None,
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
