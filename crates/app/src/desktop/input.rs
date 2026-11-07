//! What the person does over the picture, as the Mac's input: keys by position, modifiers on
//! their own, buttons with their click counts, scrolls with their gesture and momentum phases,
//! and the trackpad's pinch, rotation, smart zoom and swipe (slopty-ui's `screen.rs` and
//! `screen/keyboard.rs`).
//!
//! Plain state, no GPUI context: the view reads each event's position through [`Placement`]
//! and the key's place through `keys::KeyPlatform`, and sends what [`Input::take`] hands back.
//! Every press is remembered until its release has gone, so [`Input::let_go`] can release on
//! the Mac whatever the person still held when the view lost the keyboard: input stuck down on
//! the Mac is the one thing a remote desktop must never leave behind.

use std::time::Duration;

use gpui::{Bounds, Keystroke, Modifiers, Pixels, Point, TouchPhase, point, px, size};
use onicode_desktop_client::fit::{Fit, Rect, pixels};
use onicode_desktop_proto::input::{KeyAction, KeyCode, Mods, MouseButton as ProtoButton};
use onicode_desktop_proto::screen::{ScreenInput, ScrollPhase, SwipeDirection};

use super::keys::{self, KeyKind, NativeKey};

/// How long a fling coasts without an event before its close counts as lost. macOS closes its
/// momentum itself (`ScrollEvent::momentum_phase`); this is the backstop for a close that never
/// came. Momentum events arrive about a frame apart, so this is several frames of silence.
pub(crate) const MOMENTUM_GAP: Duration = Duration::from_millis(120);

/// Where the picture sits in the view, and the way between the view's points and the stream's
/// pixels (`onicode_desktop_client::fit::Fit`, letterboxed).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placement {
    fit: Fit,
    origin: Point<Pixels>,
}

impl Placement {
    /// The picture of a stream `stream` pixels in size, in a view at `view`.
    #[must_use]
    pub(crate) fn new(view: Bounds<Pixels>, stream: (u32, u32)) -> Self {
        // A view or a stream with no area has no picture: an empty rectangle, under which no
        // point is the stream's.
        let fit = Fit::new(
            (f32::from(view.size.width), f32::from(view.size.height)),
            stream,
        )
        .unwrap_or(Fit {
            rect: Rect::default(),
            stream: (pixels(stream.0), pixels(stream.1)),
            scale: 1.0,
        });
        Self {
            fit,
            origin: view.origin,
        }
    }

    /// Where the picture is drawn, in window points.
    #[must_use]
    pub(crate) fn picture(&self) -> Bounds<Pixels> {
        let Rect {
            x,
            y,
            width: w,
            height: h,
        } = self.fit.rect;
        Bounds {
            origin: self.origin + point(px(x), px(y)),
            size: size(px(w), px(h)),
        }
    }

    /// The stream pixel under `at`, a window point; `None` beside the picture.
    #[must_use]
    pub(crate) fn inside(&self, at: Point<Pixels>) -> Option<(f32, f32)> {
        let local = at - self.origin;
        self.fit.to_stream(f32::from(local.x), f32::from(local.y))
    }

    /// The stream pixel nearest `at`: beside the picture, the one at its edge, so a drag or a
    /// scroll that drifts off the picture ends where it began.
    #[must_use]
    pub(crate) fn edge(&self, at: Point<Pixels>) -> (f32, f32) {
        let Rect {
            x,
            y,
            width: w,
            height: h,
        } = self.fit.rect;
        let local = at - self.origin;
        // A hair inside the far edges: the last pixel, not the one past it.
        let clamp = |v: f32, lo: f32, len: f32| v.clamp(lo, lo + (len - 0.01).max(0.0));
        let (cx, cy) = (
            clamp(f32::from(local.x), x, w),
            clamp(f32::from(local.y), y, h),
        );
        self.fit.to_stream(cx, cy).unwrap_or((0.0, 0.0))
    }

    /// Where the stream pixel `(x, y)` is drawn, in window points.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn to_window(self, x: f32, y: f32) -> Point<Pixels> {
        let (vx, vy) = self.fit.to_view(x, y);
        self.origin + point(px(vx), px(vy))
    }
}

/// Where a scroll gesture over the picture has got to: what the Mac has been told is open, so
/// a second `Started` is not a second gesture and a fling whose close was lost is closed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Scrolling {
    /// Nothing open. A mouse wheel's notch never leaves this state.
    #[default]
    Idle,
    /// Between `Started` and `Ended`: the fingers are down.
    Fingers,
    /// The fingers have lifted and the momentum they left is still coasting.
    Momentum,
}

