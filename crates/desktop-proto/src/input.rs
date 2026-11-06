//! Keyboard and pointer events as the client observed them.
//!
//! The client sends *what happened* (physical key, modifiers, the text the OS produced); the host
//! posts it by position. Names follow the W3C UI Events `code` values.
//!
//! Ported from slopty's `slopty-proto::input` (MIT, same authors).

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// Every physical key the engine knows.
///
/// One name per W3C `KeyboardEvent.code` value plus the numpad/browser extras ghostty recognises.
/// Exported so the engine's mapping is generated from the same list and checked exhaustively.
#[macro_export]
macro_rules! for_each_key_code {
    ($callback:ident) => {
        $callback! {
            Unidentified, Backquote, Backslash, BracketLeft, BracketRight, Comma, Digit0, Digit1,
            Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
            Equal, IntlBackslash, IntlRo, IntlYen, A, B, C, D,
            E, F, G, H, I, J, K, L,
            M, N, O, P, Q, R, S, T,
            U, V, W, X, Y, Z, Minus, Period,
            Quote, Semicolon, Slash, AltLeft, AltRight, Backspace, CapsLock, ContextMenu,
            ControlLeft, ControlRight, Enter, MetaLeft, MetaRight, ShiftLeft, ShiftRight, Space,
            Tab, Convert, KanaMode, NonConvert, Delete, End, Help, Home,
            Insert, PageDown, PageUp, ArrowDown, ArrowLeft, ArrowRight, ArrowUp, NumLock,
            Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7,
            Numpad8, Numpad9, NumpadAdd, NumpadBackspace, NumpadClear, NumpadClearEntry, NumpadComma, NumpadDecimal,
            NumpadDivide, NumpadEnter, NumpadEqual, NumpadMemoryAdd, NumpadMemoryClear, NumpadMemoryRecall, NumpadMemoryStore, NumpadMemorySubtract,
            NumpadMultiply, NumpadParenLeft, NumpadParenRight, NumpadSubtract, NumpadSeparator, NumpadUp, NumpadDown, NumpadRight,
            NumpadLeft, NumpadBegin, NumpadHome, NumpadEnd, NumpadInsert, NumpadDelete, NumpadPageUp, NumpadPageDown,
            Escape, F1, F2, F3, F4, F5, F6, F7,
            F8, F9, F10, F11, F12, F13, F14, F15,
            F16, F17, F18, F19, F20, F21, F22, F23,
            F24, F25, Fn, FnLock, PrintScreen, ScrollLock, Pause, BrowserBack,
            BrowserFavorites, BrowserForward, BrowserHome, BrowserRefresh, BrowserSearch, BrowserStop, Eject, LaunchApp1,
            LaunchApp2, LaunchMail, MediaPlayPause, MediaSelect, MediaStop, MediaTrackNext, MediaTrackPrevious, Power,
            Sleep, AudioVolumeDown, AudioVolumeMute, AudioVolumeUp, WakeUp, Copy, Cut, Paste,
        }
    };
}

macro_rules! define_key_code {
    ($($name:ident),* $(,)?) => {
        /// Physical key (W3C `KeyboardEvent.code`), same set and order as the engine's.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, Default)]
        #[expect(missing_docs, reason = "W3C key names are self-describing")]
        pub enum KeyCode {
            #[default]
            $($name,)*
        }

        impl KeyCode {
            /// Every key, in wire order.
            pub const ALL: &[Self] = &[$(Self::$name),*];
        }
    };
}
for_each_key_code!(define_key_code);

/// Keys that share a Mac key position with another: [`KeyCode::from_mac_vk`] names the other.
const MAC_ALIASES: &[KeyCode] = &[
    KeyCode::NumLock,
    KeyCode::PrintScreen,
    KeyCode::ScrollLock,
    KeyCode::Pause,
    KeyCode::Insert,
];

