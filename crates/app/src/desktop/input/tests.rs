//! The keyboard's tests are slopty-ui's `screen/keyboard/tests.rs`, on plain state: the key
//! event being dispatched is handed in as `native`, as `keys::System` reads it off AppKit.

use gpui::{Keystroke, Modifiers, TouchPhase, bounds, point, px, size};
use onicode_desktop_proto::input::{KeyAction, KeyCode, Mods, MouseButton as ProtoButton};
use onicode_desktop_proto::screen::{MediaKey, ScreenInput, ScrollPhase, SwipeDirection};
use proptest::prelude::*;

use super::*;
use crate::desktop::keys::{KeyKind, NativeKey};

fn key(code: KeyCode, action: KeyAction, mods: Mods) -> ScreenInput {
    ScreenInput::Key {
        code,
        action,
        mods,
        chord: None,
    }
}

fn stroke(key: &str, modifiers: Modifiers) -> Keystroke {
    Keystroke {
        modifiers,
        key: key.to_owned(),
        key_char: None,
    }
}

fn native(vk: u16, kind: KeyKind, mods: Mods) -> Option<NativeKey> {
    Some(NativeKey { vk, kind, mods })
}

const CMD: Modifiers = Modifiers {
    control: false,
    alt: false,
    shift: false,
    platform: true,
    function: false,
};

/// A key goes by its place, not its character: AZERTY's A sits at the US Q, and ⌘ there is
/// ⌘A by its character (the chord names it), which the Mac presses where its own layout
/// types it. Before, the character "a" at the US A was read as ⌘Q on an AZERTY Mac: the app
/// quit. Without ⌘ the key goes as its place, naming nothing.
#[test]
fn a_key_goes_by_position_and_a_chord_by_its_character() {
    let mut input = Input::default();
    assert!(input.key_pressed(
        native(0x0c, KeyKind::Down, Mods::empty()),
        &stroke("a", Modifiers::default()),
        false
    ));
    assert!(input.key_released(
        native(0x0c, KeyKind::Up, Mods::empty()),
        &stroke("a", Modifiers::default())
    ));
    assert!(input.key_pressed(
        native(0x0c, KeyKind::Down, Mods::SUPER),
        &stroke("a", CMD),
        false
    ));
    assert!(input.key_released(native(0x0c, KeyKind::Up, Mods::SUPER), &stroke("a", CMD)));
    // An arrow names no character: its place.
    assert!(input.key_pressed(
        native(0x7b, KeyKind::Down, Mods::SUPER),
        &stroke("left", CMD),
        false
    ));
    assert_eq!(
        input.take(),
        [
            key(KeyCode::Q, KeyAction::Press, Mods::empty()),
            key(KeyCode::Q, KeyAction::Release, Mods::empty()),
            ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Press,
                mods: Mods::SUPER,
                chord: Some("a".to_owned()),
            },
            key(KeyCode::A, KeyAction::Release, Mods::SUPER),
            key(KeyCode::ArrowLeft, KeyAction::Press, Mods::SUPER),
        ]
    );
}

/// ⌘⌥⎋ and ⌃⌘Q stay on this Mac; a key no keyboard has is not sent; a release whose press
/// never went is dropped.
#[test]
fn the_ways_out_stay_here() {
    let mut input = Input::default();
    let force = Modifiers {
        platform: true,
        alt: true,
        ..Modifiers::default()
    };
    assert!(!input.key_pressed(None, &stroke("escape", force), false));
    let lock = Modifiers {
        platform: true,
        control: true,
        ..Modifiers::default()
    };
    assert!(!input.key_pressed(None, &stroke("q", lock), false));
    assert!(!input.key_pressed(None, &stroke("é", Modifiers::default()), false));
    assert!(!input.key_released(None, &stroke("x", Modifiers::default())));
    assert!(input.take().is_empty());
}