/// A scroll as the view read it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Scroll {
    /// Horizontal: points for a trackpad, lines for a wheel.
    pub dx: f32,
    /// Vertical.
    pub dy: f32,
    /// A trackpad's (points, with phases) rather than a wheel's (lines).
    pub precise: bool,
    /// The fingers' phase.
    pub touch: TouchPhase,
    /// The coast's phase, once the fingers lifted.
    pub momentum: Option<TouchPhase>,
    /// Where, in stream pixels.
    pub at: (f32, f32),
    /// Modifiers.
    pub mods: Mods,
    /// When, in the wire's microseconds (`Input::time_us`).
    pub time_us: u32,
}

/// The view's input state, and what it has to send.
#[derive(Debug, Default)]
pub(crate) struct Input {
    /// Keys whose press went to the Mac and whose release has not.
    held: Vec<KeyCode>,
    /// The modifiers as GPUI last reported them.
    modifiers: Modifiers,
    /// Caps Lock's state as last sent.
    caps: Option<bool>,
    /// Buttons down on the Mac.
    buttons: Vec<ProtoButton>,
    /// Where the scroll gesture has got to.
    scrolling: Scrolling,
    /// A pinch is under way on the Mac.
    pinching: bool,
    /// The pointer's last place on the picture, in stream pixels.
    at: (f32, f32),
    /// To send, oldest first.
    out: Vec<ScreenInput>,
}

impl Input {
    /// What to send, in order.
    pub(crate) fn take(&mut self) -> Vec<ScreenInput> {
        std::mem::take(&mut self.out)
    }

    /// Whether a scroll's coast is open on the Mac (its close is armed for [`MOMENTUM_GAP`]).
    #[must_use]
    pub(crate) fn coasting(&self) -> bool {
        self.scrolling == Scrolling::Momentum
    }

    /// Buttons down on the Mac.
    #[must_use]
    pub(crate) fn dragging(&self) -> bool {
        !self.buttons.is_empty()
    }

    fn send(&mut self, input: ScreenInput) {
        self.out.push(input);
    }

    // The keyboard.

    /// The view took the keyboard: the Mac hears Caps Lock's state.
    pub(crate) fn focused(&mut self, caps: Option<bool>) {
        self.caps = None;
        if let Some(on) = caps {
            self.set_caps(on);
        }
    }

    /// Caps Lock is `on` here: the Mac's follows, when it differs from what was sent.
    fn set_caps(&mut self, on: bool) {
        if self.caps != Some(on) {
            self.caps = Some(on);
            self.send(ScreenInput::Lock { caps: on });
        }
    }

    /// A key went down or repeats; whether the view took it. `native` is the event being
    /// dispatched, when the platform can read it.
    pub(crate) fn key_pressed(
        &mut self,
        native: Option<NativeKey>,
        keystroke: &Keystroke,
        repeat: bool,
    ) -> bool {
        if keys::stays_here(keystroke) {
            return false;
        }
        let native = native.filter(|n| {
            matches!(
                (n.kind, repeat),
                (KeyKind::Down, false) | (KeyKind::Repeat, true)
            )
        });
        let (at, mods) = keys::code_of(native, keystroke);
        if at == KeyCode::Unidentified {
            return false;
        }
        if let Some(key) = keys::media_key(at) {
            self.send(ScreenInput::Media { key, down: true });
            return true;
        }
        self.hold_key(at);
        let action = if repeat {
            KeyAction::Repeat
        } else {
            KeyAction::Press
        };
        self.send(ScreenInput::Key {
            code: at,
            action,
            mods,
        });
        true
    }

    /// `code` is down on the Mac until its release goes.
    fn hold_key(&mut self, code: KeyCode) {
        if !self.held.contains(&code) {
            self.held.push(code);
        }
    }

    /// A key went down on the Mac: remember it as held and send it.
    fn press_key(&mut self, code: KeyCode, mods: Mods) {
        self.hold_key(code);
        self.send(ScreenInput::Key {
            code,
            action: KeyAction::Press,
            mods,
        });
    }

