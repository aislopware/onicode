//! The Terminal group (`docs/onicode/decisions/terminal.md`, engine.md step 7): the engine hosts
//! every terminal, a PTY whose output runs through the engine's own libghostty terminal, which
//! answers the program's queries and encodes the viewers' input from its modes.
//!
//! A viewer opens a terminal under a [`TerminalId`] it minted ([`OpenTerminal`]) and watches it
//! ([`WatchTerminal`]): a libghostty snapshot of the screen, its scrollback right behind it, then
//! the PTY's output byte for byte from there, which the viewer feeds to a mirror of its own with
//! its replies to the program turned off. The cursor's `seq` numbers the stream's events, so a
//! viewer that comes back while the engine's ring still holds what it missed gets those bytes
//! instead of a new snapshot. Keys, paste and wheel go to the engine as [`TerminalInput`] calls,
//! in order, and the engine encodes them from its terminal's modes at that instant.
//!
//! Every call that sizes or types names its [`ViewerId`]: the PTY takes the size of the viewer
//! that typed last, and a resize answers with the size the PTY has after it, so a viewer knows
//! whether its own size won. A program's OSC 7501 reports reach viewers as a [`ProgramStatus`],
//! display evidence only: a terminal's report never resolves an approval or finishes a native
//! turn.
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use crate::files::Bytes;
use crate::{Call, Cursor, Outcome, TerminalId, Watch};

/// One viewer of an engine's terminals: an app run, another Mac, the iPhone. UUIDv7, minted by
/// the viewer once per run, so the engine can tell two viewers of one terminal apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ViewerId(pub Uuid);

impl ViewerId {
    /// A fresh id. UUIDv7, so ids sort by when they were minted.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl Default for ViewerId {
    /// A fresh id, as [`Self::new`].
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ViewerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// The grid a viewer paints, in cells, and its cell size in pixels, which programs that ask in
/// pixels are told.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

/// The colours a program that asks (OSC 10, 11 and 12) is told, as `0xRRGGBB`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TerminalColors {
    pub foreground: u32,
    pub background: u32,
    pub cursor: u32,
}

/// A login shell in `cwd` on a new PTY of the opening viewer's `size`, under the viewer's
/// `terminal` id. Opening an id the engine already hosts answers its summary and opens nothing. A
/// shell that cannot start leaves a terminal that says why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTerminal {
    pub terminal: TerminalId,
    pub viewer: ViewerId,
    pub cwd: String,
    pub size: TerminalSize,
    pub colors: TerminalColors,
}

impl Call for OpenTerminal {
    type Params = Self;
    type Reply = TerminalSummary;

    const NAME: &'static str = "OpenTerminal";
}

/// One terminal the engine hosts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSummary {
    pub terminal: TerminalId,
    pub cwd: String,
    /// The shell ended; the terminal stays until it is closed.
    pub exited: bool,
    /// What the program in it last reported, the most urgent of its records.
    pub status: Option<ProgramStatus>,
}

/// Every terminal the engine hosts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTerminals {}

impl Call for ListTerminals {
    type Params = Self;
    type Reply = Vec<TerminalSummary>;

    const NAME: &'static str = "ListTerminals";
}

/// The viewer's grid. The PTY takes it when this viewer typed last, or when nobody watching has
/// typed; every watcher's mirror reflows at the same byte, through [`TerminalEvent::Resized`].
/// The reply is the PTY's size after the call: this one when the PTY took it, else the size of
/// the viewer it follows. A viewer showing its own size ahead of the host's word goes back to the
/// host's grid when the reply names another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeTerminal {
    pub terminal: TerminalId,
    pub viewer: ViewerId,
    pub size: TerminalSize,
}

impl Call for ResizeTerminal {
    type Params = Self;
    type Reply = TerminalSize;

    const NAME: &'static str = "ResizeTerminal";
}

/// Hangs up the shell and its job and forgets the terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseTerminal {
    pub terminal: TerminalId,
}

impl Call for CloseTerminal {
    type Params = Self;
    type Reply = Outcome;

    const NAME: &'static str = "CloseTerminal";
}

/// The keys of a keystroke as the viewer's keyboard named them: `key` is the key's own name
/// (`a`, `enter`, `left`, `f5`, `space`), `key_char` the text it typed, if any.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    pub key: String,
    pub key_char: Option<String>,
    pub shift: bool,
    /// Option on a Mac, which the terminal treats as Meta.
    pub alt: bool,
    pub control: bool,
    /// Command on a Mac.
    pub platform: bool,
}

/// What a viewer sends a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Input {
    /// A keystroke, encoded from the terminal's keyboard modes.
    Key(Key),
    /// Text written as it is: a command line to run, or a key the viewer binds itself.
    Text(String),
    /// Pasted text, bracketed when the program asked for it.
    Paste(String),
    /// Wheel notches over a cell of the viewer's grid, `up` into history: mouse reports when
    /// the program tracks the mouse, else arrow keys on the alternate screen.
    Wheel {
        up: bool,
        lines: u32,
        row: u16,
        col: u16,
    },
}

