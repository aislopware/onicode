//! A viewer's copy of a host terminal: libghostty-vt fed the host's snapshot and then its output,
//! with PTY write-back off, so the program's queries go unanswered here (the host answers them).
//!
//! Painting goes through libghostty's render state. Each change is captured into it at once, and
//! the rows it marks dirty collect until [`Mirror::frame`] hands them out. A row keeps its
//! [`RowId`] as it moves, so a view caches shaped rows by id and reshapes only dirty or new ones.
//! A synchronized update (mode 2026) holds the frame captured when it began, until it ends or
//! [`SYNC_TIMEOUT`] passes, as alacritty did.
//!
//! Selection follows alacritty's rules ([`crate::select`]), anchored in screen space so it stays
//! on its text as output scrolls; libghostty holds the resulting range, which paints and copies.
//!
//! A mirror starts from the renderable prefix of the host's snapshot ([`Mirror::attach`]) and
//! paints at once; the history after it ([`Mirror::history`]) swaps in the whole snapshot with
//! the output since replayed.
//!
//! The mirror shows the view's own size the moment the view takes it ([`Mirror::relayout`]),
//! before the host's PTY has, and keeps what it needs to come back to the host's terminal exactly:
//! the snapshot it started from (or one of its own) and every step of the host's since. When the
//! host takes that size the shown terminal is the host's, rebuilt from that replay only when
//! output crossed the resize on the way; when the host keeps another viewer's size, the host's
//! terminal shows again. Nothing about the host is guessed: what shows is either the host's
//! terminal or the host's terminal laid out at the view's size, until the host says which.
//!
//! Find runs over the mirror's own scrollback ([`crate::search`]): the grid is read once per
//! change and every query typed meanwhile matches the same lines.
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use libghostty_vt::fmt::{Formatter, FormatterOptions};
pub use libghostty_vt::render::RowId;
use libghostty_vt::render::{CellIteration, CellIterator, Cursor, Dirty, RowIterator};
use libghostty_vt::screen::{CellContentTag, CellWide, GridRef, Screen};
use libghostty_vt::selection::Selection as Range;
use libghostty_vt::snapshot::Decoder;
use libghostty_vt::style::{Palette, PaletteIndex, RgbColor, StyleColor, Underline};
use libghostty_vt::terminal::{CursorStyle, Mode, PointCoordinate, PointSpace, ScrollViewport};
use libghostty_vt::{RenderState, Terminal};
use onicode_protocol::terminal::TerminalSize;

use crate::search::{self, Found, Query};
use crate::select::{self, Grid, Kind, Point, Selection, Side};
use crate::{links, palette};

/// Output a mirror keeps to replay onto the snapshot it rebuilds from. Past this a history still
/// to come is given up (the mirror keeps the screen and the output since), and a view size still
/// waiting for the host starts over from a snapshot of the host's terminal as it is then.
const MAX_REPLAY_BYTES: usize = 8 << 20;

/// A synchronized update the program never ends shows after this (alacritty's
/// `SYNC_UPDATE_TIMEOUT`), so a crashed TUI cannot freeze the screen.
pub const SYNC_TIMEOUT: Duration = Duration::from_millis(150);

/// A cell's colour as the program set it; the view resolves the default and the palette
/// against its theme. An index the program redefined (OSC 4) arrives as `Rgb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellColor {
    /// The default foreground, or the default background (transparent).
    Default,
    /// Palette index: 0-15 ANSI, 16-231 the colour cube, 232-255 the grey ramp.
    Indexed(u8),
    /// `0xRRGGBB`.
    Rgb(u32),
}

/// One painted cell: its first codepoint (monocode drew no combining marks), colours and the
/// flags its paint reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    /// A space for an empty cell.
    pub ch: char,
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub inverse: bool,
    pub hidden: bool,
    /// The left half of a double-width character.
    pub wide: bool,
    /// Padding beside a wide character: paint nothing.
    pub spacer: bool,
    pub selected: bool,
}

/// The cursor's shape as DECSCUSR set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Beam,
    Underline,
    HollowBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameCursor {
    /// Viewport row and column.
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
}

/// One viewport row of a [`Frame`].
#[derive(Clone, Debug)]
pub struct FrameRow {
    /// Stable while the row moves; a view keys its cache on it.
    pub id: RowId,
    /// The row's cells, present when it changed since the last frame or the caller had nothing
    /// cached for its id.
    pub cells: Option<Vec<Cell>>,
}

/// What a view paints: the viewport's rows top to bottom and the cursor.
#[derive(Clone, Debug)]
pub struct Frame {
    /// Everything changed (a resize, scroll, selection or colour change): drop caches.
    pub full: bool,
    pub rows: Vec<FrameRow>,
    /// `None` when hidden or scrolled out of view.
    pub cursor: Option<FrameCursor>,
    /// The default foreground the program set (OSC 10), as `0xRRGGBB`.
    pub foreground: Option<u32>,
    /// The screen line of the viewport's top row.
    pub top: u32,
}

/// The modes that decide where the wheel goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM.
    pub app_cursor: bool,
    /// The program tracks the mouse (1000, 1002 or 1003).
    pub mouse: bool,
    pub alt_screen: bool,
    /// The wheel scrolls a full-screen program by arrow keys (1007).
    pub alt_scroll: bool,
}

/// An HTTP(S) link in the grid, an OSC 8 hyperlink or a plain URL, by its first and last cells.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: Point,
    pub end: Point,
    pub uri: String,
}

impl Link {
    #[must_use]
    pub fn contains(&self, at: Point) -> bool {
        select::contains(self.start, self.end, at)
    }
}

/// The render state and what changed since the view last read it. Shared with the render-hold
/// callback, which captures the frame a synchronized update leaves on screen.
struct RenderSide {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    dirty: HashSet<RowId>,
    full: bool,
    cursor: Option<Cursor>,
    /// When the program's synchronized update began.
    held_since: Option<Instant>,
}

impl RenderSide {
    fn new() -> libghostty_vt::error::Result<Self> {
        Ok(Self {
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            dirty: HashSet::new(),
            full: true,
            cursor: None,
            held_since: None,
        })
    }