    /// A key whose press went to the Mac was let go; one whose press did not is not sent.
    fn release_key(&mut self, code: KeyCode, mods: Mods) -> bool {
        let Some(at) = self.held.iter().position(|&c| c == code) else {
            return false;
        };
        self.held.swap_remove(at);
        self.send(ScreenInput::Key {
            code,
            action: KeyAction::Release,
            mods,
        });
        true
    }

    /// A key went up; whether its press had gone to the Mac.
    pub(crate) fn key_released(
        &mut self,
        native: Option<NativeKey>,
        keystroke: &Keystroke,
    ) -> bool {
        let native = native.filter(|n| n.kind == KeyKind::Up);
        let (at, mods) = keys::code_of(native, keystroke);
        if let Some(key) = keys::media_key(at) {
            self.send(ScreenInput::Media { key, down: false });
            return true;
        }
        self.release_key(at, mods)
    }

    /// A key the shortcut grab took off the app before GPUI saw it (`super::shortcuts`): it
    /// goes to the Mac by its place with the modifiers it was pressed with. The Mission Control
    /// key has no place, and goes as ⌃↑, Mission Control's shortcut on the Mac.
    pub(crate) fn taken_key(&mut self, key: NativeKey) {
        if key.vk == super::shortcuts::MISSION_CONTROL {
            if key.kind == KeyKind::Down {
                for action in [KeyAction::Press, KeyAction::Release] {
                    self.send(ScreenInput::Key {
                        code: KeyCode::ArrowUp,
                        action,
                        mods: Mods::CTRL,
                    });
                }
            }
            return;
        }
        let Some(code) = KeyCode::from_mac_vk(key.vk) else {
            return;
        };
        match key.kind {
            KeyKind::Down | KeyKind::Repeat => {
                self.hold_key(code);
                let action = if key.kind == KeyKind::Repeat {
                    KeyAction::Repeat
                } else {
                    KeyAction::Press
                };
                self.send(ScreenInput::Key {
                    code,
                    action,
                    mods: key.mods,
                });
            }
            KeyKind::Up => {
                self.release_key(code, key.mods);
            }
            KeyKind::Flags => {}
        }
    }

    /// The modifier keys moved, or Caps Lock did: each modifier that went down or up goes as
    /// its own key, so a remote program sees ⌘ held on its own, ⌥ pressed over a menu, ⇧ held
    /// to run; Caps Lock goes as its state, never as a key; fn only rides on the other keys.
    pub(crate) fn modifiers_moved(
        &mut self,
        native: Option<NativeKey>,
        now: Modifiers,
        caps: bool,
    ) {
        self.set_caps(caps);
        let was = std::mem::replace(&mut self.modifiers, now);
        let native = native.filter(|n| n.kind == KeyKind::Flags);
        match native.and_then(|n| KeyCode::from_mac_vk(n.vk).map(|code| (code, n.mods))) {
            // The key that moved, with its side: a modifier key toggles on each change. A press
            // needs its modifier held now, so a change GPUI reports with an older event behind
            // it presses nothing.
            Some((code, mods)) => {
                if !matches!(code, KeyCode::CapsLock | KeyCode::Fn)
                    && !self.release_key(code, mods)
                    && keys::holds(now, code)
                {
                    self.press_key(code, mods);
                }
                // GPUI reports no change that leaves the modifiers as they were, so with both
                // ⇧ keys down, letting go of the left one is never heard: whatever holds a
                // modifier that is off now is let go.
                let stale: Vec<KeyCode> = self
                    .held
                    .iter()
                    .copied()
                    .filter(|&held| keys::is_side(held) && !keys::holds(now, held))
                    .collect();
                for held in stale {
                    self.release_key(held, mods);
                }
            }
            // No position to read (a test): the left key stands for both.
            None => {
                let mods = keys::mods(now);
                for (code, action) in keys::modifier_keys(was, now) {
                    match action {
                        KeyAction::Release => {
                            self.release_key(code, mods);
                        }
                        KeyAction::Press | KeyAction::Repeat => self.press_key(code, mods),
                    }
                }
            }
        }
    }

    // The pointer.

    /// The pointer moved to `at` on the picture.
    pub(crate) fn moved(&mut self, at: (f32, f32)) {
        self.at = at;
        self.send(ScreenInput::Move { x: at.0, y: at.1 });
    }