/// A repeat goes as a repeat; the event read off AppKit counts only when it is the same kind
/// of event, so a stale up does not name a press.
#[test]
fn a_held_key_repeats_and_lets_go_once() {
    let mut input = Input::default();
    let plain = Modifiers::default();
    assert!(input.key_pressed(
        native(0x00, KeyKind::Down, Mods::empty()),
        &stroke("a", plain),
        false
    ));
    assert!(input.key_pressed(
        native(0x00, KeyKind::Repeat, Mods::empty()),
        &stroke("a", plain),
        true
    ));
    // The event being dispatched is an up: not this press's place; the character's is.
    assert!(input.key_pressed(
        native(0x01, KeyKind::Up, Mods::empty()),
        &stroke("d", plain),
        false
    ));
    assert!(input.key_released(
        native(0x00, KeyKind::Up, Mods::empty()),
        &stroke("a", plain)
    ));
    assert!(!input.key_released(
        native(0x00, KeyKind::Up, Mods::empty()),
        &stroke("a", plain)
    ));
    assert_eq!(
        input.take(),
        [
            key(KeyCode::A, KeyAction::Press, Mods::empty()),
            key(KeyCode::A, KeyAction::Repeat, Mods::empty()),
            key(KeyCode::D, KeyAction::Press, Mods::empty()),
            key(KeyCode::A, KeyAction::Release, Mods::empty()),
        ]
    );
}

/// A right-hand modifier goes as its own key with its side, down and up.
#[test]
fn right_modifiers_keep_their_side() {
    let mut input = Input::default();
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let right = Mods::SHIFT | Mods::SHIFT_RIGHT;
    input.modifiers_moved(native(0x3c, KeyKind::Flags, right), shift, false);
    input.modifiers_moved(
        native(0x3c, KeyKind::Flags, Mods::empty()),
        Modifiers::default(),
        false,
    );
    assert_eq!(
        input.take(),
        [
            ScreenInput::Lock { caps: false },
            key(KeyCode::ShiftRight, KeyAction::Press, right),
            key(KeyCode::ShiftRight, KeyAction::Release, Mods::empty()),
        ]
    );
}

/// Caps Lock goes as its state when the view takes the keyboard and whenever it changes,
/// never as a key, even when the event being handled is its key.
#[test]
fn caps_lock_state_follows_focus_and_changes() {
    let mut input = Input::default();
    input.focused(Some(true));
    assert_eq!(input.take(), [ScreenInput::Lock { caps: true }], "on focus");
    input.modifiers_moved(
        native(0x39, KeyKind::Flags, Mods::empty()),
        Modifiers::default(),
        false,
    );
    assert_eq!(input.take(), [ScreenInput::Lock { caps: false }], "no key");
    input.modifiers_moved(None, Modifiers::default(), false);
    assert!(input.take().is_empty(), "unchanged: nothing");
}

/// With both ⇧ keys down, GPUI never reports the left one going up (the modifiers stay as
/// they were), so the release of the right one lets go of the left too.
#[test]
fn overlapping_modifiers_are_all_let_go() {
    let mut input = Input::default();
    input.focused(Some(false));
    input.take();
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    input.modifiers_moved(native(0x38, KeyKind::Flags, Mods::SHIFT), shift, false);
    input.modifiers_moved(
        native(0x3c, KeyKind::Flags, Mods::empty()),
        Modifiers::default(),
        false,
    );
    assert_eq!(
        input.take(),
        [
            key(KeyCode::ShiftLeft, KeyAction::Press, Mods::SHIFT),
            key(KeyCode::ShiftLeft, KeyAction::Release, Mods::empty()),
        ]
    );
}

/// With no place to read, the modifiers that moved go as their left keys.
#[test]
fn modifiers_without_a_place_go_as_the_left_keys() {
    let mut input = Input::default();
    input.focused(Some(false));
    input.take();
    let both = Modifiers {
        platform: true,
        alt: true,
        ..Modifiers::default()
    };
    input.modifiers_moved(None, both, false);
    input.modifiers_moved(None, Modifiers::default(), false);
    assert_eq!(
        input.take(),
        [
            key(KeyCode::AltLeft, KeyAction::Press, Mods::ALT | Mods::SUPER),
            key(KeyCode::MetaLeft, KeyAction::Press, Mods::ALT | Mods::SUPER),
            key(KeyCode::AltLeft, KeyAction::Release, Mods::empty()),
            key(KeyCode::MetaLeft, KeyAction::Release, Mods::empty()),
        ]
    );
    assert_eq!(
        keys::modifier_keys(Modifiers::default(), CMD),
        [(KeyCode::MetaLeft, KeyAction::Press)]
    );
}