    /// Pull the terminal into the render state and note what changed.
    fn capture(&mut self, term: &Terminal<'static, 'static>) -> libghostty_vt::error::Result<()> {
        let Self {
            state,
            rows,
            dirty,
            full,
            cursor,
            ..
        } = self;
        let snapshot = state.update(term)?;
        *cursor = Some(snapshot.cursor()?);
        match snapshot.dirty()? {
            Dirty::Clean => {}
            Dirty::Full => *full = true,
            Dirty::Partial => {
                let mut iteration = rows.update(&snapshot)?;
                while let Some(row) = iteration.next() {
                    if row.dirty()? {
                        dirty.insert(row.id()?);
                    }
                }
            }
        }
        snapshot.clean()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the host did after the snapshot a mirror can rebuild from, to replay onto it.
enum Step {
    Feed(Vec<u8>),
    Resize(TerminalSize),
}

/// The host's terminal as the mirror can rebuild it: a snapshot (only its prefix through READY
/// while the history is still to come) and every step the host took since.
struct Replay {
    snapshot: Vec<u8>,
    whole: bool,
    steps: Vec<Step>,
    bytes: usize,
}

impl Replay {
    const fn new(snapshot: Vec<u8>, whole: bool) -> Self {
        Self {
            snapshot,
            whole,
            steps: Vec::new(),
            bytes: 0,
        }
    }

    /// The host's terminal now: the snapshot with every step replayed. `None` when the snapshot
    /// does not decode.
    fn truth(&self) -> Option<Terminal<'static, 'static>> {
        let mut term = decode(&self.snapshot, self.whole)?;
        crate::configure(&mut term);
        for step in &self.steps {
            match step {
                Step::Feed(bytes) => term.vt_write(bytes),
                Step::Resize(size) => resize_term(&mut term, *size),
            }
        }
        Some(term)
    }
}

/// A terminal from a host snapshot, whole or through READY. It keeps an unfinished escape
/// sequence the snapshot carried, so the mirror can be snapshotted in turn.
fn decode(bytes: &[u8], whole: bool) -> Option<Terminal<'static, 'static>> {
    let mut decoder = Decoder::new_buf(bytes).ok()?;
    let _ = decoder.set_retain_continuation(true);
    let _ = decoder.set_max_continuation_bytes(crate::MAX_CONTINUATION_BYTES);
    if whole {
        decoder.decode().ok()
    } else {
        decoder
            .ready()
            .ok()
            .map(libghostty_vt::snapshot::IncrementalDecoder::into_terminal)
    }
}

fn resize_term(term: &mut Terminal<'static, 'static>, size: TerminalSize) {
    let _ = term.resize(
        size.cols.max(2),
        size.rows.max(1),
        u32::from(size.cell_width.max(1)),
        u32::from(size.cell_height.max(1)),
    );
}

/// The view's own size, shown before the host has taken it.
struct Local {
    size: TerminalSize,
    /// What the shown terminal went through since the replay's snapshot.
    shown: Vec<Shown>,
}

#[derive(Clone, Copy)]
enum Shown {
    /// The host's step of that index.
    Host(usize),
    /// A size of the view's own.
    Size(TerminalSize),
}

/// The shown terminal went through exactly what the host's did, so it is the host's.
fn same(shown: &[Shown], steps: &[Step]) -> bool {
    shown.len() == steps.len()
        && shown
            .iter()
            .zip(steps)
            .enumerate()
            .all(|(at, pair)| match pair {
                (Shown::Host(index), _) => *index == at,
                (Shown::Size(size), Step::Resize(host)) => size == host,
                (Shown::Size(_), Step::Feed(_)) => false,
            })
}

/// The find panel's query over the mirror.
struct Search {
    query: Query,
    /// The grid's logical lines as last read; `None` once the grid changed since.
    lines: Option<Vec<search::Line>>,
    found: Vec<Found>,
    current: Option<usize>,
    invalid: bool,
}

/// Where a find stands, for the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Finding {
    pub current: Option<usize>,
    pub total: usize,
    /// The query is a regular expression that does not parse.
    pub invalid: bool,
    /// The grid changed since the matches were found: [`Mirror::refresh_find`] finds them again.
    pub stale: bool,
}

/// The match a find starts on: the last one that begins above the bottom of the view, else the
/// first.
fn nearest(found: &[Found], bottom: u32) -> Option<usize> {
    found
        .iter()
        .rposition(|hit| hit.start.line <= bottom)
        .or_else(|| (!found.is_empty()).then_some(0))
}

/// A viewer's libghostty terminal, fed the host's snapshot and output.
pub struct Mirror {
    term: Terminal<'static, 'static>,
    render: Arc<Mutex<RenderSide>>,
    /// The palette before any OSC 4, to tell a redefined index.
    default_palette: Palette,
    /// The OSC 10 foreground the last frame painted with.
    foreground: Option<u32>,
    selection: Option<Selection>,
    /// The size the host's stream last gave.
    host_size: Option<TerminalSize>,
    /// What the shown terminal is rebuilt from, while the snapshot's history is still to come or
    /// the view's own size waits for the host.
    replay: Option<Replay>,
    /// The view's own size, shown ahead of the host's word ([`Self::relayout`]).
    local: Option<Local>,
    search: Option<Search>,
    /// Counts changes to the matches and the current one.
    found_epoch: u64,
}

impl std::fmt::Debug for Mirror {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mirror")
            .field("size", &self.size())
            .field("display_offset", &self.display_offset())
            .finish_non_exhaustive()
    }
}

impl Mirror {
    /// An empty terminal of `size`, until the host's snapshot arrives.
    #[must_use]
    pub fn new(size: TerminalSize) -> Self {
        let term =
            Terminal::new(size.cols.max(2), size.rows.max(1)).unwrap_or_else(|_| crate::oom());
        let mut mirror = Self::wrap(term);
        mirror.resize(size);
        mirror
    }

    /// The host's terminal as its whole snapshot encodes it, or `None` when the bytes are not
    /// one.
    #[must_use]
    pub fn from_snapshot(bytes: &[u8]) -> Option<Self> {
        Some(Self::wrap(decode(bytes, true)?))
    }

    /// The host's terminal from its snapshot's renderable prefix (through READY), to paint at
    /// once; the older scrollback follows through [`Self::history`]. A blank terminal of `size`
    /// when the bytes are empty or not a snapshot.
    #[must_use]
    pub fn attach(screen: &[u8], size: TerminalSize) -> Self {
        let Some(term) = decode(screen, false) else {
            return Self::new(size);
        };
        let mut mirror = Self::wrap(term);
        mirror.host_size = Some(size);
        mirror.replay = Some(Replay::new(screen.to_vec(), false));
        mirror
    }

    /// The rest of the snapshot [`Self::attach`] started from. The mirror becomes the whole
    /// snapshot with the output and resizes since replayed, scrolled back as far as it was and
    /// still at the view's own size if that waits for the host; a history that does not decode
    /// leaves it as it is.
    pub fn history(&mut self, history: &[u8]) {
        let Some(replay) = self.replay.as_mut().filter(|replay| !replay.whole) else {
            return;
        };
        replay.snapshot.extend_from_slice(history);
        replay.whole = true;
        if !self.rebuild() {
            self.replay = None;
            self.local = None;
        }
    }

    /// Older scrollback is still to come.
    #[must_use]
    pub fn awaiting_history(&self) -> bool {
        self.replay.as_ref().is_some_and(|replay| !replay.whole)
    }

    /// Shows the view's own size before the host has taken it, so a window resize relays the
    /// grid out in the same frame. The host's stream settles it: when the host takes this size,
    /// the shown terminal is the host's (rebuilt from the snapshot it started from if output
    /// crossed the resize on the way); when the host keeps another size ([`Self::settle`]), the
    /// host's terminal shows again. Back at the host's size, the host's terminal shows at once.
    pub fn relayout(&mut self, size: TerminalSize) {
        if self.host_size == Some(size) {
            self.settle_local();
            return;
        }
        if let Some(local) = &mut self.local {
            if local.size != size {
                local.size = size;
                local.shown.push(Shown::Size(size));
                self.apply_size(size);
            }
            return;
        }
        if self.replay.is_none() {
            let mut snapshot = Vec::new();
            if self.term.encode_snapshot(&mut snapshot).is_err() {
                // Nothing to come back to: the mirror waits for the host's word.
                return;
            }
            self.replay = Some(Replay::new(snapshot, true));
        }
        self.show_local(size);
    }

    /// The host answered the view's resize to `requested` with the PTY's size after it, or
    /// `None` when the call failed. A size the host did not take stops showing.
    pub fn settle(&mut self, requested: TerminalSize, pty: Option<TerminalSize>) {
        if pty != Some(requested)
            && self
                .local
                .as_ref()
                .is_some_and(|local| local.size == requested)
        {
            self.settle_local();
        }
    }

    /// The view's own size shows, and the host has yet to take it.
    #[must_use]
    pub const fn ahead(&self) -> bool {
        self.local.is_some()
    }

    /// Shows `size` from the replay's terminal as it is now, which the shown one must be.
    fn show_local(&mut self, size: TerminalSize) {
        let steps = self.replay.as_ref().map_or(0, |replay| replay.steps.len());
        let mut shown: Vec<Shown> = (0..steps).map(Shown::Host).collect();
        shown.push(Shown::Size(size));
        self.local = Some(Local { size, shown });
        self.apply_size(size);
    }

    /// The view's own size stops waiting: the shown terminal becomes the host's, rebuilt from
    /// the replay unless it went through exactly what the host's did.
    fn settle_local(&mut self) {
        let Some(local) = self.local.take() else {
            return;
        };
        let exact = self
            .replay
            .as_ref()
            .is_some_and(|replay| same(&local.shown, &replay.steps));
        if !exact && !self.rebuild() {
            self.replay = None;
            return;
        }
        if self.replay.as_ref().is_some_and(|replay| replay.whole) {
            self.replay = None;
        }
    }

