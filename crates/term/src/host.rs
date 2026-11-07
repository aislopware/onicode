//! The engine's terminal for one PTY: the only one that answers the program.
//!
//! Every byte the PTY writes goes through [`Host::feed`], under the lock that also encodes
//! snapshots, so a snapshot always covers exactly the output published before it. Replies to the
//! program's queries (DA, DSR, OSC 4/10/11 colours, size reports) collect as [`Effects`] for the
//! PTY, and OSC 52 copies for the viewers' clipboards. Viewers' keys, wheel and paste are encoded
//! here from the modes at that instant, so a mode change can never race a key in flight.
//!
//! The host alone registers for OSC 7501, so only it answers a program's support query, and it
//! keeps the program's records ([`crate::program`]) through prompts, resets and the shell's exit.
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use libghostty_vt::Terminal;
use libghostty_vt::key::{self, Mods, OptionAsAlt};
use libghostty_vt::screen::Screen;
use libghostty_vt::terminal::{
    Mode, ProgramStatusKind, ProgramStatusState, SemanticPromptKind, SizeReportSize,
};
use onicode_protocol::terminal::{
    Input, InputKind, Key, ProgramState, ProgramStatus, TerminalColors, TerminalSize,
};

use crate::program::{self, Change, Programs, Report};
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
    /// The program's status changed (OSC 7501): its most urgent record now, or none.
    pub status: Option<Option<ProgramStatus>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The engine's terminal for one PTY.
pub struct Host {
    term: Terminal<'static, 'static>,
    effects: Arc<Mutex<Effects>>,
    programs: Arc<Mutex<Programs>>,
    /// The status the last effects reported.
    reported: Option<ProgramStatus>,
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
        let programs = Arc::new(Mutex::new(Programs::default()));
        {
            let programs = programs.clone();
            let _ = term.on_program_status(move |_, report| {
                let Ok(state) = report.state() else {
                    return;
                };
                let id = String::from_utf8_lossy(report.id()).into_owned();
                let state = match state {
                    ProgramStatusState::Clear => {
                        lock(&programs).apply(Change::Clear(id));
                        return;
                    }
                    ProgramStatusState::Idle => ProgramState::Idle,
                    ProgramStatusState::Working => ProgramState::Working,
                    ProgramStatusState::Done => ProgramState::Done,
                    ProgramStatusState::Blocked => ProgramState::Blocked,
                    ProgramStatusState::Error => ProgramState::Error,
                    _ => return,
                };
                let kind = match report.kind() {
                    Ok(ProgramStatusKind::Permission) => Some(InputKind::Permission),
                    Ok(ProgramStatusKind::Question) => Some(InputKind::Question),
                    Ok(ProgramStatusKind::Auth) => Some(InputKind::Auth),
                    _ => None,
                };
                lock(&programs).apply(Change::Report(Report {
                    id,
                    state,
                    kind,
                    progress: report.progress(),
                    app: program::clean(report.app()),
                    title: program::clean(report.title()),
                    message: program::clean(report.message()),
                }));
            });
        }
        {
            let programs = programs.clone();
            let _ = term.on_semantic_prompt(move |_, event| {
                if matches!(event.kind(), Ok(SemanticPromptKind::PromptStart)) {
                    lock(&programs).apply(Change::PromptStart);
                }
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
            programs,
            reported: None,
            size,
        };
        host.resize(size);
        host
    }

    /// PTY output, or a note the engine writes into the grid.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
    }

    /// Replies, copies and a status change since the last call.
    pub fn take_effects(&mut self) -> Effects {
        let mut effects = std::mem::take(&mut *lock(&self.effects));
        let status = {
            let mut programs = lock(&self.programs);
            programs.take_changed().then(|| programs.status())
        };
        if let Some(status) = status.filter(|status| *status != self.reported) {
            self.reported.clone_from(&status);
            effects.status = Some(status);
        }
        effects
    }

    /// The program's most urgent record now.
    #[must_use]
    pub fn status(&self) -> Option<ProgramStatus> {
        lock(&self.programs).status()
    }