    /// A button went down at `at`, its `clicks`-th in a row.
    pub(crate) fn button_down(
        &mut self,
        button: ProtoButton,
        at: (f32, f32),
        clicks: usize,
        mods: Mods,
    ) {
        self.at = at;
        if !self.buttons.contains(&button) {
            self.buttons.push(button);
        }
        self.send(ScreenInput::Button {
            button,
            down: true,
            x: at.0,
            y: at.1,
            clicks: clicks_of(clicks),
            mods,
        });
    }

    /// A button went up at `at`; whether its press had gone to the Mac. Up beside the picture
    /// it still goes, at the picture's edge.
    pub(crate) fn button_up(
        &mut self,
        button: ProtoButton,
        at: (f32, f32),
        clicks: usize,
        mods: Mods,
    ) -> bool {
        let Some(i) = self.buttons.iter().position(|&b| b == button) else {
            return false;
        };
        self.buttons.swap_remove(i);
        self.at = at;
        self.send(ScreenInput::Button {
            button,
            down: false,
            x: at.0,
            y: at.1,
            clicks: clicks_of(clicks),
            mods,
        });
        true
    }

    /// A scroll: a wheel's notch, or a step of a trackpad's gesture or of its coast.
    pub(crate) fn scroll(&mut self, scroll: Scroll) {
        // A finger landing on a fling stops it, and macOS closes the old momentum before it
        // opens the new gesture.
        if scroll.precise && scroll.touch == TouchPhase::Started && scroll.momentum.is_none() {
            self.end_momentum(scroll.at, scroll.mods, scroll.time_us);
        }
        let (phase, momentum) = self.scroll_phases(scroll.precise, scroll.touch, scroll.momentum);
        self.at = scroll.at;
        self.send(ScreenInput::Scroll {
            dx: scroll.dx,
            dy: scroll.dy,
            precise: scroll.precise,
            phase,
            momentum,
            x: scroll.at.0,
            y: scroll.at.1,
            mods: scroll.mods,
            time_us: scroll.time_us,
        });
    }

    /// The gesture and momentum phases a scroll goes with, keeping track of what is open.
    fn scroll_phases(
        &mut self,
        precise: bool,
        touch: TouchPhase,
        momentum: Option<TouchPhase>,
    ) -> (ScrollPhase, ScrollPhase) {
        if !precise {
            return (ScrollPhase::None, ScrollPhase::None);
        }
        if let Some(momentum) = momentum {
            self.scrolling = match momentum {
                TouchPhase::Started | TouchPhase::Moved => Scrolling::Momentum,
                TouchPhase::Ended | TouchPhase::Cancelled => Scrolling::Idle,
            };
            return (ScrollPhase::None, phase_of(momentum));
        }
        match (touch, self.scrolling) {
            // gpui reads `NSEventPhaseMayBegin` and `NSEventPhaseBegan` as the same `Started`,
            // so fingers that rest before they push open the gesture twice. The second is the
            // same gesture; telling the Mac it began again would restart its rubber-banding.
            (TouchPhase::Started, Scrolling::Fingers) | (TouchPhase::Moved, _) => {
                (ScrollPhase::Changed, ScrollPhase::None)
            }
            (TouchPhase::Started, _) => {
                self.scrolling = Scrolling::Fingers;
                (ScrollPhase::Began, ScrollPhase::None)
            }
            (TouchPhase::Ended | TouchPhase::Cancelled, _) => {
                self.scrolling = Scrolling::Idle;
                (phase_of(touch), ScrollPhase::None)
            }
        }
    }

    /// The zero-delta `momentumPhase = End` macOS sends when a fling stops, for a coast whose
    /// own close never came: without it the remote app stays latched to a scroll that never
    /// ended. Whether one went.
    pub(crate) fn end_momentum(&mut self, at: (f32, f32), mods: Mods, time_us: u32) -> bool {
        if self.scrolling != Scrolling::Momentum {
            return false;
        }
        self.scrolling = Scrolling::Idle;
        self.send(ScreenInput::Scroll {
            dx: 0.0,
            dy: 0.0,
            precise: true,
            phase: ScrollPhase::None,
            momentum: ScrollPhase::Ended,
            x: at.0,
            y: at.1,
            mods,
            time_us,
        });
        true
    }