/// A keypad key goes as the keypad's own key, not the main row's, whether it is read by its
/// place or named `kp…`; ⌘ on it still goes by its place.
#[test]
fn keypad_keys_keep_their_place() {
    let mut input = Input::default();
    let plain = Modifiers::default();
    assert!(input.key_pressed(
        native(0x57, KeyKind::Down, Mods::empty()),
        &stroke("5", plain),
        false
    ));
    assert!(input.key_pressed(None, &stroke("kpenter", plain), false));
    assert!(input.key_pressed(
        native(0x45, KeyKind::Down, Mods::SUPER),
        &stroke("+", CMD),
        false
    ));
    assert_eq!(
        input.take(),
        [
            key(KeyCode::Numpad5, KeyAction::Press, Mods::empty()),
            key(KeyCode::NumpadEnter, KeyAction::Press, Mods::empty()),
            key(KeyCode::NumpadAdd, KeyAction::Press, Mods::SUPER),
        ]
    );
}

/// Media keys go as media keys, down and up.
#[test]
fn media_keys_go_to_the_mac() {
    assert_eq!(
        keys::media_key(KeyCode::MediaPlayPause),
        Some(MediaKey::PlayPause)
    );
    assert_eq!(keys::media_key(KeyCode::A), None);
}

/// The names GPUI gives keys, and characters, map to the keys a US keyboard has them on.
#[test]
fn names_map_to_keys() {
    for (name, code) in [
        ("a", KeyCode::A),
        ("Z", KeyCode::Z),
        ("1", KeyCode::Digit1),
        ("`", KeyCode::Backquote),
        ("enter", KeyCode::Enter),
        ("pagedown", KeyCode::PageDown),
        ("f20", KeyCode::F20),
        ("kpdecimal", KeyCode::NumpadDecimal),
        ("é", KeyCode::Unidentified),
        ("hyper", KeyCode::Unidentified),
    ] {
        assert_eq!(keys::key_code(name), code, "{name}");
    }
    assert_eq!(keys::chord_char("Z").as_deref(), Some("z"));
    assert_eq!(keys::chord_char("left"), None);
    assert_eq!(keys::mods(CMD), Mods::SUPER);
}

/// Losing the keyboard lets go of every button, then the plain keys, then the modifiers, and
/// closes a coast still open; nothing is left down on the Mac, and a second let-go sends
/// nothing.
#[test]
fn letting_go_releases_everything_held() {
    let mut input = Input::default();
    input.button_down(ProtoButton::Left, (5.0, 6.0), 1, Mods::empty());
    input.modifiers_moved(native(0x37, KeyKind::Flags, Mods::SUPER), CMD, false);
    assert!(input.key_pressed(
        native(0x01, KeyKind::Down, Mods::SUPER),
        &stroke("s", CMD),
        false
    ));
    input.scroll(Scroll {
        dx: 0.0,
        dy: -3.0,
        precise: true,
        touch: TouchPhase::Ended,
        momentum: Some(TouchPhase::Started),
        at: (5.0, 6.0),
        mods: Mods::empty(),
        time_us: 1,
    });
    input.take();
    input.let_go(2);
    let sent = input.take();
    assert_eq!(
        sent,
        [
            ScreenInput::Button {
                button: ProtoButton::Left,
                down: false,
                x: 5.0,
                y: 6.0,
                clicks: 1,
                mods: Mods::empty(),
            },
            key(KeyCode::S, KeyAction::Release, Mods::empty()),
            key(KeyCode::MetaLeft, KeyAction::Release, Mods::empty()),
            ScreenInput::Scroll {
                dx: 0.0,
                dy: 0.0,
                precise: true,
                phase: ScrollPhase::None,
                momentum: ScrollPhase::Ended,
                x: 5.0,
                y: 6.0,
                mods: Mods::empty(),
                time_us: 2,
            },
        ]
    );
    input.let_go(3);
    assert!(input.take().is_empty());
}