    /// Shows the host's terminal as the replay rebuilds it, at the view's own size if that still
    /// waits for the host. The replay goes once it has nothing more to wait for. False when its
    /// snapshot does not decode.
    fn rebuild(&mut self) -> bool {
        let Some(truth) = self.replay.as_ref().and_then(Replay::truth) else {
            return false;
        };
        let local = self.local.take().map(|local| local.size);
        self.replace(truth);
        match local {
            Some(size) => self.show_local(size),
            None => {
                if self.replay.as_ref().is_some_and(|replay| replay.whole) {
                    self.replay = None;
                }
            }
        }
        true
    }

    /// Swaps in `term`, keeping what the view set: scroll position, selection and find.
    fn replace(&mut self, term: Terminal<'static, 'static>) {
        let back = self.display_offset();
        let mut next = Self::wrap(term);
        next.host_size = self.host_size;
        next.replay = self.replay.take();
        next.local = self.local.take();
        next.search = self.search.take();
        next.found_epoch = self.found_epoch;
        next.selection = self.selection.take();
        if back > 0 {
            next.scroll(i32::try_from(back).unwrap_or(i32::MAX));
        }
        *self = next;
        self.show_selection();
        self.stale_search();
    }

    /// Keeps a host step to replay.
    fn remember(&mut self, step: Step) {
        let Some(replay) = &mut self.replay else {
            return;
        };
        let feed = matches!(step, Step::Feed(_));
        replay.bytes += match &step {
            Step::Feed(bytes) => bytes.len(),
            Step::Resize(_) => 0,
        };
        let index = replay.steps.len();
        replay.steps.push(step);
        let over = replay.bytes > MAX_REPLAY_BYTES;
        if feed && let Some(local) = &mut self.local {
            local.shown.push(Shown::Host(index));
        }
        if over {
            self.overflow();
        }
    }

    /// The output since the replay's snapshot passed what a mirror keeps. A history still to
    /// come is given up (the screen and the output since stay), and a view size still waiting
    /// starts over from the host's terminal now.
    fn overflow(&mut self) {
        let Some(size) = self.local.as_ref().map(|local| local.size) else {
            self.replay = None;
            return;
        };
        self.local = None;
        if !self.rebuild() {
            self.replay = None;
            return;
        }
        let mut snapshot = Vec::new();
        if self.term.encode_snapshot(&mut snapshot).is_err() {
            self.replay = None;
            return;
        }
        self.replay = Some(Replay::new(snapshot, true));
        self.show_local(size);
    }

    fn wrap(mut term: Terminal<'static, 'static>) -> Self {
        crate::configure(&mut term);
        // The view's own size snapshots the mirror, which may be inside an escape sequence.
        let _ = term.set_continuation_max_bytes(crate::MAX_CONTINUATION_BYTES);
        let render = Arc::new(Mutex::new(
            RenderSide::new().unwrap_or_else(|_| crate::oom()),
        ));
        {
            let render = render.clone();
            // A panic here aborts the process; nothing below can panic.
            let _ = term.on_render_hold(move |term, held| {
                let Ok(mut side) = render.try_lock() else {
                    return;
                };
                if held {
                    let _ = side.capture(term);
                    side.held_since = Some(Instant::now());
                } else {
                    side.held_since = None;
                }
            });
        }
        let default_palette = term
            .default_color_palette()
            .unwrap_or_else(|_| palette::palette());
        let mut mirror = Self {
            term,
            render,
            default_palette,
            foreground: None,
            selection: None,
            host_size: None,
            replay: None,
            local: None,
            search: None,
            found_epoch: 0,
        };
        mirror.capture();
        mirror
    }

    /// Take the terminal's changes into the render state, unless a synchronized update holds
    /// the frame.
    fn capture(&mut self) {
        let mut side = lock(&self.render);
        if side.held_since.is_none() {
            let _ = side.capture(&self.term);
        }
    }

    /// Everything on screen changed for reasons libghostty's row flags may not cover
    /// (selection, scrolling).
    fn invalidate(&mut self) {
        lock(&self.render).full = true;
        self.capture();
    }

