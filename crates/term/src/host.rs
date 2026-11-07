//! The engine's terminal for one PTY: the only one that answers the program.
//!
//! Every byte the PTY writes goes through [`Host::feed`], under the lock that also encodes
//! snapshots, so a snapshot always covers exactly the output published before it. Replies to the
//! program's queries (DA, DSR, OSC 4/10/11 colours, size reports) collect as [`Effects`] for the
//! PTY, and OSC 52 copies for the viewers' clipboards. Viewers' keys, wheel and paste are encoded
//! here from the modes at that instant, so a mode change can never race a key in flight.
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use libghostty_vt::Terminal;
use libghostty_vt::key::{self, Mods, OptionAsAlt};
use libghostty_vt::screen::Screen;
use libghostty_vt::terminal::{Mode, SizeReportSize};
use onicode_protocol::terminal::{Input, Key, TerminalColors, TerminalSize};

use crate::{keys, palette};

/// The longest unfinished escape sequence a snapshot can carry. A snapshot taken inside a longer
/// one (a huge image upload) waits for the next feed.
const MAX_CONTINUATION_BYTES: usize = 16 << 20;

/// Replies the program can queue between drains. A program flooding queries loses the rest
/// instead of growing this.
const MAX_PENDING_PTY: usize = 64 << 10;

/// Wheel notches one input may carry.
const MAX_WHEEL_LINES: u32 = 1000;