impl KeyCode {
    /// The Mac virtual key code (`kVK_*`, `<HIToolbox/Events.h>`) of this key's position, or
    /// `None` when macOS has no keyboard event for it (media and browser keys are
    /// system-defined events, not key presses).
    ///
    /// A virtual key code names a *position*: the layout of the Mac that reads it decides the
    /// character. The client and the worker share this one table.
    #[must_use]
    pub const fn to_mac_vk(self) -> Option<u16> {
        let vk: u16 = match self {
            Self::A => 0x00,
            Self::S => 0x01,
            Self::D => 0x02,
            Self::F => 0x03,
            Self::H => 0x04,
            Self::G => 0x05,
            Self::Z => 0x06,
            Self::X => 0x07,
            Self::C => 0x08,
            Self::V => 0x09,
            Self::IntlBackslash => 0x0a,
            Self::B => 0x0b,
            Self::Q => 0x0c,
            Self::W => 0x0d,
            Self::E => 0x0e,
            Self::R => 0x0f,
            Self::Y => 0x10,
            Self::T => 0x11,
            Self::Digit1 => 0x12,
            Self::Digit2 => 0x13,
            Self::Digit3 => 0x14,
            Self::Digit4 => 0x15,
            Self::Digit6 => 0x16,
            Self::Digit5 => 0x17,
            Self::Equal => 0x18,
            Self::Digit9 => 0x19,
            Self::Digit7 => 0x1a,
            Self::Minus => 0x1b,
            Self::Digit8 => 0x1c,
            Self::Digit0 => 0x1d,
            Self::BracketRight => 0x1e,
            Self::O => 0x1f,
            Self::U => 0x20,
            Self::BracketLeft => 0x21,
            Self::I => 0x22,
            Self::P => 0x23,
            Self::Enter => 0x24,
            Self::L => 0x25,
            Self::J => 0x26,
            Self::Quote => 0x27,
            Self::K => 0x28,
            Self::Semicolon => 0x29,
            Self::Backslash => 0x2a,
            Self::Comma => 0x2b,
            Self::Slash => 0x2c,
            Self::N => 0x2d,
            Self::M => 0x2e,
            Self::Period => 0x2f,
            Self::Tab => 0x30,
            Self::Space => 0x31,
            Self::Backquote => 0x32,
            Self::Backspace => 0x33,
            Self::Escape => 0x35,
            Self::MetaRight => 0x36,
            Self::MetaLeft => 0x37,
            Self::ShiftLeft => 0x38,
            Self::CapsLock => 0x39,
            Self::AltLeft => 0x3a,
            Self::ControlLeft => 0x3b,
            Self::ShiftRight => 0x3c,
            Self::AltRight => 0x3d,
            Self::ControlRight => 0x3e,
            Self::Fn => 0x3f,
            Self::F17 => 0x40,
            Self::NumpadDecimal => 0x41,
            Self::NumpadMultiply => 0x43,
            Self::NumpadAdd => 0x45,
            Self::NumpadClear | Self::NumLock => 0x47,
            Self::AudioVolumeUp => 0x48,
            Self::AudioVolumeDown => 0x49,
            Self::AudioVolumeMute => 0x4a,
            Self::NumpadDivide => 0x4b,
            Self::NumpadEnter => 0x4c,
            Self::NumpadSubtract => 0x4e,
            Self::F18 => 0x4f,
            Self::F19 => 0x50,
            Self::NumpadEqual => 0x51,
            Self::Numpad0 => 0x52,
            Self::Numpad1 => 0x53,
            Self::Numpad2 => 0x54,
            Self::Numpad3 => 0x55,
            Self::Numpad4 => 0x56,
            Self::Numpad5 => 0x57,
            Self::Numpad6 => 0x58,
            Self::Numpad7 => 0x59,
            Self::F20 => 0x5a,
            Self::Numpad8 => 0x5b,
            Self::Numpad9 => 0x5c,
            Self::IntlYen => 0x5d,
            Self::IntlRo => 0x5e,
            Self::NumpadComma => 0x5f,
            Self::F5 => 0x60,
            Self::F6 => 0x61,
            Self::F7 => 0x62,
            Self::F3 => 0x63,
            Self::F8 => 0x64,
            Self::F9 => 0x65,
            Self::Convert => 0x66,
            Self::F11 => 0x67,
            Self::KanaMode => 0x68,
            Self::F13 | Self::PrintScreen => 0x69,
            Self::F16 => 0x6a,
            Self::F14 | Self::ScrollLock => 0x6b,
            Self::F10 => 0x6d,
            Self::ContextMenu => 0x6e,
            Self::F12 => 0x6f,
            Self::F15 | Self::Pause => 0x71,
            Self::Help | Self::Insert => 0x72,
            Self::Home => 0x73,
            Self::PageUp => 0x74,
            Self::Delete => 0x75,
            Self::F4 => 0x76,
            Self::End => 0x77,
            Self::F2 => 0x78,
            Self::PageDown => 0x79,
            Self::F1 => 0x7a,
            Self::ArrowLeft => 0x7b,
            Self::ArrowRight => 0x7c,
            Self::ArrowDown => 0x7d,
            Self::ArrowUp => 0x7e,
            _ => return None,
        };
        Some(vk)
    }