    /// The host's output, or a note written into the grid.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.term.vt_write(bytes);
        self.capture();
        self.stale_search();
        if self.replay.is_some() {
            self.remember(Step::Feed(bytes.to_vec()));
        }
    }

    /// When the open synchronized update shows anyway.
    #[must_use]
    pub fn sync_deadline(&self) -> Option<Instant> {
        lock(&self.render).held_since.map(|at| at + SYNC_TIMEOUT)
    }

    /// Ends a synchronized update past its deadline; true when it did.
    pub fn expire_sync(&mut self, now: Instant) -> bool {
        if !self.sync_deadline().is_some_and(|deadline| deadline <= now) {
            return false;
        }
        let _ = self.term.set_mode(Mode::SYNC_OUTPUT, false);
        lock(&self.render).held_since = None;
        self.invalidate();
        true
    }

    /// The host's PTY took a new size here in its stream. While the view's own size shows, the
    /// shown terminal keeps it, and the host taking that same size settles it.
    pub fn resize(&mut self, size: TerminalSize) {
        self.host_size = Some(size);
        self.remember(Step::Resize(size));
        match &self.local {
            Some(local) if local.size == size => self.settle_local(),
            Some(_) => {}
            None => self.apply_size(size),
        }
    }

    fn apply_size(&mut self, size: TerminalSize) {
        resize_term(&mut self.term, size);
        self.show_selection();
        self.stale_search();
    }

    /// Columns and rows.
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        (self.term.cols().unwrap_or(0), self.term.rows().unwrap_or(0))
    }

    fn mode(&self, mode: Mode) -> bool {
        self.term.mode(mode).unwrap_or(false)
    }

    #[must_use]
    pub fn modes(&self) -> Modes {
        Modes {
            app_cursor: self.mode(Mode::DECCKM),
            mouse: self.mode(Mode::NORMAL_MOUSE)
                || self.mode(Mode::BUTTON_MOUSE)
                || self.mode(Mode::ANY_MOUSE),
            alt_screen: self
                .term
                .active_screen()
                .is_ok_and(|screen| screen == Screen::Alternate),
            alt_scroll: self.mode(Mode::ALT_SCROLL),
        }
    }

    /// Lines scrolled back from the live bottom.
    #[must_use]
    pub fn display_offset(&self) -> usize {
        self.term.scrollbar().map_or(0, |bar| {
            usize::try_from(bar.total.saturating_sub(bar.offset).saturating_sub(bar.len))
                .unwrap_or(0)
        })
    }

    /// Scrolls `lines` back into history (negative: toward the bottom); true when the view
    /// moved.
    pub fn scroll(&mut self, lines: i32) -> bool {
        let before = self.display_offset();
        self.term.scroll_viewport(ScrollViewport::Delta(
            isize::try_from(lines).map_or(0, |l| -l),
        ));
        let moved = self.display_offset() != before;
        if moved {
            self.invalidate();
        }
        moved
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.display_offset() > 0 {
            self.term.scroll_viewport(ScrollViewport::Bottom);
            self.invalidate();
        }
    }

    /// A viewport cell in screen space, clamped into the grid.
    #[must_use]
    pub fn screen_point(&self, row: usize, col: usize) -> Option<Point> {
        let (cols, rows) = self.size();
        let x = u16::try_from(col)
            .unwrap_or(u16::MAX)
            .min(cols.saturating_sub(1));
        let y = u16::try_from(row)
            .unwrap_or(u16::MAX)
            .min(rows.saturating_sub(1));
        let cell = self
            .term
            .grid_ref(libghostty_vt::terminal::Point::Viewport(PointCoordinate {
                x,
                y: u32::from(y),
            }))
            .ok()?;
        let at = self
            .term
            .point_from_grid_ref(&cell, PointSpace::Screen)
            .ok()
            .flatten()?;
        Some(Point {
            line: at.y,
            col: at.x,
        })
    }

    /// The screen lines the view shows, top and bottom.
    fn viewport_lines(&self) -> (u32, u32) {
        let top = self.screen_point(0, 0).map_or(0, |at| at.line);
        let rows = u32::from(self.size().1).max(1);
        (top, top + rows - 1)
    }

    fn stale_search(&mut self) {
        if let Some(open) = &mut self.search {
            open.lines = None;
        }
    }

    /// Finds `query` in the scrollback and on the screen, and scrolls to the match nearest the
    /// bottom of the view.
    pub fn find(&mut self, query: Query) {
        let lines = self
            .search
            .take()
            .and_then(|open| open.lines)
            .unwrap_or_else(|| search::lines(&View(&self.term)));
        let found = search::find(&lines, &query);
        let (_, bottom) = self.viewport_lines();
        let current = found.as_deref().and_then(|found| nearest(found, bottom));
        self.search = Some(Search {
            query,
            lines: Some(lines),
            invalid: found.is_none(),
            found: found.unwrap_or_default(),
            current,
        });
        self.found_epoch += 1;
        self.reveal();
    }

    /// Steps to the next match down, or up with `backwards`, round the ends, and scrolls to it.
    pub fn find_step(&mut self, backwards: bool) {
        let Some(open) = &mut self.search else {
            return;
        };
        let total = open.found.len();
        if total == 0 {
            return;
        }
        open.current = Some(match open.current {
            None if backwards => total - 1,
            None => 0,
            Some(at) if backwards => (at + total - 1) % total,
            Some(at) => (at + 1) % total,
        });
        self.found_epoch += 1;
        self.reveal();
    }

    /// Finds the query again over a grid that changed since, keeping the current match where it
    /// was, without scrolling; true when the matches changed.
    pub fn refresh_find(&mut self) -> bool {
        let (_, bottom) = self.viewport_lines();
        let Some(open) = &mut self.search else {
            return false;
        };
        if open.lines.is_some() {
            return false;
        }
        let lines = search::lines(&View(&self.term));
        let found = search::find(&lines, &open.query).unwrap_or_default();
        let was = open.current.and_then(|at| open.found.get(at)).copied();
        let current = match was {
            Some(was) => found.iter().position(|hit| *hit == was).or_else(|| {
                let after = found.partition_point(|hit| *hit < was);
                found.len().checked_sub(1).map(|last| after.min(last))
            }),
            None => nearest(&found, bottom),
        };
        open.lines = Some(lines);
        let changed = found != open.found || current != open.current;
        open.found = found;
        open.current = current;
        if changed {
            self.found_epoch += 1;
        }
        changed
    }

    /// Closes the find: no matches show.
    pub fn end_find(&mut self) {
        if self.search.take().is_some() {
            self.found_epoch += 1;
        }
    }

    /// Where the open find stands.
    #[must_use]
    pub fn finding(&self) -> Option<Finding> {
        self.search.as_ref().map(|open| Finding {
            current: open.current,
            total: open.found.len(),
            invalid: open.invalid,
            stale: open.lines.is_none(),
        })
    }

    /// The matches on the screen lines `top..top + rows`, each with whether it is the current
    /// one.
    #[must_use]
    pub fn found_in(&self, top: u32, rows: u32) -> Vec<(Found, bool)> {
        let Some(open) = &self.search else {
            return Vec::new();
        };
        let bottom = top.saturating_add(rows);
        let first = open.found.partition_point(|hit| hit.end.line < top);
        open.found
            .iter()
            .enumerate()
            .skip(first)
            .take_while(|(_, hit)| hit.start.line < bottom)
            .map(|(at, hit)| (*hit, open.current == Some(at)))
            .collect()
    }

    /// Changes whenever the matches or the current one do, for a view caching painted rows.
    #[must_use]
    pub const fn found_epoch(&self) -> u64 {
        self.found_epoch
    }

    /// Scrolls the current match into the middle of the view, unless it shows already.
    fn reveal(&mut self) {
        let hit = self
            .search
            .as_ref()
            .and_then(|open| open.found.get(open.current?))
            .copied();
        let Some(hit) = hit else {
            return;
        };
        let (top, bottom) = self.viewport_lines();
        if hit.start.line >= top && hit.end.line <= bottom {
            return;
        }
        let half = (bottom - top).div_ceil(2);
        let target = hit.start.line.saturating_sub(half);
        let lines = i64::from(top) - i64::from(target);
        self.scroll(i32::try_from(lines).unwrap_or(0));
    }

    /// Starts a selection at a viewport cell; a click selects nothing until it is dragged, a
    /// double click selects the word, a triple click the line.
    pub fn start_selection(&mut self, kind: Kind, row: usize, col: usize, side: Side) {
        self.selection = self
            .screen_point(row, col)
            .map(|at| Selection::new(kind, at, side));
        self.show_selection();
    }

    /// Moves the selection's end to a viewport cell; false without a selection.
    pub fn extend_selection(&mut self, row: usize, col: usize, side: Side) -> bool {
        let (Some(mut selection), Some(at)) = (self.selection, self.screen_point(row, col)) else {
            return false;
        };
        selection.update(at, side);
        self.selection = Some(selection);
        self.show_selection();
        true
    }

    /// There is a selection, even one that covers no cell yet.
    #[must_use]
    pub fn selecting(&self) -> bool {
        self.selection.is_some()
    }

    /// The selection covers at least one cell.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.term.selection().ok().flatten().is_some()
    }

    /// Selects the scrollback and the screen.
    pub fn select_all(&mut self) {
        let (cols, _) = self.size();
        let lines = View(&self.term).lines();
        let mut selection = Selection::new(Kind::Simple, Point::default(), Side::Left);
        selection.update(
            Point {
                line: lines.saturating_sub(1),
                col: cols.saturating_sub(1),
            },
            Side::Right,
        );
        self.selection = Some(selection);
        self.show_selection();
    }

    pub fn clear_selection(&mut self) {
        if self.selection.take().is_some() || self.has_selection() {
            self.show_selection();
        }
    }

    /// Hands libghostty the selection's cells, for painting and copying.
    fn show_selection(&mut self) {
        let term = &self.term;
        let cell = |at: Point| {
            term.grid_ref(libghostty_vt::terminal::Point::Screen(PointCoordinate {
                x: at.col,
                y: at.line,
            }))
            .ok()
        };
        let range = self
            .selection
            .and_then(|selection| selection.range(&View(term)))
            .and_then(|(start, end)| Some(Range::new(cell(start)?, cell(end)?, false)));
        let _ = term.set_selection(range.as_ref());
        self.invalidate();
    }

    /// The selection as it copies: soft wraps joined, trailing blanks trimmed.
    #[must_use]
    pub fn selection_text(&self) -> Option<String> {
        let selection = self.term.selection().ok().flatten()?;
        let options = FormatterOptions::copy().with_selection(&selection);
        let mut formatter = Formatter::new(&self.term, options).ok()?;
        let mut out = Vec::new();
        formatter.format_vec(&mut out).ok()?;
        String::from_utf8(out).ok().filter(|text| !text.is_empty())
    }

    /// The HTTP(S) link under a viewport cell: the OSC 8 hyperlink on it, else a plain URL on its
    /// logical line.
    #[must_use]
    pub fn link_at(&self, row: usize, col: usize) -> Option<Link> {
        let at = self.screen_point(row, col)?;
        let view = View(&self.term);
        let (first, last) = select::logical_line(&view, at);
        if let Some(uri) = view.hyperlink(at) {
            let same = |point: Point| view.hyperlink(point).as_deref() == Some(uri.as_str());
            let mut start = at;
            while start > first
                && let Some(before) = select::prev(&view, start)
                && same(before)
            {
                start = before;
            }
            let mut end = at;
            while end < last
                && let Some(after) = select::next(&view, end)
                && same(after)
            {
                end = after;
            }
            return links::is_web_link(&uri).then_some(Link { start, end, uri });
        }
        let mut chars = Vec::new();
        let mut points = Vec::new();
        let mut cursor = Some(first);
        while let Some(point) = cursor.filter(|point| *point <= last) {
            if !view.spacer(point) {
                chars.push(view.char_at(point));
                points.push(point);
            }
            cursor = select::next(&view, point);
        }
        links::urls(&chars).into_iter().find_map(|(from, to)| {
            let (start, end) = (*points.get(from)?, *points.get(to)?);
            let uri: String = chars.get(from..=to)?.iter().collect();
            (select::contains(start, end, at) && links::is_web_link(&uri)).then_some(Link {
                start,
                end,
                uri,
            })
        })
    }

    /// The frame to paint. Rows come with cells when they changed since the last frame or
    /// `cached` has nothing for their id; a view repaints those and reuses its cache for the
    /// rest.
    pub fn frame(&mut self, mut cached: impl FnMut(RowId) -> bool) -> Frame {
        let foreground = match (self.term.fg_color(), self.term.default_fg_color()) {
            (Ok(Some(color)), Ok(default)) if Some(color) != default => Some(palette::hex(color)),
            _ => None,
        };
        let recolored = foreground != self.foreground;
        self.foreground = foreground;
        let top = self.screen_point(0, 0).map_or(0, |at| at.line);
        let mut side = lock(&self.render);
        side.full |= recolored;
        let mut frame =
            read_frame(&mut side, &self.default_palette, &mut cached).unwrap_or_else(|_| Frame {
                full: true,
                rows: Vec::new(),
                cursor: None,
                foreground: None,
                top: 0,
            });
        drop(side);
        frame.foreground = foreground;
        frame.top = top;
        frame
    }

    /// The viewport as text, a line per row with trailing blanks and empty rows trimmed.
    #[must_use]
    pub fn text(&self) -> String {
        let (cols, rows) = self.size();
        let view = View(&self.term);
        let top = self.screen_point(0, 0).map_or(0, |at| at.line);
        (0..u32::from(rows))
            .map(|row| {
                let line: String = (0..cols)
                    .map(|col| Point {
                        line: top + row,
                        col,
                    })
                    .filter(|at| !view.spacer(*at))
                    .map(|at| view.char_at(at))
                    .collect();
                line.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end_matches('\n')
            .to_owned()
    }
}

/// The grid in screen space, as [`select`] reads it.
struct View<'t>(&'t Terminal<'static, 'static>);

