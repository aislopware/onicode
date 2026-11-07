//! GPUI keystrokes named as the desktop protocol names keys, and the key event AppKit is
//! dispatching now, whose virtual key code is the key's place on the keyboard.
//!
//! A key goes to the Mac as its position, never as the character this Mac's layout puts there:
//! the Mac's own layout makes the character, so ⌘Q is the Q key on both Macs and a dead key
//! composes in the remote app (slopty's ruling, "Keys go by position"). GPUI's keystroke names
//! the character; the position is read off the `NSEvent` being dispatched ([`System`]). A ⌘ or
//! ⌃ chord names its character too ([`chord_char`]), so the Mac presses the key its own layout
//! types it with.

use gpui::{Keystroke, Modifiers};
use onicode_desktop_proto::input::{KeyAction, KeyCode, Mods};
use onicode_desktop_proto::screen::MediaKey;

/// One key event as the keyboard reported it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NativeKey {
    /// The Mac virtual key code (`kVK_*`): the key's position.
    pub vk: u16,
    /// What happened to it.
    pub kind: KeyKind,
    /// The modifiers held, with their side, Caps Lock and fn.
    pub mods: Mods,
}

/// What a [`NativeKey`] event is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyKind {
    /// Went down.
    Down,
    /// The system's auto-repeat.
    Repeat,
    /// Let go.
    Up,
    /// A modifier key went down or up (`flagsChanged:`).
    Flags,
}

/// What the view asks the platform about the keyboard. A seam: tests drive a stand-in.
pub(crate) trait KeyPlatform {
    /// The key event being dispatched now, with its position and its modifiers' sides.
    fn native(&self) -> Option<NativeKey>;
    /// Whether Caps Lock is on.
    fn caps_lock(&self) -> Option<bool>;
}

/// This Mac's own keyboard.
pub(crate) struct System;

impl KeyPlatform for System {
    fn native(&self) -> Option<NativeKey> {
        mac::current()
    }

    fn caps_lock(&self) -> Option<bool> {
        mac::caps_lock()
    }
}

/// A keyboard that answers nothing: under test, where no real key is ever read.
#[cfg(test)]
pub(crate) struct Inert;

#[cfg(test)]
impl KeyPlatform for Inert {
    fn native(&self) -> Option<NativeKey> {
        None
    }

    fn caps_lock(&self) -> Option<bool> {
        None
    }
}

/// GPUI's modifiers as the wire's, fn included; no side is known from them.
#[must_use]
pub(crate) fn mods(m: Modifiers) -> Mods {
    let mut out = Mods::empty();
    out.set(Mods::SHIFT, m.shift);
    out.set(Mods::ALT, m.alt);
    out.set(Mods::CTRL, m.control);
    out.set(Mods::SUPER, m.platform);
    out.set(Mods::FN, m.function);
    out
}

/// The key GPUI's name for it stands for: its character's place on a US keyboard, or a named
/// key's own.
#[must_use]
pub(crate) fn key_code(key: &str) -> KeyCode {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return single_char(c);
    }
    match key {
        "enter" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Space,
        "backspace" => KeyCode::Backspace,
        "escape" => KeyCode::Escape,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "up" => KeyCode::ArrowUp,
        "down" => KeyCode::ArrowDown,
        "left" => KeyCode::ArrowLeft,
        "right" => KeyCode::ArrowRight,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "capslock" => KeyCode::CapsLock,
        "f1" => KeyCode::F1,
        "f2" => KeyCode::F2,
        "f3" => KeyCode::F3,
        "f4" => KeyCode::F4,
        "f5" => KeyCode::F5,
        "f6" => KeyCode::F6,
        "f7" => KeyCode::F7,
        "f8" => KeyCode::F8,
        "f9" => KeyCode::F9,
        "f10" => KeyCode::F10,
        "f11" => KeyCode::F11,
        "f12" => KeyCode::F12,
        "f13" => KeyCode::F13,
        "f14" => KeyCode::F14,
        "f15" => KeyCode::F15,
        "f16" => KeyCode::F16,
        "f17" => KeyCode::F17,
        "f18" => KeyCode::F18,
        "f19" => KeyCode::F19,
        "f20" => KeyCode::F20,
        // The keypad's own names (gpui-fast's).
        "kp0" => KeyCode::Numpad0,
        "kp1" => KeyCode::Numpad1,
        "kp2" => KeyCode::Numpad2,
        "kp3" => KeyCode::Numpad3,
        "kp4" => KeyCode::Numpad4,
        "kp5" => KeyCode::Numpad5,
        "kp6" => KeyCode::Numpad6,
        "kp7" => KeyCode::Numpad7,
        "kp8" => KeyCode::Numpad8,
        "kp9" => KeyCode::Numpad9,
        "kpadd" => KeyCode::NumpadAdd,
        "kpsubtract" => KeyCode::NumpadSubtract,
        "kpmultiply" => KeyCode::NumpadMultiply,
        "kpdivide" => KeyCode::NumpadDivide,
        "kpdecimal" => KeyCode::NumpadDecimal,
        "kpequal" => KeyCode::NumpadEqual,
        "kpenter" => KeyCode::NumpadEnter,
        _ => KeyCode::Unidentified,
    }
}

