//! What the viewers' input becomes on the PTY, as monocode's terminal encoded it: xterm keys with
//! terminalKeys.ts's macOS editing shortcuts and Option as Meta, X10 or SGR wheel reports, and
//! bracketed paste. The host runs these against its own modes; a viewer runs [`handles`] to know
//! which keys are the terminal's.
use libghostty_vt::key::Key as Code;
use onicode_protocol::terminal::Key;

/// Whether a keystroke is the terminal's, rather than one the app's bindings get.
#[must_use]
pub fn handles(key: &Key) -> bool {
    legacy(key, false).is_some()
}

/// One of terminalKeys.ts's macOS editing shortcuts, which readline understands whatever the
/// program asked for.
#[must_use]
pub fn editing_shortcut(key: &Key) -> bool {
    let plain = !key.control && !key.shift;
    (key.platform
        && plain
        && !key.alt
        && matches!(key.key.as_str(), "left" | "right" | "backspace"))
        || (key.alt && plain && !key.platform && matches!(key.key.as_str(), "left" | "right"))
}

/// Bytes a keystroke sends with xterm's legacy encoding, or `None` to leave it to the app's
/// bindings. `app_cursor` is DECCKM.
#[must_use]
pub fn legacy(key: &Key, app_cursor: bool) -> Option<Vec<u8>> {
    let name = key.key.as_str();
    if key.platform {
        if key.control || key.shift || key.alt {
            return None;
        }
        return match name {
            "left" => Some(b"\x01".to_vec()),
            "right" => Some(b"\x05".to_vec()),
            "backspace" => Some(b"\x15".to_vec()),
            _ => None,
        };
    }
    if key.alt && !key.control && !key.shift {
        match name {
            "left" => return Some(b"\x1bb".to_vec()),
            "right" => return Some(b"\x1bf".to_vec()),
            _ => {}
        }
    }
    let modifier = 1 + u8::from(key.shift) + 2 * u8::from(key.alt) + 4 * u8::from(key.control);
    let cursor = |final_byte: char| {
        Some(if modifier > 1 {
            format!("\x1b[1;{modifier}{final_byte}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        })
    };
    let tilde = |code: u8| {
        Some(if modifier > 1 {
            format!("\x1b[{code};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        })
    };
    let function = |final_byte: char| {
        Some(if modifier > 1 {
            format!("\x1b[1;{modifier}{final_byte}").into_bytes()
        } else {
            format!("\x1bO{final_byte}").into_bytes()
        })
    };
    let meta = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if key.alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        Some(out)
    };
    match name {
        "up" => return cursor('A'),
        "down" => return cursor('B'),
        "right" => return cursor('C'),
        "left" => return cursor('D'),
        "home" => return cursor('H'),
        "end" => return cursor('F'),
        "insert" => return tilde(2),
        "delete" => return tilde(3),
        "pageup" => return tilde(5),
        "pagedown" => return tilde(6),
        "f1" => return function('P'),
        "f2" => return function('Q'),
        "f3" => return function('R'),
        "f4" => return function('S'),
        "f5" => return tilde(15),
        "f6" => return tilde(17),
        "f7" => return tilde(18),
        "f8" => return tilde(19),
        "f9" => return tilde(20),
        "f10" => return tilde(21),
        "f11" => return tilde(23),
        "f12" => return tilde(24),
        "enter" => return meta(b"\r"),
        "escape" => return meta(b"\x1b"),
        "tab" if key.shift => return Some(b"\x1b[Z".to_vec()),
        "tab" => return meta(b"\t"),
        "backspace" if key.control => return meta(b"\x08"),
        "backspace" => return meta(b"\x7f"),
        _ => {}
    }
    if key.control {
        let ch = if name == "space" {
            ' '
        } else {
            let mut chars = name.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            ch
        };
        let code = match ch.to_ascii_lowercase() {
            c @ 'a'..='z' => c as u8 - b'a' + 1,
            ' ' | '@' | '2' => 0,
            '[' | '3' => 27,
            '\\' | '4' => 28,
            ']' | '5' => 29,
            '^' | '6' => 30,
            '_' | '-' | '/' | '7' => 31,
            '8' => 127,
            _ => return None,
        };
        return meta(&[code]);
    }
    // Option is Meta (`macOptionIsMeta`): the key itself, after ESC.
    if key.alt {
        let text = if name == "space" {
            " ".to_owned()
        } else if key.shift {
            name.to_uppercase()
        } else {
            name.to_owned()
        };
        if text.chars().count() == 1 {
            return meta(text.as_bytes());
        }
    }
    let text = key.key_char.as_deref().or(match name {
        "space" => Some(" "),
        _ => None,
    })?;
    Some(text.as_bytes().to_vec())
}

/// libghostty's code for a key the viewer named, for the Kitty keyboard protocol.
#[must_use]
pub fn code(name: &str) -> Option<Code> {
    Some(match name {
        "a" => Code::A,
        "b" => Code::B,
        "c" => Code::C,
        "d" => Code::D,
        "e" => Code::E,
        "f" => Code::F,
        "g" => Code::G,
        "h" => Code::H,
        "i" => Code::I,
        "j" => Code::J,
        "k" => Code::K,
        "l" => Code::L,
        "m" => Code::M,
        "n" => Code::N,
        "o" => Code::O,
        "p" => Code::P,
        "q" => Code::Q,
        "r" => Code::R,
        "s" => Code::S,
        "t" => Code::T,
        "u" => Code::U,
        "v" => Code::V,
        "w" => Code::W,
        "x" => Code::X,
        "y" => Code::Y,
        "z" => Code::Z,
        "0" => Code::Digit0,
        "1" => Code::Digit1,
        "2" => Code::Digit2,
        "3" => Code::Digit3,
        "4" => Code::Digit4,
        "5" => Code::Digit5,
        "6" => Code::Digit6,
        "7" => Code::Digit7,
        "8" => Code::Digit8,
        "9" => Code::Digit9,
        "`" => Code::Backquote,
        "\\" => Code::Backslash,
        "[" => Code::BracketLeft,
        "]" => Code::BracketRight,
        "," => Code::Comma,
        "=" => Code::Equal,
        "-" => Code::Minus,
        "." => Code::Period,
        "'" => Code::Quote,
        ";" => Code::Semicolon,
        "/" => Code::Slash,
        "space" => Code::Space,
        "enter" => Code::Enter,
        "tab" => Code::Tab,
        "backspace" => Code::Backspace,
        "escape" => Code::Escape,
        "delete" => Code::Delete,
        "insert" => Code::Insert,
        "home" => Code::Home,
        "end" => Code::End,
        "pageup" => Code::PageUp,
        "pagedown" => Code::PageDown,
        "up" => Code::ArrowUp,
        "down" => Code::ArrowDown,
        "left" => Code::ArrowLeft,
        "right" => Code::ArrowRight,
        "f1" => Code::F1,
        "f2" => Code::F2,
        "f3" => Code::F3,
        "f4" => Code::F4,
        "f5" => Code::F5,
        "f6" => Code::F6,
        "f7" => Code::F7,
        "f8" => Code::F8,
        "f9" => Code::F9,
        "f10" => Code::F10,
        "f11" => Code::F11,
        "f12" => Code::F12,
        _ => return None,
    })
}

/// One wheel notch as a mouse report at a zero-based cell, SGR when asked for, else the legacy
/// X10 encoding (which cannot address past column or line 223).
#[must_use]
pub fn wheel_report(up: bool, row: usize, col: usize, sgr: bool) -> Vec<u8> {
    let button: u8 = if up { 64 } else { 65 };
    if sgr {
        format!("\x1b[<{button};{};{}M", col + 1, row + 1).into_bytes()
    } else {
        let cell = |n: usize| u8::try_from(n.min(222) + 33).unwrap_or(255);
        vec![0x1b, b'[', b'M', button + 32, cell(col), cell(row)]
    }
}

/// One wheel notch as an arrow key, for a full-screen program that scrolls itself.
#[must_use]
pub fn wheel_arrow(up: bool, app_cursor: bool) -> &'static [u8] {
    match (up, app_cursor) {
        (true, true) => b"\x1bOA",
        (true, false) => b"\x1b[A",
        (false, true) => b"\x1bOB",
        (false, false) => b"\x1b[B",
    }
}

/// Pasted text: bracketed when the program asked for it (a pasted end marker must not end the
/// paste early), else with every line ending as a carriage return.
#[must_use]
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let text = text.replace("\x1b[201~", "");
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// A keystroke as gpui names it: `cmd-shift-d`, `alt-left`, `a`.
    fn key(spec: &str) -> Key {
        let mut parts: Vec<&str> = spec.split('-').collect();
        // `ctrl--` names the minus key.
        let name = if spec.ends_with("--") {
            parts.truncate(parts.len().saturating_sub(2));
            "-"
        } else {
            parts.pop().unwrap_or_default()
        };
        let mut key = Key {
            key: name.to_owned(),
            ..Key::default()
        };
        for modifier in parts {
            match modifier {
                "shift" => key.shift = true,
                "alt" => key.alt = true,
                "ctrl" => key.control = true,
                "cmd" => key.platform = true,
                other => panic!("{other}"),
            }
        }
        key
    }

    fn bytes(spec: &str) -> Option<Vec<u8>> {
        legacy(&key(spec), false)
    }

    #[test]
    fn keys_encode_like_xterm_with_the_macos_editing_shortcuts() {
        assert_eq!(bytes("enter"), Some(b"\r".to_vec()));
        assert_eq!(bytes("backspace"), Some(b"\x7f".to_vec()));
        assert_eq!(bytes("up"), Some(b"\x1b[A".to_vec()));
        assert_eq!(legacy(&key("up"), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(bytes("shift-right"), Some(b"\x1b[1;2C".to_vec()));
        assert_eq!(bytes("ctrl-c"), Some(vec![3]));
        assert_eq!(bytes("ctrl-["), Some(vec![27]));
        assert_eq!(bytes("pagedown"), Some(b"\x1b[6~".to_vec()));
        assert_eq!(bytes("f5"), Some(b"\x1b[15~".to_vec()));
        assert_eq!(bytes("shift-tab"), Some(b"\x1b[Z".to_vec()));
        // terminalKeys.ts
        assert_eq!(bytes("alt-left"), Some(b"\x1bb".to_vec()));
        assert_eq!(bytes("alt-right"), Some(b"\x1bf".to_vec()));
        assert_eq!(bytes("cmd-left"), Some(b"\x01".to_vec()));
        assert_eq!(bytes("cmd-right"), Some(b"\x05".to_vec()));
        assert_eq!(bytes("cmd-backspace"), Some(b"\x15".to_vec()));
        assert!(editing_shortcut(&key("cmd-left")) && editing_shortcut(&key("alt-right")));
        assert!(!editing_shortcut(&key("alt-b")));
        // Option is Meta.
        assert_eq!(bytes("alt-b"), Some(b"\x1bb".to_vec()));
        // App shortcuts stay with the app.
        assert_eq!(bytes("cmd-j"), None);
        assert_eq!(bytes("cmd-shift-d"), None);
        assert!(!handles(&key("cmd-j")));
        let typed = Key {
            key: "a".into(),
            key_char: Some("a".into()),
            ..Key::default()
        };
        assert_eq!(legacy(&typed, false), Some(b"a".to_vec()));
    }

    #[test]
    fn wheel_reports_match_the_requested_encoding() {
        assert_eq!(wheel_report(true, 4, 9, true), b"\x1b[<64;10;5M");
        assert_eq!(wheel_report(false, 0, 0, true), b"\x1b[<65;1;1M");
        assert_eq!(
            wheel_report(true, 4, 9, false),
            [0x1b, b'[', b'M', 96, 42, 37]
        );
        assert_eq!(wheel_report(true, 900, 900, false)[4..], [255, 255]);
        assert_eq!(wheel_arrow(true, true), b"\x1bOA");
    }

    #[test]
    fn paste_is_bracketed_when_the_program_asks() {
        assert_eq!(paste("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(paste("a\n\x1b[201~b", true), b"\x1b[200~a\nb\x1b[201~");
    }

    fn modifiers() -> impl Strategy<Value = [bool; 4]> {
        any::<[bool; 4]>()
    }

    proptest! {
        /// Only cursor keys read DECCKM, and only without modifiers: every other key encodes the
        /// same in either mode, and a handled key stays handled.
        #[test]
        fn app_cursor_mode_moves_only_bare_cursor_keys(
            name in prop::sample::select(vec![
                "a", "z", "1", "-", "[", "space", "enter", "escape", "tab", "backspace", "up",
                "down", "left", "right", "home", "end", "insert", "delete", "pageup", "pagedown",
                "f1", "f4", "f5", "f12", "@",
            ]),
            mods in modifiers(),
        ) {
            let key = Key {
                key: name.to_owned(),
                key_char: (name.chars().count() == 1).then(|| name.to_owned()),
                shift: mods[0],
                alt: mods[1],
                control: mods[2],
                platform: mods[3],
            };
            let normal = legacy(&key, false);
            let app = legacy(&key, true);
            prop_assert_eq!(normal.is_some(), app.is_some());
            let cursor_key = matches!(name, "up" | "down" | "left" | "right" | "home" | "end");
            let bare = !key.shift && !key.alt && !key.control && !key.platform;
            if cursor_key && bare {
                let (normal, app) = (normal.unwrap_or_default(), app.unwrap_or_default());
                prop_assert_eq!(&normal[..2], b"\x1b[");
                prop_assert_eq!(&app[..2], b"\x1bO");
                prop_assert_eq!(normal[2..].to_vec(), app[2..].to_vec());
            } else {
                prop_assert_eq!(normal, app);
            }
            if !key.platform {
                prop_assert!(code(name).is_some() || name == "@");
            }
        }
    }
}