impl View<'_> {
    fn cell(&self, at: Point) -> Option<GridRef<'_>> {
        self.0
            .grid_ref(libghostty_vt::terminal::Point::Screen(PointCoordinate {
                x: at.col,
                y: at.line,
            }))
            .ok()
    }

    /// The OSC 8 URI on a cell.
    fn hyperlink(&self, at: Point) -> Option<String> {
        let cell = self.cell(at)?;
        if !cell.cell().ok()?.has_hyperlink().ok()? {
            return None;
        }
        let mut buf = vec![0; 2048];
        let len = match cell.hyperlink_uri(&mut buf) {
            Ok(len) => len,
            Err(libghostty_vt::Error::OutOfSpace { required }) if required > buf.len() => {
                buf.resize(required, 0);
                cell.hyperlink_uri(&mut buf).ok()?
            }
            Err(_) => return None,
        };
        buf.truncate(len);
        String::from_utf8(buf).ok().filter(|uri| !uri.is_empty())
    }
}

impl Grid for View<'_> {
    fn columns(&self) -> u16 {
        self.0.cols().unwrap_or(1).max(1)
    }

    fn lines(&self) -> u32 {
        self.0
            .scrollbar()
            .map_or(1, |bar| u32::try_from(bar.total).unwrap_or(u32::MAX).max(1))
    }

    fn char_at(&self, at: Point) -> char {
        self.cell(at)
            .and_then(|cell| cell.cell().ok())
            .filter(|cell| cell.has_text().unwrap_or(false))
            .and_then(|cell| cell.codepoint().ok())
            .and_then(char::from_u32)
            .unwrap_or(' ')
    }

    fn spacer(&self, at: Point) -> bool {
        self.cell(at)
            .and_then(|cell| cell.cell().ok())
            .and_then(|cell| cell.wide().ok())
            .is_some_and(|wide| matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead))
    }

    fn wrapped(&self, line: u32) -> bool {
        self.cell(Point { line, col: 0 })
            .and_then(|cell| cell.row().ok())
            .and_then(|row| row.is_wrapped().ok())
            .unwrap_or(false)
    }

    /// One grid read per cell, for a find reading the whole scrollback.
    fn glyph(&self, at: Point) -> Option<char> {
        let fields = self
            .cell(at)
            .and_then(|cell| cell.cell().ok())
            .and_then(|cell| cell.fields().ok());
        match fields {
            Some(fields) if matches!(fields.wide, CellWide::SpacerTail | CellWide::SpacerHead) => {
                None
            }
            Some(fields) if fields.has_text() => {
                Some(char::from_u32(fields.codepoint).unwrap_or(' '))
            }
            _ => Some(' '),
        }
    }
}

fn shape(style: CursorStyle) -> CursorShape {
    match style {
        CursorStyle::Bar => CursorShape::Beam,
        CursorStyle::Underline => CursorShape::Underline,
        CursorStyle::BlockHollow => CursorShape::HollowBlock,
        _ => CursorShape::Block,
    }
}

/// Reads the captured render state, handing out (and clearing) what changed since the last
/// frame.
fn read_frame(
    side: &mut RenderSide,
    defaults: &Palette,
    cached: &mut dyn FnMut(RowId) -> bool,
) -> libghostty_vt::error::Result<Frame> {
    let RenderSide {
        state,
        rows,
        cells,
        dirty,
        full,
        cursor,
        ..
    } = side;
    let snapshot = state.snapshot()?;
    let colors = snapshot.colors()?;
    let mut frame = Frame {
        full: *full,
        rows: Vec::with_capacity(usize::from(snapshot.rows()?)),
        cursor: cursor.and_then(|cursor| {
            let at = cursor.viewport.filter(|_| cursor.visible)?;
            Some(FrameCursor {
                row: usize::from(at.y),
                col: usize::from(at.x),
                shape: shape(cursor.visual_style),
            })
        }),
        foreground: None,
        top: 0,
    };
    let mut iteration = rows.update(&snapshot)?;
    while let Some(row) = iteration.next() {
        let id = row.id()?;
        let painted = if frame.full || dirty.contains(&id) || !cached(id) {
            let mut iteration = cells.update(row)?;
            Some(read_cells(&mut iteration, &colors.palette, defaults)?)
        } else {
            None
        };
        frame.rows.push(FrameRow { id, cells: painted });
    }
    dirty.clear();
    *full = false;
    Ok(frame)
}

fn color(color: StyleColor, active: &[RgbColor; 256], defaults: &Palette) -> CellColor {
    match color {
        StyleColor::None => CellColor::Default,
        StyleColor::Palette(PaletteIndex(index)) => indexed(index, active, defaults),
        StyleColor::Rgb(rgb) => CellColor::Rgb(palette::hex(rgb)),
    }
}

/// An index stays an index for the view to resolve, unless the program redefined it.
fn indexed(index: u8, active: &[RgbColor; 256], defaults: &Palette) -> CellColor {
    let slot = usize::from(index);
    match (active.get(slot), defaults.0.get(slot)) {
        (Some(now), Some(default)) if now != default => CellColor::Rgb(palette::hex(*now)),
        _ => CellColor::Indexed(index),
    }
}