/// A character's place on a US keyboard.
const fn single_char(c: char) -> KeyCode {
    match c.to_ascii_lowercase() {
        'a' => KeyCode::A,
        'b' => KeyCode::B,
        'c' => KeyCode::C,
        'd' => KeyCode::D,
        'e' => KeyCode::E,
        'f' => KeyCode::F,
        'g' => KeyCode::G,
        'h' => KeyCode::H,
        'i' => KeyCode::I,
        'j' => KeyCode::J,
        'k' => KeyCode::K,
        'l' => KeyCode::L,
        'm' => KeyCode::M,
        'n' => KeyCode::N,
        'o' => KeyCode::O,
        'p' => KeyCode::P,
        'q' => KeyCode::Q,
        'r' => KeyCode::R,
        's' => KeyCode::S,
        't' => KeyCode::T,
        'u' => KeyCode::U,
        'v' => KeyCode::V,
        'w' => KeyCode::W,
        'x' => KeyCode::X,
        'y' => KeyCode::Y,
        'z' => KeyCode::Z,
        '0' => KeyCode::Digit0,
        '1' => KeyCode::Digit1,
        '2' => KeyCode::Digit2,
        '3' => KeyCode::Digit3,
        '4' => KeyCode::Digit4,
        '5' => KeyCode::Digit5,
        '6' => KeyCode::Digit6,
        '7' => KeyCode::Digit7,
        '8' => KeyCode::Digit8,
        '9' => KeyCode::Digit9,
        '-' => KeyCode::Minus,
        '=' => KeyCode::Equal,
        '[' => KeyCode::BracketLeft,
        ']' => KeyCode::BracketRight,
        '\\' => KeyCode::Backslash,
        ';' => KeyCode::Semicolon,
        '\'' => KeyCode::Quote,
        ',' => KeyCode::Comma,
        '.' => KeyCode::Period,
        '/' => KeyCode::Slash,
        '`' => KeyCode::Backquote,
        ' ' => KeyCode::Space,
        _ => KeyCode::Unidentified,
    }
}

/// The key a native event is, by its position; with no position (a test, a key GPUI made up),
/// the character's place on a US keyboard.
#[must_use]
pub(crate) fn code_of(native: Option<NativeKey>, keystroke: &Keystroke) -> (KeyCode, Mods) {
    native
        .and_then(|n| KeyCode::from_mac_vk(n.vk).map(|code| (code, n.mods)))
        .unwrap_or_else(|| (key_code(&keystroke.key), mods(keystroke.modifiers)))
}

/// The character a chord's key names, as the wire carries it (`ScreenInput::Key::chord`): the
/// key's own character, lowercased, when it is one character; a named key (`left`, `f1`) names
/// none.
#[must_use]
pub(crate) fn chord_char(key: &str) -> Option<String> {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c.to_lowercase().collect()),
        _ => None,
    }
}

/// A key of the numeric keypad: its place is right whatever it types.
#[must_use]
pub(crate) const fn is_keypad(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Numpad0
            | KeyCode::Numpad1
            | KeyCode::Numpad2
            | KeyCode::Numpad3
            | KeyCode::Numpad4
            | KeyCode::Numpad5
            | KeyCode::Numpad6
            | KeyCode::Numpad7
            | KeyCode::Numpad8
            | KeyCode::Numpad9
            | KeyCode::NumpadAdd
            | KeyCode::NumpadSubtract
            | KeyCode::NumpadMultiply
            | KeyCode::NumpadDivide
            | KeyCode::NumpadDecimal
            | KeyCode::NumpadEqual
            | KeyCode::NumpadEnter
            | KeyCode::NumpadClear
    )
}

/// ⌘⌥⎋ (force quit) and ⌃⌘Q (lock the screen): the ways out that stay on this Mac.
#[must_use]
pub(crate) fn stays_here(keystroke: &Keystroke) -> bool {
    let m = keystroke.modifiers;
    (m.platform && m.alt && !m.control && keystroke.key == "escape")
        || (m.platform && m.control && !m.alt && keystroke.key == "q")
}

/// The media key a key code is.
#[must_use]
pub(crate) const fn media_key(code: KeyCode) -> Option<MediaKey> {
    match code {
        KeyCode::MediaPlayPause => Some(MediaKey::PlayPause),
        KeyCode::MediaTrackNext => Some(MediaKey::Next),
        KeyCode::MediaTrackPrevious => Some(MediaKey::Previous),
        _ => None,
    }
}

/// A modifier key with a side: ⇧, ⌃, ⌥ or ⌘, left or right.
#[must_use]
pub(crate) const fn is_side(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::ShiftLeft
            | KeyCode::ShiftRight
            | KeyCode::ControlLeft
            | KeyCode::ControlRight
            | KeyCode::AltLeft
            | KeyCode::AltRight
            | KeyCode::MetaLeft
            | KeyCode::MetaRight
    )
}