    /// The shell ended: its program's live records go, as at a new prompt. The change shows in
    /// the next [`Self::take_effects`].
    pub fn exited(&mut self) {
        lock(&self.programs).apply(Change::Exit);
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

    /// The PTY's size.
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

    #[test]
    fn the_host_answers_the_program_status_query_and_reports_changes_once() {
        let mut host = host();
        host.feed(b"\x1b]7501;?\x07");
        let reply = host.take_effects().pty;
        assert!(reply.starts_with(b"\x1b]7501;?"), "{reply:?}");
        let report = b"\x1b]7501;state=blocked:kind=permission:app=codex:msg=aGk=\x1b\\";
        host.feed(report);
        let status = host
            .take_effects()
            .status
            .expect("a change")
            .expect("a record");
        assert_eq!(
            (
                status.state,
                status.kind,
                status.app.as_deref(),
                status.message.as_deref()
            ),
            (
                ProgramState::Blocked,
                Some(InputKind::Permission),
                Some("codex"),
                Some("hi")
            )
        );
        host.feed(report);
        assert_eq!(
            host.take_effects().status,
            None,
            "the same record again is no change"
        );
        host.exited();
        assert_eq!(host.take_effects().status, Some(None));
    }

    mod program_status {
        //! OSC 7501 through the host's parser, chunked anywhere, against a reference model of
        //! the records' lifecycle.
        use std::collections::BTreeMap;

        use proptest::prelude::*;

        use super::*;

        const IDS: [&str; 5] = ["", "a", "a/b", "ab", "b"];
        const STATES: [(ProgramState, &str); 5] = [
            (ProgramState::Idle, "idle"),
            (ProgramState::Working, "working"),
            (ProgramState::Done, "done"),
            (ProgramState::Blocked, "blocked"),
            (ProgramState::Error, "error"),
        ];

        #[derive(Clone, Debug)]
        enum Op {
            Report {
                id: usize,
                state: usize,
                app: bool,
                msg: bool,
            },
            Clear(usize),
            Prompt,
            Reset,
            Exit,
        }

        impl Op {
            /// What the program writes for it; `None` for the shell's exit.
            fn bytes(&self) -> Option<Vec<u8>> {
                let text = match self {
                    Self::Report {
                        id,
                        state,
                        app,
                        msg,
                    } => {
                        let (state, name) = STATES[*state];
                        let mut body = format!("state={name}");
                        if !IDS[*id].is_empty() {
                            body += &format!(":id={}", IDS[*id]);
                        }
                        match state {
                            ProgramState::Blocked => body += ":kind=question",
                            ProgramState::Working => body += ":progress=40",
                            _ => {}
                        }
                        if *app {
                            body += ":app=codex";
                        }
                        if *msg {
                            body += ":msg=aGk=";
                        }
                        format!("\x1b]7501;{body}\x1b\\")
                    }
                    Self::Clear(id) if IDS[*id].is_empty() => "\x1b]7501;state=clear\x07".into(),
                    Self::Clear(id) => format!("\x1b]7501;state=clear:id={}\x07", IDS[*id]),
                    Self::Prompt => "\x1b]133;A\x07".into(),
                    Self::Reset => "\x1bc".into(),
                    Self::Exit => return None,
                };
                Some(text.into_bytes())
            }
        }

        fn op() -> impl Strategy<Value = Op> {
            prop_oneof![
                6 => (0..IDS.len(), 0..STATES.len(), any::<bool>(), any::<bool>())
                    .prop_map(|(id, state, app, msg)| Op::Report { id, state, app, msg }),
                2 => (0..IDS.len()).prop_map(Op::Clear),
                1 => Just(Op::Prompt),
                1 => Just(Op::Reset),
                1 => Just(Op::Exit),
            ]
        }

        /// The specification's records, kept the plainest way.
        #[derive(Default)]
        struct Model {
            records: BTreeMap<String, (ProgramState, bool, bool, u64)>,
            clock: u64,
        }

        impl Model {
            fn apply(&mut self, op: &Op) {
                match op {
                    Op::Report {
                        id,
                        state,
                        app,
                        msg,
                    } => {
                        self.clock += 1;
                        self.records.insert(
                            IDS[*id].to_owned(),
                            (STATES[*state].0, *app, *msg, self.clock),
                        );
                    }
                    Op::Clear(id) => {
                        let id = IDS[*id];
                        self.records.retain(|key, _| {
                            !(id.is_empty() || key == id || key.starts_with(&format!("{id}/")))
                        });
                    }
                    Op::Prompt | Op::Exit => self.records.retain(|_, (state, ..)| {
                        matches!(state, ProgramState::Done | ProgramState::Error)
                    }),
                    Op::Reset => self.records.clear(),
                }
            }

            fn status(&self) -> Option<ProgramStatus> {
                let rank = |state: ProgramState| {
                    [
                        ProgramState::Blocked,
                        ProgramState::Working,
                        ProgramState::Error,
                        ProgramState::Done,
                        ProgramState::Idle,
                    ]
                    .iter()
                    .position(|s| *s == state)
                };
                let (id, (state, app, msg, _)) = self
                    .records
                    .iter()
                    .min_by_key(|(id, r)| (rank(r.0), std::cmp::Reverse(r.3), (*id).clone()))?;
                let mut app = *app;
                let mut parent = id.as_str();
                while !app && !parent.is_empty() {
                    parent = parent.rsplit_once('/').map_or("", |(p, _)| p);
                    app = self.records.get(parent).is_some_and(|r| r.1);
                }
                Some(ProgramStatus {
                    state: *state,
                    kind: (*state == ProgramState::Blocked).then_some(InputKind::Question),
                    progress: (*state == ProgramState::Working).then_some(40),
                    app: app.then(|| "codex".to_owned()),
                    title: None,
                    message: msg.then(|| "hi".to_owned()),
                })
            }
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]

            /// The program's reports, cut into chunks anywhere (inside a report, between its
            /// ESC and backslash), leave the host where the reference model says.
            #[test]
            fn reports_split_anywhere_follow_the_lifecycle(
                ops in proptest::collection::vec(op(), 0..40),
                cuts in proptest::collection::vec(any::<prop::sample::Index>(), 0..8),
            ) {
                let mut host = host();
                let mut model = Model::default();
                let mut pending = Vec::new();
                let flush = |host: &mut Host, pending: &mut Vec<u8>| {
                    let mut at: Vec<usize> =
                        cuts.iter().map(|cut| cut.index(pending.len() + 1)).collect();
                    at.push(0);
                    at.push(pending.len());
                    at.sort_unstable();
                    for pair in at.windows(2) {
                        host.feed(&pending[pair[0]..pair[1]]);
                    }
                    pending.clear();
                };
                for op in &ops {
                    model.apply(op);
                    match op.bytes() {
                        Some(bytes) => pending.extend(bytes),
                        None => {
                            flush(&mut host, &mut pending);
                            host.exited();
                        }
                    }
                }
                flush(&mut host, &mut pending);
                prop_assert_eq!(host.status(), model.status());
                let _ = host.take_effects();
                prop_assert_eq!(host.take_effects().status, None);
            }
        }
    }
}