/// What the terminal did besides drawing, since the last [`Host::take_effects`].
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Effects {
    /// Replies for the PTY.
    pub pty: Vec<u8>,
    /// Text the program copied (OSC 52), for the viewers' clipboards.
    pub clipboard: Vec<String>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct Host {
    term: Terminal<'static, 'static>,
    effects: Arc<Mutex<Effects>>,
    size: TerminalSize,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl Host {
    /// A terminal for a PTY of `size`, running as `terminfo` (which XTGETTCAP reports). `colors`
    /// answer the program's OSC 10/11/12 queries: the colours the viewers paint.
    #[must_use]
    pub fn new(size: TerminalSize, colors: TerminalColors, terminfo: &str) -> Self {
        let mut term =
            Terminal::new(size.cols.max(2), size.rows.max(1)).unwrap_or_else(|_| crate::oom());
        crate::configure(&mut term);
        // Snapshots may land inside an escape sequence.
        let _ = term.set_continuation_max_bytes(MAX_CONTINUATION_BYTES);
        let _ = term.set_terminfo_name(terminfo);
        let _ = term.set_default_fg_color(Some(palette::rgb(colors.foreground)));
        let _ = term.set_default_bg_color(Some(palette::rgb(colors.background)));
        let _ = term.set_default_cursor_color(Some(palette::rgb(colors.cursor)));
        let effects = Arc::new(Mutex::new(Effects::default()));
        // Callbacks run inside libghostty, where a panic aborts: none of these can panic.
        {
            let effects = effects.clone();
            let _ = term.on_pty_write(move |_, bytes| {
                let mut effects = lock(&effects);
                if effects.pty.len() + bytes.len() <= MAX_PENDING_PTY {
                    effects.pty.extend_from_slice(bytes);
                }
            });
        }
        {
            let effects = effects.clone();
            // Every destination lands on the one clipboard, as monocode's did; reads stay
            // unanswered (copy only).
            let _ = term.on_clipboard_write(move |_, write| {
                let text = write
                    .contents()
                    .find(|content| content.mime.starts_with(b"text/plain"))
                    .map(|content| String::from_utf8_lossy(content.data).into_owned());
                if let Some(text) = text.filter(|text| !text.is_empty()) {
                    lock(&effects).clipboard.push(text);
                }
                write.reply(Ok(()), false);
            });
        }
        let _ = term.on_size(|term| {
            let rows = term.rows().ok()?;
            let columns = term.cols().ok()?;
            Some(SizeReportSize {
                rows,
                columns,
                cell_width: term.width_px().ok()?.checked_div(u32::from(columns))?,
                cell_height: term.height_px().ok()?.checked_div(u32::from(rows))?,
            })
        });
        let mut host = Self {
            term,
            effects,
            size,
        };
        host.resize(size);
        host
    }

    /// PTY output, or a note the engine writes into the grid.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
    }

    /// Replies and copies since the last call.
    pub fn take_effects(&mut self) -> Effects {
        std::mem::take(&mut *lock(&self.effects))
    }

    /// The PTY took a new size.
    pub fn resize(&mut self, size: TerminalSize) {
        self.size = size;
        let _ = self.term.resize(
            size.cols.max(2),
            size.rows.max(1),
            u32::from(size.cell_width.max(1)),
            u32::from(size.cell_height.max(1)),
        );
    }

    #[must_use]
    pub fn size(&self) -> TerminalSize {
        self.size
    }

    /// The terminal as of the last feed, for a viewer to start from. `None` inside an escape
    /// sequence longer than the continuation limit; the next feed usually ends it.
    pub fn snapshot(&mut self) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        self.term.encode_snapshot(&mut bytes).ok()?;
        Some(bytes)
    }

    fn mode(&self, mode: Mode) -> bool {
        self.term.mode(mode).unwrap_or(false)
    }

    /// A viewer's input as PTY bytes, from the terminal's modes now.
    #[must_use]
    pub fn input(&self, input: &Input) -> Vec<u8> {
        match input {
            Input::Key(key) => self.key(key),
            Input::Text(text) => text.as_bytes().to_vec(),
            Input::Paste(text) => keys::paste(text, self.mode(Mode::BRACKETED_PASTE)),
            Input::Wheel {
                up,
                lines,
                row,
                col,
            } => self.wheel(*up, *lines, *row, *col),
        }
    }

    /// xterm's legacy keys, as monocode sent them, until a program pushes Kitty keyboard flags;
    /// then libghostty's encoder, except for the macOS editing shortcuts readline knows.
    fn key(&self, key: &Key) -> Vec<u8> {
        let kitty = self
            .term
            .kitty_keyboard_flags()
            .is_ok_and(|flags| !flags.is_empty());
        if kitty
            && !keys::editing_shortcut(key)
            && let Some(bytes) = self.kitty_key(key)
        {
            return bytes;
        }
        keys::legacy(key, self.mode(Mode::DECCKM)).unwrap_or_default()
    }

    fn kitty_key(&self, key: &Key) -> Option<Vec<u8>> {
        let code = keys::code(&key.key)?;
        let mut encoder = key::Encoder::new().ok()?;
        let mut event = key::Event::new().ok()?;
        // Option is Meta, as monocode had it: Option-a is Alt-a, not `å`.
        encoder
            .set_options_from_terminal(&self.term)
            .set_macos_option_as_alt(OptionAsAlt::True);
        let mut mods = Mods::empty();
        mods.set(Mods::SHIFT, key.shift);
        mods.set(Mods::ALT, key.alt);
        mods.set(Mods::CTRL, key.control);
        mods.set(Mods::SUPER, key.platform);
        let unshifted = unshifted(&key.key);
        let text = if key.alt {
            unshifted.map(String::from)
        } else {
            key.key_char
                .clone()
                .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        };
        event
            .set_action(key::Action::Press)
            .set_key(code)
            .set_mods(mods)
            .set_utf8(text);
        if let Some(unshifted) = unshifted {
            event.set_unshifted_codepoint(unshifted);
        }
        let mut out = Vec::new();
        encoder.encode_to_vec(&event, &mut out).ok()?;
        (!out.is_empty()).then_some(out)
    }

    /// A program tracking the mouse gets the wheel as reports; a full-screen program that asked
    /// for it (1007) gets arrow keys; the shell gets nothing, since the viewer scrolls its own
    /// history.
    fn wheel(&self, up: bool, lines: u32, row: u16, col: u16) -> Vec<u8> {
        let lines = usize::try_from(lines.min(MAX_WHEEL_LINES)).unwrap_or(0);
        let tracking = self.mode(Mode::NORMAL_MOUSE)
            || self.mode(Mode::BUTTON_MOUSE)
            || self.mode(Mode::ANY_MOUSE);
        if tracking {
            let sgr = self.mode(Mode::SGR_MOUSE);
            keys::wheel_report(up, usize::from(row), usize::from(col), sgr).repeat(lines)
        } else if self.alt_screen() && self.mode(Mode::ALT_SCROLL) {
            keys::wheel_arrow(up, self.mode(Mode::DECCKM)).repeat(lines)
        } else {
            Vec::new()
        }
    }

    fn alt_screen(&self) -> bool {
        self.term
            .active_screen()
            .is_ok_and(|screen| screen == Screen::Alternate)
    }
}

