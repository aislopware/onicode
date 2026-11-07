//! The Terminal group (`docs/onicode/decisions/terminal.md`, engine.md step 7): the engine hosts
//! every terminal, a PTY whose output runs through the engine's own libghostty terminal, which
//! answers the program's queries and encodes the viewers' input from its modes.
//!
//! A viewer opens a terminal under a [`TerminalId`] it minted ([`OpenTerminal`]) and watches it
//! ([`WatchTerminal`]): a libghostty snapshot of the screen and its scrollback, then the PTY's
//! output byte for byte from there, which the viewer feeds to a mirror of its own with its replies
//! to the program turned off. The cursor's `seq` numbers the stream's events, so a viewer that
//! comes back while the engine's ring still holds what it missed gets those bytes instead of a new
//! snapshot. Keys, paste and wheel go to the engine as [`TerminalInput`] calls, in order, and the
//! engine encodes them from its terminal's modes at that instant.
use serde::{Deserialize, Serialize};

pub use crate::files::Bytes;
use crate::{Call, Cursor, Outcome, TerminalId, Watch};

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

/// A login shell in `cwd` on a new PTY, under the viewer's `terminal` id. Opening an id the engine
/// already hosts answers its summary and opens nothing. A shell that cannot start leaves a
/// terminal that says why.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTerminal {
    pub terminal: TerminalId,
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
}

/// Every terminal the engine hosts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTerminals {}

impl Call for ListTerminals {
    type Params = Self;
    type Reply = Vec<TerminalSummary>;

    const NAME: &'static str = "ListTerminals";
}

/// The PTY takes the viewer's grid. Every watcher's mirror reflows at the same byte, through
/// [`TerminalEvent::Resized`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeTerminal {
    pub terminal: TerminalId,
    pub size: TerminalSize,
}

impl Call for ResizeTerminal {
    type Params = Self;
    type Reply = Outcome;

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

/// One input for a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInput {
    pub terminal: TerminalId,
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

/// Watch a terminal: its snapshot, then its output from there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchTerminal {
    pub terminal: TerminalId,
    /// The last cursor this viewer holds, to resume from.
    pub since: Option<Cursor>,
}

impl Watch for WatchTerminal {
    type Event = TerminalEvent;
    type Params = Self;
    type State = TerminalState;

    const NAME: &'static str = "WatchTerminal";
}

/// A terminal as a watcher starts from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalState {
    pub size: TerminalSize,
    /// libghostty's snapshot of the terminal (screen, scrollback, modes and parser state) as of
    /// the cursor. Empty when the engine could not take one (output stopped inside an escape
    /// sequence longer than libghostty keeps): the viewer starts from a blank grid.
    pub screen: Bytes,
    pub exited: bool,
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
}