    /// The key at Mac virtual key code `vk`, the inverse of [`Self::to_mac_vk`]; where two keys
    /// share a position it names the Mac's own (`NumpadClear`, `F13`, `Help`…). `None` for a
    /// code no key has.
    #[must_use]
    pub fn from_mac_vk(vk: u16) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|code| code.to_mac_vk() == Some(vk) && !MAC_ALIASES.contains(code))
    }
}

bitflags! {
    /// Modifier state. The `*_RIGHT` bits say which side, when the OS reports it.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct Mods: u16 {
        /// Shift.
        const SHIFT = 1 << 0;
        /// Option / Alt.
        const ALT = 1 << 1;
        /// Control.
        const CTRL = 1 << 2;
        /// Command / Super.
        const SUPER = 1 << 3;
        /// Caps Lock is on.
        const CAPS_LOCK = 1 << 4;
        /// Num Lock is on.
        const NUM_LOCK = 1 << 5;
        /// Right shift (only meaningful with `SHIFT`).
        const SHIFT_RIGHT = 1 << 6;
        /// Right alt.
        const ALT_RIGHT = 1 << 7;
        /// Right control.
        const CTRL_RIGHT = 1 << 8;
        /// Right super.
        const SUPER_RIGHT = 1 << 9;
        /// The fn (Globe) key is held. Screen input only: the worker sets the event's
        /// secondary-fn flag, which apps read for fn-click and fn-drag; the terminal's encoder
        /// never sees it.
        const FN = 1 << 10;
    }
}

/// Press, repeat, or release.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum KeyAction {
    /// Key went down.
    Press,
    /// OS auto-repeat.
    Repeat,
    /// Key went up (only forwarded when the kitty protocol asks for it).
    Release,
}

/// One key event, shaped exactly like ghostty's own view constructs it.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct KeyEvent {
    /// Client-assigned sequence number; the worker echoes the highest applied one in each frame so
    /// the prediction engine can retire its speculation.
    pub seq: u64,
    /// Press / repeat / release.
    pub action: KeyAction,
    /// Physical key.
    pub code: KeyCode,
    /// Modifiers held.
    pub mods: Mods,
    /// Modifiers the OS already consumed to produce `text` (e.g. Shift for `A`). Excludes Ctrl
    /// and Cmd, which the encoder handles itself.
    pub consumed_mods: Mods,
    /// Text produced by the keyboard layout, without control folding. `None` for non-text keys.
    pub text: Option<String>,
    /// The key's codepoint with no modifiers applied (`a` for Shift+A), for kitty alternates.
    pub unshifted: Option<char>,
    /// Inside an IME composition; the engine must not encode it.
    pub composing: bool,
    /// The ⌥ held is Alt for this press (the client's `option_as_alt` setting, resolved for
    /// the side): `text` is then the key without ⌥ and the worker prefixes an escape instead of
    /// typing the layout's symbol. Per key, since the encoder is the worker's and a session's
    /// clients may differ.
    pub option_as_alt: bool,
}

/// Pointer button.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MouseButton {
    /// Primary.
    Left,
    /// Secondary.
    Right,
    /// Wheel click.
    Middle,
    /// Back (button 8).
    Back,
    /// Forward (button 9).
    Forward,
}

impl MouseButton {
    /// Its bit in a set of buttons held: left 1, right 2, middle 4, back 8, forward 16.
    #[must_use]
    pub const fn bit(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Right => 1 << 1,
            Self::Middle => 1 << 2,
            Self::Back => 1 << 3,
            Self::Forward => 1 << 4,
        }
    }
}

/// Pointer action.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum MouseAction {
    /// Button down.
    Press,
    /// Button up.
    Release,
    /// Moved (with or without a button held).
    Motion,
    /// Wheel step: positive is up/left, one unit per notch or per row of precise scroll.
    Wheel {
        /// Vertical rows (positive = up).
        rows: i16,
        /// Horizontal columns (positive = left).
        cols: i16,
    },
}