/// One input for a terminal. The viewer that sends it becomes the typist: the PTY takes its size
/// first, so a mouse report counts the grid that viewer sees.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInput {
    pub terminal: TerminalId,
    pub viewer: ViewerId,
    pub input: Input,
}

impl Call for TerminalInput {
    type Params = Self;
    type Reply = Outcome;

    const NAME: &'static str = "TerminalInput";
}

/// The command that holds the terminal's foreground, unless it is the shell itself: what a tab
/// is labelled with. `node vite.js` reads `vite.js`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadForeground {
    pub terminal: TerminalId,
}

impl Call for ReadForeground {
    type Params = Self;
    type Reply = Option<String>;

    const NAME: &'static str = "ReadForeground";
}

/// Hangs up the job that holds the terminal's foreground, leaving the shell at its prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopForeground {
    pub terminal: TerminalId,
}

impl Call for StopForeground {
    type Params = Self;
    type Reply = Outcome;

    const NAME: &'static str = "StopForeground";
}

/// Watch a terminal: its snapshot, then its output from there. While a viewer watches, its size
/// counts; when the typist's last watch ends, the PTY takes the size of the viewer that typed
/// before it, else of the one that sized it last.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchTerminal {
    pub terminal: TerminalId,
    pub viewer: ViewerId,
    /// The last cursor this viewer holds, to resume from.
    pub since: Option<Cursor>,
}

impl Watch for WatchTerminal {
    type Event = TerminalEvent;
    type Params = Self;
    type State = TerminalState;

    const NAME: &'static str = "WatchTerminal";
}

/// The `seq` of a snapshot item's cursor while its [`TerminalEvent::History`] is still to come.
/// No stream reaches it, so a watch resumed from it starts over with a whole new snapshot instead
/// of resuming past a history it never got; the `History` item's `last` then moves the cursor to
/// the snapshot's own number.
pub const HISTORY_PENDING: u64 = u64::MAX;

/// A terminal as a watcher starts from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalState {
    pub size: TerminalSize,
    /// libghostty's snapshot of the terminal as of the cursor, through its READY record: the
    /// screen, modes and parser state, enough to paint. The older scrollback follows at once as
    /// [`TerminalEvent::History`], so a big history never holds back the first frame, and until
    /// it has, the item's cursor holds [`HISTORY_PENDING`]. Empty when the engine could not take
    /// one (output stopped inside an escape sequence longer than libghostty keeps): the viewer
    /// starts from a blank grid, no history follows and the cursor is the snapshot's own.
    pub screen: Bytes,
    pub exited: bool,
    pub status: Option<ProgramStatus>,
}

/// What happened to a terminal, in PTY order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalEvent {
    /// Output, as the PTY wrote it. Consecutive output may arrive merged.
    Output(Bytes),
    /// The PTY took a new size here.
    Resized(TerminalSize),
    /// The program copied text (OSC 52).
    Clipboard(String),
    /// The shell ended, with its status unless a signal ended it. The note the grid shows is in
    /// the output before it.
    Exited { code: Option<i32> },
    /// The rest of the snapshot just before it, after its READY record: the older scrollback.
    /// It is not numbered: it comes alone, right after the snapshot, in an `Events` item that
    /// covers no numbers (`last == first - 1`, the snapshot's own number), and never from the
    /// ring. Its `last` makes the snapshot's cursor resumable ([`HISTORY_PENDING`]).
    History(Bytes),
    /// What the program reports changed (OSC 7501): the most urgent record now, or none.
    Status(Option<ProgramStatus>),
}

/// The most urgent of a program's OSC 7501 records (blocked, then working, error, done, idle).
/// Its text is the program's own and untrusted: free of control characters and of bidi
/// overrides, and to be shown only as plain text, beside the terminal it came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramStatus {
    pub state: ProgramState,
    /// What a blocked program needs.
    pub kind: Option<InputKind>,
    /// 0 to 100 while working or blocked; `None` is indeterminate.
    pub progress: Option<u8>,
    /// A stable name for the program (`claude-code`, `codex`), its own or its nearest
    /// ancestor record's.
    pub app: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
}

/// What a program says it is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProgramState {
    /// At rest at its own prompt.
    Idle,
    Working,
    /// Finished; the result is ready to look at.
    Done,
    /// Cannot go on until the user does something.
    Blocked,
    /// Failed and stopped.
    Error,
}

/// What a blocked program needs from the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputKind {
    /// Approval to do something.
    Permission,
    /// An answer to type.
    Question,
    /// A login, password or token.
    Auth,
}