/// A button goes down with its click count and up with it; an up for a button never pressed
/// over the picture is not sent.
#[test]
fn buttons_carry_their_clicks() {
    let mut input = Input::default();
    assert!(!input.button_up(ProtoButton::Right, (1.0, 1.0), 1, Mods::empty()));
    input.button_down(ProtoButton::Left, (1.0, 2.0), 2, Mods::SHIFT);
    assert!(input.dragging());
    assert!(input.button_up(ProtoButton::Left, (3.0, 4.0), 2, Mods::SHIFT));
    assert_eq!(
        input.take(),
        [
            ScreenInput::Button {
                button: ProtoButton::Left,
                down: true,
                x: 1.0,
                y: 2.0,
                clicks: 2,
                mods: Mods::SHIFT,
            },
            ScreenInput::Button {
                button: ProtoButton::Left,
                down: false,
                x: 3.0,
                y: 4.0,
                clicks: 2,
                mods: Mods::SHIFT,
            },
        ]
    );
    assert_eq!(
        proto_button(gpui::MouseButton::Navigate(gpui::NavigationDirection::Back)),
        ProtoButton::Back
    );
}

fn scroll(touch: TouchPhase, momentum: Option<TouchPhase>) -> Scroll {
    Scroll {
        dx: 0.0,
        dy: 1.0,
        precise: true,
        touch,
        momentum,
        at: (0.0, 0.0),
        mods: Mods::empty(),
        time_us: 0,
    }
}

fn phases(sent: &[ScreenInput]) -> Vec<(ScrollPhase, ScrollPhase)> {
    sent.iter()
        .filter_map(|input| match input {
            ScreenInput::Scroll {
                phase, momentum, ..
            } => Some((*phase, *momentum)),
            _ => None,
        })
        .collect()
}

/// A trackpad's gesture opens once (a resting finger's second `Started` is the same gesture),
/// changes, ends, and its coast goes as momentum; a finger landing on a coast closes it first.
/// A wheel's notch has no phases.
#[test]
fn a_scroll_carries_its_gesture_and_momentum() {
    use ScrollPhase as P;
    let mut input = Input::default();
    input.scroll(scroll(TouchPhase::Started, None));
    input.scroll(scroll(TouchPhase::Started, None));
    input.scroll(scroll(TouchPhase::Moved, None));
    input.scroll(scroll(TouchPhase::Ended, None));
    input.scroll(scroll(TouchPhase::Ended, Some(TouchPhase::Started)));
    input.scroll(scroll(TouchPhase::Ended, Some(TouchPhase::Moved)));
    assert!(input.coasting());
    input.scroll(scroll(TouchPhase::Started, None));
    assert!(!input.coasting());
    let mut wheel = scroll(TouchPhase::Moved, None);
    wheel.precise = false;
    input.scroll(wheel);
    assert_eq!(
        phases(&input.take()),
        [
            (P::Began, P::None),
            (P::Changed, P::None),
            (P::Changed, P::None),
            (P::Ended, P::None),
            (P::None, P::Began),
            (P::None, P::Changed),
            (P::None, P::Ended),
            (P::Began, P::None),
            (P::None, P::None),
        ]
    );
    assert!(
        !input.end_momentum((0.0, 0.0), Mods::empty(), 0),
        "nothing coasting"
    );
}