/// Whether `mods` has the modifier the key `code` holds.
#[must_use]
pub(crate) const fn holds(mods: Modifiers, code: KeyCode) -> bool {
    match code {
        KeyCode::ShiftLeft | KeyCode::ShiftRight => mods.shift,
        KeyCode::ControlLeft | KeyCode::ControlRight => mods.control,
        KeyCode::AltLeft | KeyCode::AltRight => mods.alt,
        KeyCode::MetaLeft | KeyCode::MetaRight => mods.platform,
        _ => false,
    }
}

/// The modifier keys that went down or up between `was` and `now`, as presses and releases,
/// the left key standing for both. The fn key is left out: the Mac's own fn setting (emoji
/// picker, dictation) would fire.
#[must_use]
pub(crate) fn modifier_keys(was: Modifiers, now: Modifiers) -> Vec<(KeyCode, KeyAction)> {
    [
        (was.shift, now.shift, KeyCode::ShiftLeft),
        (was.control, now.control, KeyCode::ControlLeft),
        (was.alt, now.alt, KeyCode::AltLeft),
        (was.platform, now.platform, KeyCode::MetaLeft),
    ]
    .into_iter()
    .filter(|(before, after, _)| before != after)
    .map(|(_, down, code)| {
        let action = if down {
            KeyAction::Press
        } else {
            KeyAction::Release
        };
        (code, action)
    })
    .collect()
}

/// AppKit's side of the keyboard: the event being dispatched and Caps Lock.
mod mac {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use onicode_desktop_proto::input::Mods;

    use super::{KeyKind, NativeKey};

    /// Which key of a pair holds a modifier: the device-dependent bits of an event's flags,
    /// `NX_DEVICE*KEYMASK` in `<IOKit/hidsystem/IOLLEvent.h>`, which objc2 does not bind.
    const LEFT_CONTROL: usize = 0x0000_0001;
    const LEFT_SHIFT: usize = 0x0000_0002;
    const RIGHT_SHIFT: usize = 0x0000_0004;
    const LEFT_COMMAND: usize = 0x0000_0008;
    const RIGHT_COMMAND: usize = 0x0000_0010;
    const LEFT_OPTION: usize = 0x0000_0020;
    const RIGHT_OPTION: usize = 0x0000_0040;
    const RIGHT_CONTROL: usize = 0x0000_2000;

    /// An event's modifier flags as [`Mods`]: each modifier with the right side marked when
    /// only the right key holds it, Caps Lock and fn.
    fn mods_of(flags: NSEventModifierFlags) -> Mods {
        let raw = flags.0;
        let mut mods = Mods::empty();
        for (flag, generic, left, right, right_mod) in [
            (
                NSEventModifierFlags::Shift,
                Mods::SHIFT,
                LEFT_SHIFT,
                RIGHT_SHIFT,
                Mods::SHIFT_RIGHT,
            ),
            (
                NSEventModifierFlags::Control,
                Mods::CTRL,
                LEFT_CONTROL,
                RIGHT_CONTROL,
                Mods::CTRL_RIGHT,
            ),
            (
                NSEventModifierFlags::Option,
                Mods::ALT,
                LEFT_OPTION,
                RIGHT_OPTION,
                Mods::ALT_RIGHT,
            ),
            (
                NSEventModifierFlags::Command,
                Mods::SUPER,
                LEFT_COMMAND,
                RIGHT_COMMAND,
                Mods::SUPER_RIGHT,
            ),
        ] {
            if !flags.contains(flag) {
                continue;
            }
            mods |= generic;
            if raw & right != 0 && raw & left == 0 {
                mods |= right_mod;
            }
        }
        mods.set(
            Mods::CAPS_LOCK,
            flags.contains(NSEventModifierFlags::CapsLock),
        );
        mods.set(Mods::FN, flags.contains(NSEventModifierFlags::Function));
        mods
    }

    /// The key event AppKit is dispatching now (what GPUI's key handlers are being called for),
    /// or `None` outside one and off the main thread.
    pub(super) fn current() -> Option<NativeKey> {
        let mtm = MainThreadMarker::new()?;
        let event = NSApplication::sharedApplication(mtm).currentEvent()?;
        let kind = match event.r#type() {
            NSEventType::KeyDown if event.isARepeat() => KeyKind::Repeat,
            NSEventType::KeyDown => KeyKind::Down,
            NSEventType::KeyUp => KeyKind::Up,
            NSEventType::FlagsChanged => KeyKind::Flags,
            _ => return None,
        };
        Some(NativeKey {
            vk: event.keyCode(),
            kind,
            mods: mods_of(event.modifierFlags()),
        })
    }

    /// Whether Caps Lock is on; `None` off the main thread.
    pub(super) fn caps_lock() -> Option<bool> {
        MainThreadMarker::new()?;
        Some(NSEvent::modifierFlags_class().contains(NSEventModifierFlags::CapsLock))
    }
}