/// The character a key types without modifiers, which Kitty reports keys by.
fn unshifted(name: &str) -> Option<char> {
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => (name == "space").then_some(' '),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: TerminalSize = TerminalSize {
        cols: 20,
        rows: 4,
        cell_width: 8,
        cell_height: 16,
    };
    const COLORS: TerminalColors = TerminalColors {
        foreground: 0xebebeb,
        background: 0x171717,
        cursor: 0x4da3f5,
    };

    fn host() -> Host {
        Host::new(SIZE, COLORS, "xterm-ghostty")
    }

    fn key(name: &str, f: impl FnOnce(&mut Key)) -> Input {
        let mut key = Key {
            key: name.to_owned(),
            ..Key::default()
        };
        f(&mut key);
        Input::Key(key)
    }

    #[test]
    fn the_host_answers_queries() {
        let mut host = host();
        host.feed(b"ab\x1b[6n");
        assert_eq!(host.take_effects().pty, b"\x1b[1;3R");
        host.feed(b"\x1b]10;?\x1b\\");
        let reply = String::from_utf8(host.take_effects().pty).unwrap();
        assert!(reply.starts_with("\x1b]10;rgb:ebeb/ebeb/ebeb"), "{reply:?}");
        host.feed(b"\x1b[14t");
        let reply = String::from_utf8(host.take_effects().pty).unwrap();
        assert_eq!(reply, "\x1b[4;64;160t");
        assert_eq!(host.take_effects(), Effects::default());
    }

    #[test]
    fn osc_52_copies_text() {
        let mut host = host();
        host.feed(b"\x1b]52;c;aGk=\x07");
        assert_eq!(host.take_effects().clipboard, ["hi"]);
    }

    #[test]
    fn paste_brackets_when_asked() {
        let mut host = host();
        let paste = Input::Paste("a\nb".to_owned());
        assert_eq!(host.input(&paste), b"a\rb");
        host.feed(b"\x1b[?2004h");
        assert_eq!(host.input(&paste), b"\x1b[200~a\nb\x1b[201~");
    }

    #[test]
    fn the_wheel_follows_the_programs_modes() {
        let mut host = host();
        let wheel = |up| Input::Wheel {
            up,
            lines: 2,
            row: 1,
            col: 3,
        };
        assert_eq!(host.input(&wheel(true)), b"");
        host.feed(b"\x1b[?1049h");
        assert_eq!(host.input(&wheel(true)), b"\x1b[A\x1b[A");
        host.feed(b"\x1b[?1h");
        assert_eq!(host.input(&wheel(false)), b"\x1bOB\x1bOB");
        host.feed(b"\x1b[?1000h\x1b[?1006h");
        assert_eq!(host.input(&wheel(true)), b"\x1b[<64;4;2M\x1b[<64;4;2M");
    }

    #[test]
    fn kitty_flags_switch_keys_to_libghostty_but_keep_editing_shortcuts() {
        let mut host = host();
        let ctrl_a = key("a", |k| k.control = true);
        assert_eq!(host.input(&ctrl_a), b"\x01");
        host.feed(b"\x1b[>1u");
        assert_eq!(host.input(&ctrl_a), b"\x1b[97;5u");
        assert_eq!(host.input(&key("left", |k| k.platform = true)), b"\x01");
        assert_eq!(host.input(&key("left", |k| k.alt = true)), b"\x1bb");
        host.feed(b"\x1b[<u");
        assert_eq!(host.input(&key("up", |_| ())), b"\x1b[A");
    }
}