/// A pinch that starts beside the picture stays here; one that starts on it goes to the Mac to
/// its end, wherever it drifts. Rotation, smart zoom and swipes go as themselves.
#[test]
fn gestures_go_as_themselves() {
    let mut input = Input::default();
    assert!(!input.pinch(0.1, TouchPhase::Started, None, (0.0, 0.0), 0));
    assert!(!input.pinch(0.1, TouchPhase::Moved, Some((1.0, 1.0)), (1.0, 1.0), 0));
    assert!(input.pinch(0.1, TouchPhase::Started, Some((1.0, 1.0)), (1.0, 1.0), 0));
    assert!(input.pinch(0.2, TouchPhase::Moved, None, (9.0, 9.0), 1));
    assert!(input.pinch(0.0, TouchPhase::Ended, None, (9.0, 9.0), 2));
    assert!(!input.pinch(0.1, TouchPhase::Moved, Some((1.0, 1.0)), (1.0, 1.0), 3));
    input.rotate(5.0, TouchPhase::Moved, (1.0, 2.0), 4);
    input.smart_magnify((1.0, 2.0));
    assert!(input.swipe(1.0, 0.0, (1.0, 2.0)));
    assert!(!input.swipe(0.0, 0.0, (1.0, 2.0)));
    let sent = input.take();
    assert_eq!(sent.len(), 6);
    assert_eq!(
        sent.get(1),
        Some(&ScreenInput::Magnify {
            delta: 0.2,
            phase: ScrollPhase::Changed,
            x: 9.0,
            y: 9.0,
            time_us: 1,
        })
    );
    assert_eq!(
        sent.last(),
        Some(&ScreenInput::Swipe {
            direction: SwipeDirection::Left,
            x: 1.0,
            y: 2.0,
        })
    );
    assert_eq!(swipe_direction(-1.0, 0.0), Some(SwipeDirection::Right));
    assert_eq!(swipe_direction(0.0, 1.0), Some(SwipeDirection::Up));
    assert_eq!(swipe_direction(0.0, -1.0), Some(SwipeDirection::Down));
}

/// A square stream in a wide view sits in the middle, letterboxed: a point on it maps to its
/// pixel, a point beside it to none, and to the edge's pixel when a drag drifts there.
#[test]
fn the_picture_letterboxes_and_maps_points_to_pixels() {
    let view = bounds(point(px(10.0), px(20.0)), size(px(200.0), px(100.0)));
    let placed = Placement::new(view, (200, 200));
    assert_eq!(
        placed.picture(),
        bounds(point(px(60.0), px(20.0)), size(px(100.0), px(100.0)))
    );
    let on = placed
        .inside(point(px(70.0), px(30.0)))
        .expect("on the picture");
    assert!(
        (on.0 - 20.0).abs() < 0.01 && (on.1 - 20.0).abs() < 0.01,
        "{on:?}"
    );
    assert_eq!(
        placed.inside(point(px(20.0), px(30.0))),
        None,
        "in the bar beside it"
    );
    let edge = placed.edge(point(px(20.0), px(30.0)));
    assert!(
        edge.0.abs() < 0.01 && (edge.1 - 20.0).abs() < 0.01,
        "{edge:?}"
    );
    let back = placed.to_window(20.0, 20.0);
    assert!((f32::from(back.x) - 70.0).abs() < 0.01);
}

proptest! {
    /// Every point of the view maps to a pixel of the stream at its edge or inside it, and a
    /// point on the picture maps there and back to itself.
    #[test]
    fn mapping_stays_on_the_stream_and_round_trips(
        vw in 50.0_f32..2000.0,
        vh in 50.0_f32..2000.0,
        sw in 16_u32..4000,
        sh in 16_u32..4000,
        fx in 0.0_f32..1.0,
        fy in 0.0_f32..1.0,
    ) {
        let view = bounds(point(px(0.0), px(0.0)), size(px(vw), px(vh)));
        let placed = Placement::new(view, (sw, sh));
        #[expect(clippy::cast_precision_loss, reason = "test sizes")]
        let (w, h) = (sw as f32, sh as f32);
        let at = point(px(fx * vw), px(fy * vh));
        let (x, y) = placed.edge(at);
        prop_assert!((-0.01..=w + 0.01).contains(&x) && (-0.01..=h + 0.01).contains(&y));
        if let Some((x, y)) = placed.inside(at) {
            let back = placed.to_window(x, y);
            prop_assert!((f32::from(back.x) - fx * vw).abs() < 0.5);
            prop_assert!((f32::from(back.y) - fy * vh).abs() < 0.5);
        }
    }
}
