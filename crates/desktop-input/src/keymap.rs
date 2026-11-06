//! W3C key codes → macOS virtual key codes (`kVK_*`, ANSI layout positions).
//!
//! Virtual key codes name *positions*: a `KeyCode::A` press lands as whatever the host's
//! keyboard layout puts there, which is the client's own once the host has taken its input
//! source (`crate::sources`). The table is `onicode-desktop-proto`'s, shared with the client that
//! reads positions off its keyboard.

use objc2_core_graphics::CGKeyCode;
use onicode_desktop_proto::input::KeyCode;

/// Virtual key code for `code`, or `None` when macOS has no keyboard event for it (media and
/// browser keys are system-defined events, not key presses).
#[must_use]
pub const fn virtual_key(code: KeyCode) -> Option<CGKeyCode> {
    code.to_mac_vk()
}

/// Keys that only change modifier state; they post as `FlagsChanged`, not `KeyDown`.
#[must_use]
pub const fn is_modifier(code: KeyCode) -> bool {
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
            | KeyCode::CapsLock
            | KeyCode::Fn
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_have_codes() {
        for &code in KeyCode::ALL {
            if is_modifier(code) {
                assert!(virtual_key(code).is_some(), "{code:?}");
            }
        }
    }
}