/// One pointer event in terminal cell space, plus the pixel offset the SGR-pixel mode needs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MouseEvent {
    /// Action.
    pub action: MouseAction,
    /// Button involved, if any (`None` for motion without a button).
    pub button: Option<MouseButton>,
    /// Modifiers.
    pub mods: Mods,
    /// Column.
    pub col: u16,
    /// Row.
    pub row: u16,
    /// Pixel x within the terminal content area, in the client's cell metrics.
    pub px: u32,
    /// Pixel y.
    pub py: u32,
}

#[cfg(test)]
mod tests {
    use super::{KeyCode, MAC_ALIASES};

    /// Everything macOS cannot express as a keyboard event, spelled out so a new `KeyCode`
    /// variant fails the test until someone decides where it goes.
    const NO_MAC_KEY: &[KeyCode] = &[
        KeyCode::Unidentified,
        KeyCode::NonConvert,
        KeyCode::NumpadBackspace,
        KeyCode::NumpadClearEntry,
        KeyCode::NumpadMemoryAdd,
        KeyCode::NumpadMemoryClear,
        KeyCode::NumpadMemoryRecall,
        KeyCode::NumpadMemoryStore,
        KeyCode::NumpadMemorySubtract,
        KeyCode::NumpadParenLeft,
        KeyCode::NumpadParenRight,
        KeyCode::NumpadSeparator,
        KeyCode::NumpadUp,
        KeyCode::NumpadDown,
        KeyCode::NumpadRight,
        KeyCode::NumpadLeft,
        KeyCode::NumpadBegin,
        KeyCode::NumpadHome,
        KeyCode::NumpadEnd,
        KeyCode::NumpadInsert,
        KeyCode::NumpadDelete,
        KeyCode::NumpadPageUp,
        KeyCode::NumpadPageDown,
        KeyCode::F21,
        KeyCode::F22,
        KeyCode::F23,
        KeyCode::F24,
        KeyCode::F25,
        KeyCode::FnLock,
        KeyCode::BrowserBack,
        KeyCode::BrowserFavorites,
        KeyCode::BrowserForward,
        KeyCode::BrowserHome,
        KeyCode::BrowserRefresh,
        KeyCode::BrowserSearch,
        KeyCode::BrowserStop,
        KeyCode::Eject,
        KeyCode::LaunchApp1,
        KeyCode::LaunchApp2,
        KeyCode::LaunchMail,
        KeyCode::MediaPlayPause,
        KeyCode::MediaSelect,
        KeyCode::MediaStop,
        KeyCode::MediaTrackNext,
        KeyCode::MediaTrackPrevious,
        KeyCode::Power,
        KeyCode::Sleep,
        KeyCode::WakeUp,
        KeyCode::Copy,
        KeyCode::Cut,
        KeyCode::Paste,
    ];

    #[test]
    fn every_key_has_a_mac_position_or_is_listed() {
        for &code in KeyCode::ALL {
            let mapped = code.to_mac_vk().is_some();
            let listed = NO_MAC_KEY.contains(&code);
            assert!(
                mapped != listed,
                "{code:?}: mapped={mapped} listed={listed}"
            );
        }
    }

    /// The client reads a position and the worker posts it: every virtual key code a key has
    /// comes back as the same code, and only the aliases share one.
    #[test]
    fn every_mac_vk_round_trips() {
        for vk in 0..=u16::from(u8::MAX) {
            if let Some(code) = KeyCode::from_mac_vk(vk) {
                assert_eq!(code.to_mac_vk(), Some(vk), "{vk:#x} → {code:?}");
            }
        }
        for &code in KeyCode::ALL {
            let Some(vk) = code.to_mac_vk() else { continue };
            let back = KeyCode::from_mac_vk(vk);
            if MAC_ALIASES.contains(&code) {
                assert!(
                    back.is_some_and(|b| b != code),
                    "{code:?} names its partner"
                );
            } else {
                assert_eq!(back, Some(code), "{code:?} at {vk:#x}");
            }
        }
        assert_eq!(KeyCode::from_mac_vk(0x0c), Some(KeyCode::Q), "kVK_ANSI_Q");
        assert_eq!(
            KeyCode::from_mac_vk(0x3c),
            Some(KeyCode::ShiftRight),
            "kVK_RightShift"
        );
        assert_eq!(KeyCode::from_mac_vk(0x34), None, "no key there");
    }
}