fn read_cells(
    iteration: &mut CellIteration<'static, '_>,
    active: &[RgbColor; 256],
    defaults: &Palette,
) -> libghostty_vt::error::Result<Vec<Cell>> {
    let mut out = Vec::new();
    while let Some(cell) = iteration.next() {
        let raw = cell.raw_cell()?;
        let style = cell.style()?;
        let wide = raw.wide()?;
        let ch = if raw.has_text()? {
            char::from_u32(raw.codepoint()?).unwrap_or(' ')
        } else {
            ' '
        };
        // A cell erased with a background keeps the colour in the cell, not in a style.
        let bg = match raw.content_tag()? {
            CellContentTag::BgColorPalette => indexed(raw.bg_color_palette()?.0, active, defaults),
            CellContentTag::BgColorRgb => CellColor::Rgb(palette::hex(raw.bg_color_rgb()?)),
            _ => color(style.bg_color, active, defaults),
        };
        out.push(Cell {
            ch,
            fg: color(style.fg_color, active, defaults),
            bg,
            bold: style.bold,
            dim: style.faint,
            italic: style.italic,
            underline: style.underline != Underline::None,
            strike: style.strikethrough,
            inverse: style.inverse,
            hidden: style.invisible,
            wide: wide == CellWide::Wide,
            spacer: matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead),
            selected: cell.is_selected()?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use onicode_protocol::terminal::TerminalColors;
    use proptest::prelude::*;

    use super::*;
    use crate::host::Host;

    fn size(cols: u16, rows: u16) -> TerminalSize {
        TerminalSize {
            cols,
            rows,
            cell_width: 8,
            cell_height: 16,
        }
    }

    fn mirror(cols: u16, rows: u16, bytes: &[u8]) -> Mirror {
        let mut mirror = Mirror::new(size(cols, rows));
        mirror.feed(bytes);
        mirror
    }

    fn cells(mirror: &mut Mirror) -> Vec<Vec<Cell>> {
        let frame = mirror.frame(|_| false);
        frame
            .rows
            .into_iter()
            .map(|row| row.cells.unwrap_or_default())
            .collect()
    }

    #[test]
    fn colours_stay_indexed_until_redefined() {
        let mut m = mirror(10, 2, b"\x1b[31ma\x1b[38;2;1;2;3mb\x1b[0m\x1b[7;2mc");
        let row = &cells(&mut m)[0];
        assert_eq!(row[0].fg, CellColor::Indexed(1));
        assert_eq!(row[1].fg, CellColor::Rgb(0x010203));
        assert_eq!(
            (row[2].fg, row[2].bg),
            (CellColor::Default, CellColor::Default)
        );
        assert!(row[2].inverse && row[2].dim);
        m.feed(b"\x1b]4;1;rgb:ff/00/00\x1b\\");
        assert_eq!(cells(&mut m)[0][0].fg, CellColor::Rgb(0xff0000));
        m.feed(b"\x1b[44m\x1b[K");
        assert_eq!(cells(&mut m)[0][9].bg, CellColor::Indexed(4));
    }

    #[test]
    fn osc_10_overrides_the_default_foreground() {
        let mut m = mirror(10, 2, b"a");
        assert_eq!(m.frame(|_| true).foreground, None);
        m.feed(b"\x1b]10;rgb:12/34/56\x1b\\");
        let frame = m.frame(|_| true);
        assert_eq!(frame.foreground, Some(0x123456));
        assert!(frame.full);
    }

    #[test]
    fn the_cursor_follows_decscusr_and_starts_as_a_beam() {
        let mut m = mirror(10, 2, b"ab");
        let cursor = m.frame(|_| true).cursor.unwrap();
        assert_eq!(
            (cursor.row, cursor.col, cursor.shape),
            (0, 2, CursorShape::Beam)
        );
        m.feed(b"\x1b[2 q");
        assert_eq!(m.frame(|_| true).cursor.unwrap().shape, CursorShape::Block);
        m.feed(b"\x1b[4 q");
        assert_eq!(
            m.frame(|_| true).cursor.unwrap().shape,
            CursorShape::Underline
        );
        m.feed(b"\x1b[?25l");
        assert_eq!(m.frame(|_| true).cursor, None);
    }

    #[test]
    fn only_changed_rows_come_with_cells() {
        let mut m = mirror(10, 3, b"a\r\nb\r\nc");
        let first = m.frame(|_| false);
        assert!(first.rows.iter().all(|row| row.cells.is_some()));
        m.feed(b"\x1b[2;1Hx");
        let next = m.frame(|_| true);
        assert!(!next.full);
        let painted: Vec<bool> = next.rows.iter().map(|row| row.cells.is_some()).collect();
        assert!(!painted[0] && painted[1]);
    }

    #[test]
    fn a_held_frame_shows_after_the_sync_timeout() {
        let mut m = mirror(10, 2, b"a");
        m.feed(b"\x1b[?2026hb");
        assert_eq!(m.text(), "ab");
        let row = cells(&mut m)[0][1];
        assert_eq!(row.ch, ' ');
        let deadline = m.sync_deadline().unwrap();
        assert!(!m.expire_sync(deadline - Duration::from_millis(1)));
        assert!(m.expire_sync(deadline));
        assert_eq!(cells(&mut m)[0][1].ch, 'b');
    }

    #[test]
    fn selection_follows_alacritty_and_copies_joined_lines() {
        let mut m = mirror(6, 3, b"abcdef\r\nghi");
        m.start_selection(Kind::Simple, 0, 1, Side::Left);
        assert!(m.selecting() && !m.has_selection());
        m.extend_selection(1, 1, Side::Right);
        assert_eq!(m.selection_text().as_deref(), Some("bcdef\ngh"));
        assert!(cells(&mut m)[0][1].selected);
        m.start_selection(Kind::Word, 1, 1, Side::Left);
        assert_eq!(m.selection_text().as_deref(), Some("ghi"));
        m.select_all();
        assert_eq!(m.selection_text().as_deref(), Some("abcdef\nghi"));
        m.clear_selection();
        assert!(!m.has_selection() && !m.selecting());
    }

    #[test]
    fn links_are_found_across_wraps_and_by_osc_8() {
        let mut m = mirror(10, 4, b"go https://example.com/a. ok");
        let link = m.link_at(1, 2).unwrap();
        assert_eq!(link.uri, "https://example.com/a");
        assert_eq!(
            (link.start, link.end),
            (Point { line: 0, col: 3 }, Point { line: 2, col: 3 })
        );
        assert_eq!(m.link_at(0, 0), None);
        m.feed(b"\r\n\x1b]8;;https://x.dev\x1b\\docs\x1b]8;;\x1b\\");
        let link = m.link_at(3, 1).unwrap();
        assert_eq!(link.uri, "https://x.dev");
        assert_eq!(
            (link.start, link.end),
            (Point { line: 3, col: 0 }, Point { line: 3, col: 3 })
        );
    }

    #[test]
    fn the_wheel_modes_follow_the_program() {
        let mut m = mirror(10, 2, b"");
        assert_eq!(
            m.modes(),
            Modes {
                alt_scroll: true,
                ..Modes::default()
            }
        );
        m.feed(b"\x1b[?1049h\x1b[?1h\x1b[?1002h");
        assert_eq!(
            m.modes(),
            Modes {
                app_cursor: true,
                mouse: true,
                alt_screen: true,
                alt_scroll: true,
            }
        );
    }

    #[test]
    fn scrolling_moves_through_history_and_back() {
        let mut m = mirror(4, 2, b"1\r\n2\r\n3\r\n4");
        assert!(m.scroll(1));
        assert_eq!((m.display_offset(), m.text().as_str()), (1, "2\n3"));
        m.scroll(-5);
        assert_eq!(m.display_offset(), 0);
        m.scroll(10);
        m.scroll_to_bottom();
        assert_eq!(m.text(), "3\n4");
    }

    fn host(cols: u16, rows: u16) -> Host {
        let colors = TerminalColors {
            foreground: 0xebebeb,
            background: 0x171717,
            cursor: 0x4da3f5,
        };
        Host::new(size(cols, rows), colors, "xterm-ghostty")
    }

    fn output() -> impl Strategy<Value = Vec<u8>> {
        let piece = prop_oneof![
            "[a-z ]{1,12}".prop_map(String::into_bytes),
            Just(b"\r\n".to_vec()),
            Just(b"\x1b[31m".to_vec()),
            Just(b"\x1b[0m".to_vec()),
            Just(b"\x1b[2J".to_vec()),
            Just(b"\x1b[3;2H".to_vec()),
            Just(b"\x1b[?1049h".to_vec()),
            Just(b"\x1b[?1049l".to_vec()),
            Just(b"\x1b[2 q".to_vec()),
            Just(b"\x1b]4;1;rgb:ff/00/00\x1b\\".to_vec()),
            Just("é漢".as_bytes().to_vec()),
        ];
        proptest::collection::vec(piece, 0..24).prop_map(|pieces| pieces.concat())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// A viewer that attaches with the host's snapshot at any byte, even inside an escape
        /// sequence or a character, and then follows the rest, sees what one fed everything sees.
        #[test]
        fn a_snapshot_then_the_rest_matches_the_whole_stream(
            bytes in output(),
            split in any::<prop::sample::Index>(),
        ) {
            let at = split.index(bytes.len() + 1);
            let mut host = host(12, 4);
            host.feed(&bytes[..at]);
            let snapshot = host.snapshot().expect("a snapshot");
            let mut attached = Mirror::from_snapshot(&snapshot).expect("it decodes");
            attached.feed(&bytes[at..]);
            let mut whole = mirror(12, 4, &bytes);
            prop_assert_eq!(attached.text(), whole.text());
            prop_assert_eq!(attached.frame(|_| false).cursor, whole.frame(|_| false).cursor);
            prop_assert_eq!(cells(&mut attached), cells(&mut whole));
        }

        /// A viewer that paints from the snapshot's prefix, takes output (and a resize) before
        /// the history arrives, then the history and the rest, holds what a viewer restored from
        /// the whole snapshot holds, scrollback included.
        #[test]
        fn prefix_then_history_restores_the_whole_snapshot(
            bytes in output(),
            lines in 0usize..80,
            split in any::<prop::sample::Index>(),
            mid in any::<prop::sample::Index>(),
            resized in any::<bool>(),
        ) {
            let mut host = host(12, 4);
            for n in 0..lines {
                host.feed(format!("line {n}\r\n").as_bytes());
            }
            let at = split.index(bytes.len() + 1);
            host.feed(&bytes[..at]);
            let snapshot = host.snapshot().expect("a snapshot");
            let rest = &bytes[at..];
            let cut = mid.index(rest.len() + 1);
            let (prefix, history) = crate::snapshot::split(snapshot.clone());
            let mut parts = Mirror::attach(&prefix, size(12, 4));
            prop_assert!(parts.awaiting_history());
            let mut whole = Mirror::from_snapshot(&snapshot).expect("it decodes");
            for mirror in [&mut parts, &mut whole] {
                mirror.feed(&rest[..cut]);
                if resized {
                    mirror.resize(size(9, 5));
                }
            }
            parts.history(&history);
            prop_assert!(!parts.awaiting_history());
            parts.feed(&rest[cut..]);
            whole.feed(&rest[cut..]);
            prop_assert_eq!(parts.text(), whole.text());
            prop_assert_eq!(parts.frame(|_| false).cursor, whole.frame(|_| false).cursor);
            prop_assert_eq!(cells(&mut parts), cells(&mut whole));
            parts.select_all();
            whole.select_all();
            prop_assert_eq!(parts.selection_text(), whole.selection_text());
        }
    }

    #[test]
    fn find_shows_the_nearest_match_and_steps_round_the_ends() {
        let output: String = (0..30).map(|n| format!("line {n:02}\r\n")).collect();
        let mut m = mirror(12, 4, output.as_bytes());
        let query = |text: &str| Query {
            text: text.to_owned(),
            ..Query::default()
        };
        m.find(query("LINE 0"));
        let finding = m.finding().unwrap();
        assert_eq!((finding.total, finding.current), (10, Some(9)));
        assert!(m.display_offset() > 0, "scrolled up to the nearest match");
        assert!(m.text().contains("line 09"));
        m.find_step(false);
        assert_eq!(m.finding().unwrap().current, Some(0), "round the bottom");
        assert!(m.display_offset() > 0, "scrolled up to it");
        assert!(m.text().contains("line 00"));
        let top = m.screen_point(0, 0).unwrap().line;
        let shown = m.found_in(top, 4);
        assert!(
            shown
                .iter()
                .any(|(hit, current)| *current && hit.start.line == 0)
        );
        m.find_step(true);
        assert_eq!(m.finding().unwrap().current, Some(9));
        m.feed(b"line 0x\r\n");
        assert!(m.finding().unwrap().stale);
        let epoch = m.found_epoch();
        assert!(m.refresh_find());
        assert!(m.found_epoch() > epoch);
        let finding = m.finding().unwrap();
        assert_eq!(
            (finding.total, finding.current, finding.stale),
            (11, Some(9), false)
        );
        m.find(Query {
            text: "line 0[".into(),
            regex: true,
            ..Query::default()
        });
        assert!(m.finding().unwrap().invalid);
        m.end_find();
        assert_eq!(m.finding(), None);
        assert!(m.found_in(0, 40).is_empty());
    }

    #[test]
    fn a_relayout_shows_at_once_and_the_hosts_resize_settles_it() {
        let mut host = host(12, 4);
        host.feed(b"hello");
        let snapshot = host.snapshot().unwrap();
        let mut m = Mirror::attach(&snapshot, size(12, 4));
        m.history(&[]);
        m.relayout(size(20, 6));
        assert_eq!(m.size(), (20, 6), "the view's size shows in the same frame");
        assert!(m.ahead());
        // Output the host wrote before it took the size reaches the mirror after.
        m.feed(b" world");
        m.resize(size(20, 6));
        assert!(!m.ahead(), "the host took it");
        let mut truth = Mirror::from_snapshot(&snapshot).unwrap();
        truth.feed(b" world");
        truth.resize(size(20, 6));
        assert_eq!(m.text(), truth.text());
        assert_eq!(cells(&mut m), cells(&mut truth));
        // Another viewer typed last: the host keeps its size, and so does the mirror.
        m.relayout(size(30, 8));
        m.settle(size(30, 8), Some(size(20, 6)));
        assert!(!m.ahead());
        assert_eq!(m.size(), (20, 6));
        // Back at the host's size while ahead: the host's terminal at once.
        m.relayout(size(16, 5));
        m.relayout(size(20, 6));
        assert!(!m.ahead());
        assert_eq!(m.text(), truth.text());
    }

    /// A terminal decoded from a snapshot, fed, encoded and decoded again keeps its scrollback
    /// when it next scrolls, as a relayout's rebuild does (CI's shrunk case of
    /// `a_mirror_ahead_of_the_host_comes_back_to_the_hosts_terminal`): libghostty's default
    /// 10 kB byte limit pruned the decoded terminal's compact history page whole.
    #[test]
    fn a_terminal_decoded_twice_keeps_its_scrollback_as_it_scrolls() {
        let blank = host(10, 4).snapshot().unwrap();
        let mut once = Mirror::from_snapshot(&blank).unwrap();
        let mut direct = Mirror::new(size(10, 4));
        let early = b"one\r\ntwo\r\nthree\r\nfour\r\nfive\r\nsix\r\n";
        once.feed(early);
        direct.feed(early);
        let mut encoded = Vec::new();
        once.term.encode_snapshot(&mut encoded).unwrap();
        let mut twice = Mirror::from_snapshot(&encoded).unwrap();
        for bytes in [&b"aa"[..], b"aaa a aa "] {
            twice.feed(bytes);
            direct.feed(bytes);
        }
        twice.select_all();
        direct.select_all();
        assert_eq!(twice.selection_text(), direct.selection_text());
    }

    /// The same, through a relayout whose output crossed the host's resize.
    #[test]
    fn a_relayout_rebuild_keeps_the_scrollback() {
        let mut host = host(10, 4);
        let snapshot = host.snapshot().unwrap();
        let (prefix, history) = crate::snapshot::split(snapshot.clone());
        let mut m = Mirror::attach(&prefix, size(10, 4));
        m.history(&history);
        let mut truth = Mirror::from_snapshot(&snapshot).unwrap();
        for bytes in [
            "\r\na aa  aa\u{e9}\u{6f22}\r\n".as_bytes(),
            b"\r\n\x1b[?1049h\x1b[?1049l\r\n\r\n",
        ] {
            host.feed(bytes);
            m.feed(bytes);
            truth.feed(bytes);
        }
        m.relayout(size(6, 2));
        for bytes in [&b"aa"[..], b"aaa a aa "] {
            host.feed(bytes);
            m.feed(bytes);
            truth.feed(bytes);
        }
        m.resize(size(6, 2));
        truth.resize(size(6, 2));
        assert!(!m.ahead());
        m.select_all();
        truth.select_all();
        assert_eq!(m.selection_text(), truth.selection_text());
    }

    /// One step of a host, its stream and a viewer that lays its mirror out ahead of the host.
    #[derive(Clone, Debug)]
    enum Op {
        /// The program writes.
        Output(Vec<u8>),
        /// The view takes a size of its own and asks the host for it.
        Relayout(u16, u16),
        /// The host answers the oldest resize asked: it takes it, or keeps its size because
        /// another viewer typed last.
        Answer(bool),
        /// Another viewer's size wins.
        Other(u16, u16),
        /// The next item of the host's stream reaches the mirror.
        Deliver,
        /// The next answer reaches the mirror.
        Reply,
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            3 => output().prop_map(Op::Output),
            2 => (6u16..14, 2u16..6).prop_map(|(cols, rows)| Op::Relayout(cols, rows)),
            2 => any::<bool>().prop_map(Op::Answer),
            1 => (6u16..14, 2u16..6).prop_map(|(cols, rows)| Op::Other(cols, rows)),
            4 => Just(Op::Deliver),
            2 => Just(Op::Reply),
        ]
    }

    /// What the host's stream carries to the mirror.
    enum Item {
        History(Vec<u8>),
        Output(Vec<u8>),
        Resized(TerminalSize),
    }

    /// The stream's next item reaches the mirror, and the viewer that only follows the host.
    fn deliver(item: Item, mirror: &mut Mirror, truth: &mut Mirror) {
        match item {
            Item::History(bytes) => mirror.history(&bytes),
            Item::Output(bytes) => {
                mirror.feed(&bytes);
                truth.feed(&bytes);
            }
            Item::Resized(size) => {
                mirror.resize(size);
                truth.resize(size);
            }
        }
    }

    /// A view size the mirror shows from a snapshot of its own, rebuilt once the host takes it,
    /// keeps the soft wraps the host's terminal has: a row cloned into the wider page a column
    /// growth makes used to lose its wrap (fixed in aislopware/ghostty), so a wrapped line copied
    /// as two. The shrunk case of the proptest above.
    #[test]
    fn a_view_size_rebuilt_from_its_own_snapshot_keeps_the_hosts_soft_wraps() {
        let mut host = host(10, 4);
        let snapshot = host.snapshot().expect("a snapshot");
        let (prefix, history) = crate::snapshot::split(snapshot.clone());
        let mut mirror = Mirror::attach(&prefix, size(10, 4));
        let mut truth = Mirror::from_snapshot(&snapshot).expect("it decodes");
        mirror.relayout(size(6, 2));
        mirror.history(&history);
        for viewer in [&mut mirror, &mut truth] {
            viewer.feed(b"\x1b[?1049h");
            viewer.resize(size(6, 2));
        }
        // From the alternate screen at the host's size, the view takes a wider one of its own.
        mirror.relayout(size(7, 2));
        assert!(mirror.ahead());
        for viewer in [&mut mirror, &mut truth] {
            // Seven columns of output wrap at the host's six.
            viewer.feed(b"  a a  ");
            viewer.resize(size(7, 2));
        }
        assert!(!mirror.ahead());
        mirror.select_all();
        truth.select_all();
        assert_eq!(truth.selection_text().as_deref(), Some("  a a"));
        assert_eq!(mirror.selection_text(), truth.selection_text());
    }

    /// The host's PTY takes `to`, and its stream says so when that changed it.
    fn take(host: &mut Host, to: TerminalSize, stream: &mut VecDeque<Item>) {
        if host.size() != to {
            host.resize(to);
            stream.push_back(Item::Resized(to));
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// A mirror that lays itself out at the view's size before the host has, whatever output
        /// crosses the resizes on the way and whoever's size wins, holds the host's terminal
        /// exactly whenever it is not ahead, and is never ahead once the host has answered and
        /// its stream has arrived.
        #[test]
        fn a_mirror_ahead_of_the_host_comes_back_to_the_hosts_terminal(
            lines in 0usize..30,
            ops in proptest::collection::vec(op(), 0..40),
        ) {
            let mut host = host(10, 4);
            for n in 0..lines {
                host.feed(format!("line {n}\r\n").as_bytes());
            }
            let snapshot = host.snapshot().expect("a snapshot");
            let (prefix, history) = crate::snapshot::split(snapshot.clone());
            let mut mirror = Mirror::attach(&prefix, size(10, 4));
            // What a viewer that only ever followed the host holds, fed as the stream arrives.
            let mut truth = Mirror::from_snapshot(&snapshot).expect("it decodes");
            let mut stream = VecDeque::from([Item::History(history)]);
            let mut asked = VecDeque::new();
            let mut answers = VecDeque::new();
            for op in &ops {
                match op {
                    Op::Output(bytes) => {
                        host.feed(bytes);
                        stream.push_back(Item::Output(bytes.clone()));
                    }
                    Op::Relayout(cols, rows) => {
                        mirror.relayout(size(*cols, *rows));
                        prop_assert_eq!(mirror.size(), (*cols, *rows), "at once");
                        asked.push_back(size(*cols, *rows));
                    }
                    Op::Answer(takes) => {
                        if let Some(requested) = asked.pop_front() {
                            if *takes {
                                take(&mut host, requested, &mut stream);
                            }
                            answers.push_back((requested, host.size()));
                        }
                    }
                    Op::Other(cols, rows) => take(&mut host, size(*cols, *rows), &mut stream),
                    Op::Deliver => {
                        if let Some(item) = stream.pop_front() {
                            deliver(item, &mut mirror, &mut truth);
                        }
                    }
                    Op::Reply => {
                        if let Some((requested, pty)) = answers.pop_front() {
                            mirror.settle(requested, Some(pty));
                        }
                    }
                }
                if !mirror.ahead() && !mirror.awaiting_history() {
                    prop_assert_eq!(mirror.text(), truth.text(), "after {:?}", op);
                }
            }
            while let Some(requested) = asked.pop_front() {
                take(&mut host, requested, &mut stream);
                answers.push_back((requested, host.size()));
            }
            while let Some(item) = stream.pop_front() {
                deliver(item, &mut mirror, &mut truth);
            }
            while let Some((requested, pty)) = answers.pop_front() {
                mirror.settle(requested, Some(pty));
            }
            prop_assert!(!mirror.ahead(), "the host answered and its stream arrived");
            prop_assert_eq!(mirror.size(), truth.size());
            prop_assert_eq!(mirror.text(), truth.text());
            prop_assert_eq!(mirror.frame(|_| false).cursor, truth.frame(|_| false).cursor);
            prop_assert_eq!(cells(&mut mirror), cells(&mut truth));
            mirror.select_all();
            truth.select_all();
            prop_assert_eq!(mirror.selection_text(), truth.selection_text());
        }
    }
}