    /// A pinch step: `delta` the zoom since the last (0.1 is 10 % in). One that starts beside
    /// the picture is not the Mac's; one under way finishes there wherever it goes.
    pub(crate) fn pinch(
        &mut self,
        delta: f32,
        phase: TouchPhase,
        at: Option<(f32, f32)>,
        edge: (f32, f32),
        time_us: u32,
    ) -> bool {
        if phase == TouchPhase::Started {
            self.pinching = at.is_some();
        }
        if !self.pinching {
            return false;
        }
        self.pinching = !matches!(phase, TouchPhase::Ended | TouchPhase::Cancelled);
        let (x, y) = at.unwrap_or(edge);
        self.send(ScreenInput::Magnify {
            delta,
            phase: phase_of(phase),
            x,
            y,
            time_us,
        });
        true
    }

    /// A rotation step of `degrees`, counterclockwise positive, at `at`.
    pub(crate) fn rotate(&mut self, degrees: f32, phase: TouchPhase, at: (f32, f32), time_us: u32) {
        self.send(ScreenInput::Rotate {
            degrees,
            phase: phase_of(phase),
            x: at.0,
            y: at.1,
            time_us,
        });
    }

    /// Two fingers tapped twice at `at`.
    pub(crate) fn smart_magnify(&mut self, at: (f32, f32)) {
        self.send(ScreenInput::SmartMagnify { x: at.0, y: at.1 });
    }

    /// A swipe, as AppKit's `deltaX`/`deltaY` say it; whether it was one.
    pub(crate) fn swipe(&mut self, dx: f32, dy: f32, at: (f32, f32)) -> bool {
        let Some(direction) = swipe_direction(dx, dy) else {
            return false;
        };
        self.send(ScreenInput::Swipe {
            direction,
            x: at.0,
            y: at.1,
        });
        true
    }

    /// Focus left the view or the window went inactive: release on the Mac every button, key
    /// and modifier whose press went there, since the release never will (⌘-tab away with ⌘
    /// held, a click elsewhere while a key is down). Buttons first, then plain keys, then the
    /// modifiers; a coast still open is closed.
    pub(crate) fn let_go(&mut self, time_us: u32) {
        let at = self.at;
        for button in std::mem::take(&mut self.buttons) {
            self.send(ScreenInput::Button {
                button,
                down: false,
                x: at.0,
                y: at.1,
                clicks: 1,
                mods: Mods::empty(),
            });
        }
        let (modifiers, plain): (Vec<KeyCode>, Vec<KeyCode>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|&code| keys::is_side(code));
        for code in plain.into_iter().chain(modifiers) {
            self.send(ScreenInput::Key {
                code,
                action: KeyAction::Release,
                mods: Mods::empty(),
            });
        }
        self.modifiers = Modifiers::default();
        self.end_momentum(at, Mods::empty(), time_us);
        self.scrolling = Scrolling::Idle;
        self.pinching = false;
    }
}

/// A click count as the wire carries it.
fn clicks_of(clicks: usize) -> u8 {
    u8::try_from(clicks.max(1)).unwrap_or(u8::MAX)
}

/// A touch phase as a scroll's or a gesture's phase.
#[must_use]
pub(crate) const fn phase_of(touch: TouchPhase) -> ScrollPhase {
    match touch {
        TouchPhase::Started => ScrollPhase::Began,
        TouchPhase::Moved => ScrollPhase::Changed,
        TouchPhase::Ended => ScrollPhase::Ended,
        TouchPhase::Cancelled => ScrollPhase::Cancelled,
    }
}

/// AppKit's swipe deltas as a direction: `deltaX` 1 is a swipe left, −1 right; `deltaY` 1 up,
/// −1 down.
#[must_use]
pub(crate) fn swipe_direction(dx: f32, dy: f32) -> Option<SwipeDirection> {
    if dx.abs() >= dy.abs() {
        if dx > 0.0 {
            Some(SwipeDirection::Left)
        } else if dx < 0.0 {
            Some(SwipeDirection::Right)
        } else {
            None
        }
    } else if dy > 0.0 {
        Some(SwipeDirection::Up)
    } else {
        Some(SwipeDirection::Down)
    }
}

/// A GPUI button as the wire's.
#[must_use]
pub(crate) const fn proto_button(button: gpui::MouseButton) -> ProtoButton {
    match button {
        gpui::MouseButton::Left => ProtoButton::Left,
        gpui::MouseButton::Right => ProtoButton::Right,
        gpui::MouseButton::Middle => ProtoButton::Middle,
        gpui::MouseButton::Navigate(gpui::NavigationDirection::Back) => ProtoButton::Back,
        gpui::MouseButton::Navigate(gpui::NavigationDirection::Forward) => ProtoButton::Forward,
    }
}

#[cfg(test)]
mod tests;
