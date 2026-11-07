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

use crate::select::{self, Grid, Kind, Point, Selection, Side};
use crate::{links, palette};

/// Output a mirror replays onto its whole snapshot when the history arrives. Past this the
/// history is given up and the mirror keeps the screen and the output since.
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

/// What changed a mirror after its snapshot's prefix, to replay onto the whole snapshot.
enum Step {
    Feed(Vec<u8>),
    Resize(TerminalSize),
}

/// A snapshot's prefix, waiting for its history.
struct Restore {
    screen: Vec<u8>,
    since: Vec<Step>,
    bytes: usize,
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
    /// Set from [`Self::attach`] until the history arrives.
    restore: Option<Restore>,
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
        let term = Decoder::new_buf(bytes).ok()?.decode().ok()?;
        Some(Self::wrap(term))
    }

    /// The host's terminal from its snapshot's renderable prefix (through READY), to paint at
    /// once; the older scrollback follows through [`Self::history`]. A blank terminal of `size`
    /// when the bytes are empty or not a snapshot.
    #[must_use]
    pub fn attach(screen: &[u8], size: TerminalSize) -> Self {
        let term = Decoder::new_buf(screen)
            .ok()
            .and_then(|decoder| decoder.ready().ok())
            .map(libghostty_vt::snapshot::IncrementalDecoder::into_terminal);
        let Some(term) = term else {
            return Self::new(size);
        };
        let mut mirror = Self::wrap(term);
        mirror.restore = Some(Restore {
            screen: screen.to_vec(),
            since: Vec::new(),
            bytes: 0,
        });
        mirror
    }

    /// The rest of the snapshot [`Self::attach`] started from. The mirror becomes the whole
    /// snapshot with the output and resizes since replayed, scrolled back as far as it was; a
    /// history that does not decode leaves it as it is.
    pub fn history(&mut self, history: &[u8]) {
        let Some(Restore {
            mut screen, since, ..
        }) = self.restore.take()
        else {
            return;
        };
        screen.extend_from_slice(history);
        let Some(mut whole) = Self::from_snapshot(&screen) else {
            return;
        };
        for step in since {
            match step {
                Step::Feed(bytes) => whole.feed(&bytes),
                Step::Resize(size) => whole.resize(size),
            }
        }
        let back = self.display_offset();
        if back > 0 {
            whole.scroll(i32::try_from(back).unwrap_or(i32::MAX));
        }
        *self = whole;
    }

    /// Older scrollback is still to come.
    #[must_use]
    pub const fn awaiting_history(&self) -> bool {
        self.restore.is_some()
    }

    /// Keeps a step to replay when the history arrives.
    fn remember(&mut self, step: Step) {
        let Some(restore) = &mut self.restore else {
            return;
        };
        restore.bytes += match &step {
            Step::Feed(bytes) => bytes.len(),
            Step::Resize(_) => 0,
        };
        if restore.bytes > MAX_REPLAY_BYTES {
            self.restore = None;
        } else {
            restore.since.push(step);
        }
    }

    fn wrap(mut term: Terminal<'static, 'static>) -> Self {
        crate::configure(&mut term);
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
            restore: None,
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
        if self.restore.is_some() {
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

    /// The PTY took a new size.
    pub fn resize(&mut self, size: TerminalSize) {
        let _ = self.term.resize(
            size.cols.max(2),
            size.rows.max(1),
            u32::from(size.cell_width.max(1)),
            u32::from(size.cell_height.max(1)),
        );
        self.show_selection();
        self.remember(Step::Resize(size));
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
}
