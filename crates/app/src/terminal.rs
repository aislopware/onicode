//! Port of TerminalView.tsx: a login shell the engine hosts (`docs/onicode/decisions/terminal.md`),
//! mirrored here by libghostty and painted as a grid of cell-snapped text runs. Keys, paste and
//! the wheel go to the engine, which encodes them from its own terminal's modes; output comes back
//! through the terminal's watch.
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, KeyBinding,
    KeyDownEvent, Keystroke, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, ScrollDelta,
    ScrollEvent, ShapedLine, SharedString, Size, StrikethroughStyle, Style, Task, TextAlign,
    TextRun, UnderlineStyle, Window, WindowTextSystem, actions, div, point, px, rgb, size,
};
use onicode_protocol::TerminalId;
use onicode_protocol::terminal::{
    CloseTerminal, Input, Key, OpenTerminal, ReadForeground, ResizeTerminal, StopForeground,
    TerminalColors, TerminalEvent as HostEvent, TerminalInput, TerminalSize, TerminalState,
    WatchTerminal,
};
use onicode_term::label::command_label;
use onicode_term::mirror::{CellColor, CursorShape, Link, Mirror, RowId};
use onicode_term::select::{Kind, Point as GridPoint, Side};
use onicode_term::{keys, mirror, palette};

use crate::engine_client::{self, Engine, WatchUpdate};
use crate::theme::*;

mod context_menu;

actions!(
    terminal,
    [SendTab, SendBackTab, SendEscape, Copy, Paste, SelectAll]
);

pub(crate) fn bind(cx: &mut App) {
    let c = Some("Terminal");
    // The shell owns these keys while a terminal has focus.
    cx.bind_keys([
        KeyBinding::new("tab", SendTab, c),
        KeyBinding::new("shift-tab", SendBackTab, c),
        KeyBinding::new("escape", SendEscape, c),
        KeyBinding::new(os_keys!("cmd-c", "ctrl-shift-c"), Copy, c),
        KeyBinding::new(os_keys!("cmd-v", "ctrl-shift-v"), Paste, c),
        KeyBinding::new(os_keys!("cmd-a", "ctrl-shift-a"), SelectAll, c),
    ]);
}

const FONT_SIZE: f32 = 13.;
const PADDING: f32 = 8.;
/// The grid a terminal opens at, until it is first painted.
const START: TerminalSize = TerminalSize {
    cols: 80,
    rows: 24,
    cell_width: 8,
    cell_height: 16,
};
/// oscColors(): what a CLI asking for colours is told.
const OSC_COLORS: TerminalColors = TerminalColors {
    foreground: TEXT,
    background: 0x171717,
    cursor: ACCENT,
};

pub(crate) enum TerminalEvent {
    /// Title, foreground process or running state changed.
    Changed,
}

/// Where the grid was painted, for mapping the pointer to cells.
#[derive(Clone, Copy, Default)]
struct Geometry {
    origin: gpui::Point<gpui::Pixels>,
    cell: Size<gpui::Pixels>,
}

/// What the engine is asked, in the order the view asked it.
enum Outgoing {
    Input(Input),
    Resize(TerminalSize),
    StopForeground,
    Close,
}

/// The engine's side of a terminal: the calls that reach its host, and the watch its output
/// arrives on.
struct Hosted {
    engine: Engine,
    id: TerminalId,
    outbox: async_channel::Sender<Outgoing>,
    _watch: Task<()>,
}

/// What a cached row was shaped with; a change reshapes every row.
#[derive(Clone, PartialEq)]
struct Look {
    font: Font,
    cell_width: gpui::Pixels,
    line_height: gpui::Pixels,
    foreground: Hsla,
    selection: Hsla,
    link: Option<(GridPoint, GridPoint)>,
}

/// A row shaped for paint: its filled spans and text runs by column.
struct ShapedRow {
    fills: Vec<(usize, usize, Hsla)>,
    runs: Vec<(usize, ShapedLine)>,
}

pub(crate) struct Terminal {
    context_menu: Option<context_menu::EditingMenu>,
    mirror: Mirror,
    /// `None` in tests, which run the terminal without a shell behind it.
    hosted: Option<Hosted>,
    focus: FocusHandle,
    pub cwd: String,
    /// The tab label: the foreground command, else the directory.
    pub title: String,
    /// A command other than the shell holds the terminal.
    pub foreground: Option<String>,
    pub exited: bool,
    /// The grid the view last asked the engine for.
    size: TerminalSize,
    geometry: Rc<Cell<Option<Geometry>>>,
    selecting: bool,
    selection_pointer: Option<gpui::Point<gpui::Pixels>>,
    selection_scroll: Option<Task<()>>,
    scroll_remainder: f32,
    /// The pointer over the grid, for underlining the link beneath it.
    hover: Option<gpui::Point<gpui::Pixels>>,
    /// The link a plain click pressed; releasing on it opens it.
    pressed_link: Option<Link>,
    pub(crate) visible: bool,
    /// Shaped rows by libghostty's row id, valid for `look`.
    rows: HashMap<RowId, ShapedRow>,
    look: Option<Look>,
    /// Ends a synchronized update the program left open.
    sync: Option<Task<()>>,
    _poll: Task<()>,
}
impl EventEmitter<TerminalEvent> for Terminal {}
impl Focusable for Terminal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Terminal {
    pub(crate) fn new(cwd: String, cx: &mut Context<Self>) -> Self {
        // GPUI's deterministic test scheduler runs no engine threads; UI tests run the terminal
        // without a shell behind it.
        let hosted = (!cfg!(test)).then(|| Self::host(&cwd, cx));
        // TerminalView's 1s foreground poll; a hidden dock has no title to paint.
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let Ok(asked) = this.update(cx, |this, _| {
                    (this.visible && !this.exited).then(|| {
                        this.hosted
                            .as_ref()
                            .map(|hosted| (hosted.engine.clone(), hosted.id))
                    })
                }) else {
                    break;
                };
                let Some(host) = asked else {
                    continue;
                };
                let label = match host {
                    Some((engine, terminal)) => engine
                        .call::<ReadForeground>(None, ReadForeground { terminal })
                        .await
                        .ok()
                        .flatten(),
                    None => None,
                };
                if this
                    .update(cx, |this, cx| this.set_foreground(label, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            context_menu: None,
            mirror: Mirror::new(START),
            hosted,
            focus: cx.focus_handle(),
            title: default_title(&cwd),
            cwd,
            foreground: None,
            exited: false,
            size: START,
            geometry: Rc::default(),
            selecting: false,
            selection_pointer: None,
            selection_scroll: None,
            scroll_remainder: 0.,
            hover: None,
            pressed_link: None,
            visible: true,
            rows: HashMap::new(),
            look: None,
            sync: None,
            _poll: poll,
        }
    }

    /// Opens the terminal on the app's engine and watches it.
    fn host(cwd: &str, cx: &mut Context<Self>) -> Hosted {
        let engine = engine_client::local(cx);
        let id = TerminalId::new();
        let (outbox, queued) = async_channel::unbounded::<Outgoing>();
        let (opened, on_open) = async_channel::bounded::<()>(1);
        let open = OpenTerminal {
            terminal: id,
            cwd: cwd.to_owned(),
            size: START,
            colors: OSC_COLORS,
        };
        // One task makes the calls in order, the open first. It outlives the view, to deliver
        // the close its `Drop` queues.
        let caller = engine.clone();
        cx.background_executor()
            .spawn(async move {
                if caller.call::<OpenTerminal>(None, open).await.is_ok() {
                    let _ = opened.send(()).await;
                }
                drop(opened);
                // What failed is the engine's to say: the terminal's watch shows it.
                while let Ok(outgoing) = queued.recv().await {
                    let terminal = id;
                    match outgoing {
                        Outgoing::Input(input) => {
                            let _ = caller
                                .call::<TerminalInput>(None, TerminalInput { terminal, input })
                                .await;
                        }
                        Outgoing::Resize(size) => {
                            let _ = caller
                                .call::<ResizeTerminal>(None, ResizeTerminal { terminal, size })
                                .await;
                        }
                        Outgoing::StopForeground => {
                            let _ = caller
                                .call::<StopForeground>(None, StopForeground { terminal })
                                .await;
                        }
                        Outgoing::Close => {
                            let _ = caller
                                .call::<CloseTerminal>(None, CloseTerminal { terminal })
                                .await;
                            break;
                        }
                    }
                }
            })
            .detach();
        let watcher = engine.clone();
        let watch = cx.spawn(async move |this, cx| {
            if on_open.recv().await.is_err() {
                return;
            }
            let watching = watcher.watch::<WatchTerminal>(move |since| WatchTerminal {
                terminal: id,
                since,
            });
            while let Ok(update) = watching.updates.recv().await {
                // Output arrives in bursts; one repaint covers the burst.
                let mut batch = vec![update];
                while let Ok(more) = watching.updates.try_recv() {
                    batch.push(more);
                }
                if this
                    .update(cx, |this, cx| {
                        for update in batch {
                            this.apply(update, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Hosted {
            engine,
            id,
            outbox,
            _watch: watch,
        }
    }

    /// What the engine's terminal did: a snapshot to start over from, or what it did since.
    fn apply(&mut self, update: WatchUpdate<TerminalState, HostEvent>, cx: &mut Context<Self>) {
        match update {
            WatchUpdate::Snapshot(state) => {
                self.mirror = Mirror::from_snapshot(&state.screen.0)
                    .unwrap_or_else(|| Mirror::new(state.size));
                self.rows.clear();
                if state.exited {
                    self.finish(cx);
                }
            }
            WatchUpdate::Events(events) => {
                for event in events {
                    match event {
                        HostEvent::Output(bytes) => self.mirror.feed(&bytes.0),
                        HostEvent::Resized(size) => self.mirror.resize(size),
                        HostEvent::Clipboard(text) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                        HostEvent::Exited { .. } => self.finish(cx),
                    }
                }
            }
        }
        self.schedule_sync(cx);
        cx.notify();
    }

    /// Shows a synchronized update the program left open once its deadline passes.
    fn schedule_sync(&mut self, cx: &mut Context<Self>) {
        let Some(deadline) = self.mirror.sync_deadline() else {
            self.sync = None;
            return;
        };
        if self.sync.is_some() {
            return;
        }
        let wait = deadline.saturating_duration_since(Instant::now());
        self.sync = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                this.sync = None;
                if this.mirror.expire_sync(Instant::now()) {
                    cx.notify();
                }
                this.schedule_sync(cx);
            });
        }));
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if !self.exited {
            self.exited = true;
            self.set_foreground(None, cx);
            cx.emit(TerminalEvent::Changed);
        }
        cx.notify();
    }

    fn set_foreground(&mut self, label: Option<String>, cx: &mut Context<Self>) {
        if label == self.foreground {
            return;
        }
        self.title = label.clone().unwrap_or_else(|| default_title(&self.cwd));
        self.foreground = label;
        cx.emit(TerminalEvent::Changed);
    }

    /// Hangs up the job holding the terminal, leaving the shell at its prompt.
    pub(crate) fn stop_foreground(&mut self, cx: &mut Context<Self>) {
        if !self.exited {
            self.send(Outgoing::StopForeground);
        }
        self.set_foreground(None, cx);
    }

    fn send(&self, outgoing: Outgoing) {
        if let Some(hosted) = &self.hosted {
            let _ = hosted.outbox.try_send(outgoing);
        }
    }

    /// Typing returns a scrolled-back view to the prompt.
    fn input(&mut self, input: Input, cx: &mut Context<Self>) {
        self.stop_selection();
        self.mirror.scroll_to_bottom();
        self.mirror.clear_selection();
        self.send(Outgoing::Input(input));
        cx.notify();
    }

    /// Types `command` at the prompt and runs it. The footer names it at
    /// once; the foreground poll corrects the label within a second.
    pub(crate) fn run(&mut self, command: &str, cx: &mut Context<Self>) {
        let mut line = command.trim().replace("\r\n", "\r").replace('\n', "\r");
        line.push('\r');
        self.input(Input::Text(line), cx);
        if let Some(label) = command_label(command.trim()) {
            self.set_foreground(Some(label), cx);
        }
    }

    /// Fit the grid to the painted area; the engine's PTY and terminal learn the new size, and
    /// this mirror takes it where the engine's output does.
    fn resize(&mut self, columns: usize, lines: usize, cell: Size<gpui::Pixels>) {
        let next = TerminalSize {
            cols: u16::try_from(columns).unwrap_or(u16::MAX),
            rows: u16::try_from(lines).unwrap_or(u16::MAX),
            cell_width: cell.width.as_f32() as u16,
            cell_height: cell.height.as_f32() as u16,
        };
        if (next.cols, next.rows) == (self.size.cols, self.size.rows) {
            return;
        }
        self.size = next;
        if self.hosted.is_some() {
            self.send(Outgoing::Resize(next));
        } else {
            self.mirror.resize(next);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = key(&event.keystroke);
        if keys::handles(&key) {
            cx.stop_propagation();
            self.input(Input::Key(key), cx);
        }
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.mirror.selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        self.input(Input::Paste(text), cx);
    }

    fn select_all(&mut self, cx: &mut Context<Self>) {
        self.mirror.select_all();
        cx.notify();
    }

    /// The zero-based viewport line and column under a pointer.
    fn viewport_cell(&self, position: gpui::Point<gpui::Pixels>) -> (usize, usize) {
        let Some(g) = self.geometry.get() else {
            return (0, 0);
        };
        let (columns, lines) = self.mirror.size();
        let x = ((position.x - g.origin.x) / g.cell.width).max(0.);
        let y = ((position.y - g.origin.y) / g.cell.height).max(0.);
        (
            (y as usize).min(usize::from(lines).saturating_sub(1)),
            (x as usize).min(usize::from(columns).saturating_sub(1)),
        )
    }

    /// The viewport cell under a window position, and which half of it.
    fn grid_point(&self, position: gpui::Point<gpui::Pixels>) -> Option<(usize, usize, Side)> {
        let g = self.geometry.get()?;
        let x = ((position.x - g.origin.x) / g.cell.width).max(0.);
        let (line, column) = self.viewport_cell(position);
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        Some((line, column, side))
    }

    /// The HTTP(S) link under a window position, if the pointer is on a cell.
    fn link_at(&self, position: gpui::Point<gpui::Pixels>) -> Option<Link> {
        let g = self.geometry.get()?;
        let (columns, lines) = self.mirror.size();
        let x = (position.x - g.origin.x) / g.cell.width;
        let y = (position.y - g.origin.y) / g.cell.height;
        if x < 0. || y < 0. || x >= f32::from(columns) || y >= f32::from(lines) {
            return None;
        }
        self.mirror.link_at(y as usize, x as usize)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.pressed_link = (event.click_count == 1 && !event.modifiers.shift)
            .then(|| self.link_at(event.position))
            .flatten();
        let Some((line, column, side)) = self.grid_point(event.position) else {
            return;
        };
        if event.modifiers.shift && self.mirror.selecting() {
            self.mirror.extend_selection(line, column, side);
        } else {
            let kind = match event.click_count {
                2 => Kind::Word,
                3.. => Kind::Line,
                _ => Kind::Simple,
            };
            self.mirror.start_selection(kind, line, column, side);
        }
        self.selecting = true;
        self.selection_pointer = None;
        self.selection_scroll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if !this
                    .update_in(cx, |this, window, cx| {
                        if !window.is_window_active() || !this.focus.is_focused(window) {
                            this.selecting = false;
                            this.selection_pointer = None;
                            return false;
                        }
                        this.scroll_selection(cx)
                    })
                    .unwrap_or(false)
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    /// A click that pressed and released on the same link, without
    /// dragging out a selection, opens it as the web build's xterm does.
    fn mouse_up(&mut self, event: &gpui::MouseUpEvent, cx: &mut Context<Self>) {
        self.stop_selection();
        let Some(link) = self.pressed_link.take() else {
            return;
        };
        if !self.mirror.has_selection() && self.link_at(event.position).as_ref() == Some(&link) {
            cx.open_url(&link.uri);
        }
    }

    fn stop_selection(&mut self) {
        self.selecting = false;
        self.selection_pointer = None;
        self.selection_scroll = None;
    }

    /// Keep extending the same selection through scrollback at either edge,
    /// even when the pointer is held still outside the terminal.
    fn scroll_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.selecting || !self.visible {
            self.selecting = false;
            self.selection_pointer = None;
            return false;
        }
        let Some(position) = self.selection_pointer else {
            // A press without a drag must not scroll a first/last-row click.
            return true;
        };
        let Some(geometry) = self.geometry.get() else {
            return true;
        };
        let lines = f32::from(self.mirror.size().1);
        let y = (position.y - geometry.origin.y) / geometry.cell.height;
        let delta = if y < 0.5 {
            (0.5 - y).ceil().clamp(1., 10.) as i32
        } else if y > lines - 0.5 {
            -((y - lines + 0.5).ceil().clamp(1., 10.) as i32)
        } else {
            return true;
        };
        if !self.mirror.selecting() {
            return false;
        }
        if self.mirror.scroll(delta) {
            if let Some((line, column, side)) = self.grid_point(position) {
                self.mirror.extend_selection(line, column, side);
            }
            cx.notify();
        }
        true
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.stop_selection();
            return;
        }
        self.selection_pointer = Some(event.position);
        if let Some((line, column, side)) = self.grid_point(event.position) {
            self.mirror.extend_selection(line, column, side);
            cx.notify();
        }
    }

    fn scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let line = self.geometry.get().map_or(16., |g| g.cell.height.as_f32());
        let delta = match event.delta {
            ScrollDelta::Lines(lines) => lines.y,
            ScrollDelta::Pixels(pixels) => pixels.y.as_f32() / line,
        } + self.scroll_remainder;
        let lines = delta.trunc() as i32;
        self.scroll_remainder = delta.fract();
        if lines == 0 {
            return;
        }
        let modes = self.mirror.modes();
        // A program tracking the mouse gets the wheel itself, and a full-screen program scrolls
        // itself: the engine encodes which from its terminal's modes. The shell scrolls history.
        if modes.mouse || (modes.alt_screen && modes.alt_scroll) {
            let (row, col) = self.viewport_cell(event.position);
            self.send(Outgoing::Input(Input::Wheel {
                up: lines > 0,
                lines: lines.unsigned_abs(),
                row: u16::try_from(row).unwrap_or(u16::MAX),
                col: u16::try_from(col).unwrap_or(u16::MAX),
            }));
        } else if self.mirror.scroll(lines) {
            cx.notify();
        }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        // The engine hangs up the shell and its job, after whatever was still on its way.
        self.send(Outgoing::Close);
    }
}

impl Render for Terminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        div()
            .id("terminal")
            .key_context("Terminal")
            .track_focus(&self.focus)
            .size_full()
            .cursor(gpui::CursorStyle::IBeam)
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if this.hover != Some(event.position) {
                    this.hover = Some(event.position);
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && this.hover.take().is_some() {
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(|this, event, _, cx| this.key_down(event, cx)))
            .on_action(cx.listener(|this, _: &SendTab, _, cx| {
                this.input(Input::Text("\t".into()), cx);
            }))
            .on_action(cx.listener(|this, _: &SendBackTab, _, cx| {
                this.input(Input::Text("\x1b[Z".into()), cx);
            }))
            .on_action(cx.listener(|this, _: &SendEscape, _, cx| {
                this.input(Input::Text("\x1b".into()), cx);
            }))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(cx)))
            .on_action(cx.listener(|this, _: &Paste, _, cx| this.paste(cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    if cfg!(target_os = "macos") && event.modifiers.control {
                        this.open_context_menu(event, window, cx);
                    } else {
                        this.mouse_down(event, window, cx);
                    }
                }),
            )
            .on_mouse_down(MouseButton::Right, cx.listener(Self::open_context_menu))
            .on_scroll(cx.listener(|this, event, _, cx| this.scroll(event, cx)))
            .child(Grid {
                terminal: cx.entity(),
                focused,
            })
            .children(self.editing_menu(cx))
    }
}

/// One shaped run at a column, or a filled cell span.
struct Painted {
    lines: Vec<(gpui::Point<gpui::Pixels>, ShapedLine)>,
    rects: Vec<(Bounds<gpui::Pixels>, Hsla)>,
    cursor: Option<(Bounds<gpui::Pixels>, Hsla, bool)>,
    line_height: gpui::Pixels,
    /// Set while the pointer is over a link, for the pointing hand.
    link_hitbox: Option<gpui::Hitbox>,
}

struct Grid {
    terminal: Entity<Terminal>,
    focused: bool,
}
impl IntoElement for Grid {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}
/// A cell to paint: column, glyph, pen, fill and whether it is wide.
type Painting = (usize, char, Pen, Option<Hsla>, bool);
#[derive(Clone, PartialEq)]
struct Pen {
    fg: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}
impl Element for Grid {
    type PrepaintState = Painted;
    type RequestLayoutState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = gpui::relative(1.).into();
        style.size.height = gpui::relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<gpui::Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Painted {
        let font_size = px(FONT_SIZE);
        let base = crate::fonts::terminal(cx);
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&base);
        let cell_width = text_system
            .advance(font_id, font_size, 'm')
            .map_or(px(7.8), |size| size.width);
        let line_height = (text_system.ascent(font_id, font_size)
            + text_system.descent(font_id, font_size).abs())
        .ceil()
        .max(px(14.));
        let origin = point(bounds.left() + px(PADDING), bounds.top() + px(PADDING / 2.));
        let columns =
            (((bounds.size.width - px(PADDING * 2.)) / cell_width).floor() as usize).max(2);
        let lines = (((bounds.size.height - px(PADDING)) / line_height).floor() as usize).max(1);
        let cell = size(cell_width, line_height);
        let (frame, link, mut cached) = self.terminal.update(cx, |t, _| {
            t.resize(columns, lines, cell);
            t.geometry.set(Some(Geometry { origin, cell }));
            let link = t.hover.and_then(|position| t.link_at(position));
            let look = Look {
                font: base.clone(),
                cell_width,
                line_height,
                foreground: content(1.),
                selection: selection_fill(),
                link: link.as_ref().map(|link| (link.start, link.end)),
            };
            if t.look.as_ref() != Some(&look) {
                t.rows.clear();
                t.look = Some(look);
            }
            let frame = t.mirror.frame(|id| t.rows.contains_key(&id));
            (frame, link, std::mem::take(&mut t.rows))
        });
        if frame.full {
            cached.clear();
        }
        // The default foreground, unless the program set its own (OSC 10).
        let foreground: Hsla = frame
            .foreground
            .map_or_else(|| content(1.), |color| rgb(color).into());

        let mut painted = Painted {
            lines: Vec::new(),
            rects: Vec::new(),
            cursor: None,
            line_height,
            link_hitbox: link
                .as_ref()
                .map(|_| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)),
        };
        let mut rows = HashMap::with_capacity(frame.rows.len());
        for (line, row) in frame.rows.into_iter().enumerate().take(lines) {
            let shaped = match row.cells {
                Some(cells) => {
                    let screen_line = frame.top + u32::try_from(line).unwrap_or(u32::MAX);
                    let pens = pens(&cells, screen_line, link.as_ref(), foreground);
                    shape_row(&pens, &base, &text_system, font_size, cell_width)
                }
                None => match cached.remove(&row.id) {
                    Some(shaped) => shaped,
                    None => continue,
                },
            };
            let y = origin.y + line_height * line as f32;
            for (start, end, fill) in &shaped.fills {
                painted
                    .rects
                    .push((cell_rect(origin, cell, line, *start, *end), *fill));
            }
            for (column, run) in &shaped.runs {
                painted.lines.push((
                    point(origin.x + cell_width * *column as f32, y),
                    run.clone(),
                ));
            }
            rows.insert(row.id, shaped);
        }
        self.terminal.update(cx, |t, _| t.rows = rows);

        if let Some(at) = frame.cursor
            && at.row < lines
        {
            let x = origin.x + cell_width * at.col as f32;
            let y = origin.y + line_height * at.row as f32;
            let accent: Hsla = accent_color();
            let (bounds, hollow) = match (at.shape, self.focused) {
                (CursorShape::Beam, _) => {
                    (Bounds::new(point(x, y), size(px(2.), line_height)), false)
                }
                (CursorShape::Underline, _) => (
                    Bounds::new(point(x, y + line_height - px(2.)), size(cell_width, px(2.))),
                    false,
                ),
                (CursorShape::Block, true) => (Bounds::new(point(x, y), cell), false),
                _ => (Bounds::new(point(x, y), cell), true),
            };
            let mut fill = accent;
            if !self.focused {
                fill.a = 0.5;
            } else if at.shape == CursorShape::Block {
                fill.a = 0.6;
            }
            painted.cursor = Some((bounds, fill, hollow));
        }
        painted
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<gpui::Pixels>,
        _: &mut (),
        painted: &mut Painted,
        window: &mut Window,
        cx: &mut App,
    ) {
        // Element mouse handlers only receive hovered events. An active drag
        // must keep tracking movement and release anywhere in the window.
        let terminal = self.terminal.downgrade();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
            if phase == gpui::DispatchPhase::Capture {
                let _ = terminal.update(cx, |terminal, cx| terminal.mouse_move(event, cx));
            }
        });
        let terminal = self.terminal.downgrade();
        window.on_mouse_event(move |event: &gpui::MouseUpEvent, phase, _, cx| {
            if phase == gpui::DispatchPhase::Capture && event.button == MouseButton::Left {
                let _ = terminal.update(cx, |terminal, cx| terminal.mouse_up(event, cx));
            }
        });
        if let Some(hitbox) = &painted.link_hitbox {
            window.set_cursor_style(gpui::CursorStyle::PointingHand, hitbox);
        }
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for (rect, fill) in &painted.rects {
                window.paint_quad(gpui::fill(*rect, *fill));
            }
            for (origin, line) in &painted.lines {
                let _ = line.paint(
                    *origin,
                    painted.line_height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            if let Some((rect, fill, hollow)) = painted.cursor {
                if hollow {
                    window.paint_quad(gpui::outline(rect, fill, gpui::BorderStyle::Solid));
                } else {
                    window.paint_quad(gpui::fill(rect, fill));
                }
            }
        });
    }
}

/// A row's cells as monocode painted them: inverse swaps the colours (the chat surface stands in
/// for a transparent background), dim fades the text, a selection fills the cell and a hovered
/// link underlines it. Wide characters' padding paints nothing.
fn pens(cells: &[mirror::Cell], line: u32, link: Option<&Link>, foreground: Hsla) -> Vec<Painting> {
    let resolve = |color: CellColor| -> Hsla {
        match color {
            CellColor::Default => foreground,
            CellColor::Indexed(index) => rgb(palette::indexed_rgb(index)).into(),
            CellColor::Rgb(value) => rgb(value).into(),
        }
    };
    let mut row = Vec::with_capacity(cells.len());
    for (column, cell) in cells.iter().enumerate() {
        if cell.spacer {
            continue;
        }
        let mut fg = resolve(cell.fg);
        let mut bg = match cell.bg {
            CellColor::Default => None,
            other => Some(resolve(other)),
        };
        if cell.inverse {
            let back = bg.unwrap_or(rgb(CHAT).into());
            bg = Some(fg);
            fg = back;
        }
        if cell.dim {
            fg.a *= 0.66;
        }
        if cell.selected {
            bg = Some(selection_fill());
        }
        let c = if cell.hidden { ' ' } else { cell.ch };
        let at = GridPoint {
            line,
            col: u16::try_from(column).unwrap_or(u16::MAX),
        };
        row.push((
            column,
            c,
            Pen {
                fg,
                bold: cell.bold,
                italic: cell.italic,
                underline: cell.underline || link.is_some_and(|link| link.contains(at)),
                strike: cell.strike,
            },
            bg,
            cell.wide,
        ));
    }
    row
}

/// Shapes a row: backgrounds merged across equal neighbours, and text runs that break where the
/// pen changes and around wide glyphs, so each one spans its two cells.
fn shape_row(
    cells: &[Painting],
    base: &Font,
    text_system: &WindowTextSystem,
    font_size: gpui::Pixels,
    cell_width: gpui::Pixels,
) -> ShapedRow {
    let mut shaped = ShapedRow {
        fills: Vec::new(),
        runs: Vec::new(),
    };
    let mut span: Option<(usize, usize, Hsla)> = None;
    for (column, _, _, bg, wide) in cells {
        let width = if *wide { 2 } else { 1 };
        match (span, bg) {
            (Some((start, end, fill)), Some(bg)) if end == *column && fill == *bg => {
                span = Some((start, end + width, fill));
            }
            (_, bg) => {
                if let Some(fill) = span.take() {
                    shaped.fills.push(fill);
                }
                span = bg.map(|bg| (*column, column + width, bg));
            }
        }
    }
    if let Some(fill) = span {
        shaped.fills.push(fill);
    }
    let mut ix = 0;
    while ix < cells.len() {
        let (column, _, pen, _, wide) = &cells[ix];
        let mut text = String::new();
        let mut end = ix;
        while end < cells.len()
            && cells[end].2 == *pen
            && !cells[end].4
            && (end == ix || !wide)
            && cells[end].0 == column + (end - ix)
        {
            text.push(cells[end].1);
            end += 1;
        }
        if end == ix {
            text.push(cells[ix].1);
            end = ix + 1;
        }
        if !text.trim().is_empty() {
            let mut font = base.clone();
            if pen.bold {
                font.weight = FontWeight::BOLD;
            }
            if pen.italic {
                font.style = FontStyle::Italic;
            }
            let run = TextRun {
                len: text.len(),
                font,
                color: pen.fg,
                background_color: None,
                underline: pen.underline.then_some(UnderlineStyle {
                    thickness: px(1.),
                    color: Some(pen.fg),
                    wavy: false,
                }),
                strikethrough: pen.strike.then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(pen.fg),
                }),
            };
            let width = if *wide { cell_width * 2. } else { cell_width };
            let line =
                text_system.shape_line(SharedString::from(text), font_size, &[run], Some(width));
            shaped.runs.push((*column, line));
        }
        ix = end;
    }
    shaped
}

fn cell_rect(
    origin: gpui::Point<gpui::Pixels>,
    cell: Size<gpui::Pixels>,
    line: usize,
    start: usize,
    end: usize,
) -> Bounds<gpui::Pixels> {
    Bounds::new(
        point(
            origin.x + cell.width * start as f32,
            origin.y + cell.height * line as f32,
        ),
        size(cell.width * (end - start) as f32, cell.height),
    )
}

fn selection_fill() -> Hsla {
    crate::theme::text_selection()
}

/// A keystroke as the engine's terminal reads it.
fn key(keystroke: &Keystroke) -> Key {
    let m = &keystroke.modifiers;
    Key {
        key: keystroke.key.clone(),
        key_char: keystroke.key_char.clone(),
        shift: m.shift,
        alt: m.alt,
        control: m.control,
        platform: m.platform,
    }
}

/// The supported agent CLI a foreground label names, as a provider id, so
/// its tab can show the provider's mark instead of the terminal icon.
pub(crate) fn agent_cli(label: &str) -> Option<&'static str> {
    // Script launchers report the script: `codex.js` is Codex.
    let name = label.split('.').next()?;
    match name {
        "cursor-agent" | "agent" => return Some("cursor"),
        "pi-coding-agent" => return Some("pi"),
        _ => {}
    }
    onicode_core::provider::Kind::parse(name).map(|kind| kind.id())
}
impl Terminal {
    /// The agent CLI holding this terminal, if any.
    pub(crate) fn agent(&self) -> Option<&'static str> {
        agent_cli(self.foreground.as_deref()?)
    }
}

fn default_title(cwd: &str) -> String {
    let name = std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.is_empty() || name == "/" {
        "Terminal".into()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext, Modifiers};

    use super::*;

    fn bytes(spec: &'static str) -> Option<Vec<u8>> {
        keys::legacy(&key(&Keystroke::parse(spec).unwrap()), false)
    }

    /// Writes into the grid as the engine's output would.
    fn write_note(terminal: &mut Terminal, text: &str) {
        terminal.mirror.feed(text.as_bytes());
    }

    fn p(line: u32, col: u16) -> GridPoint {
        GridPoint { line, col }
    }

    #[gpui::test]
    fn links_cover_plain_urls_and_osc8_hyperlinks(cx: &mut gpui::TestAppContext) {
        let view = cx.new(|cx| Terminal::new("/tmp".into(), cx));
        view.update(cx, |terminal, _| {
            let cell = size(px(8.), px(16.));
            terminal.resize(20, 4, cell);
            terminal.geometry.set(Some(Geometry {
                origin: point(px(0.), px(0.)),
                cell,
            }));
            // A URL wrapping onto the next row, trailing punctuation
            // excluded; then an OSC 8 link and a file:// one.
            write_note(
                terminal,
                "see https://example.com/a/b. ok\r\n\x1b]8;;https://x.dev\x1b\\docs\x1b]8;;\x1b\\ \
                 \x1b]8;;file:///tmp\x1b\\tmp\x1b]8;;\x1b\\ ftp://a.b",
            );
            let at = |line: usize, column: usize| {
                point(px(column as f32 * 8. + 1.), px(line as f32 * 16. + 1.))
            };
            let url = terminal.link_at(at(1, 2)).expect("wrapped URL");
            assert_eq!(url.uri, "https://example.com/a/b");
            assert_eq!((url.start, url.end), (p(0, 4), p(1, 6)));
            assert_eq!(terminal.link_at(at(0, 4)), Some(url));
            assert_eq!(terminal.link_at(at(0, 1)), None);
            assert_eq!(terminal.link_at(at(1, 7)), None);

            let docs = terminal.link_at(at(2, 1)).expect("OSC 8 link");
            assert_eq!(docs.uri, "https://x.dev");
            assert_eq!((docs.start, docs.end), (p(2, 0), p(2, 3)));
            assert_eq!(terminal.link_at(at(2, 6)), None, "file:// stays inert");
            assert_eq!(terminal.link_at(at(2, 10)), None, "ftp:// stays inert");
            assert_eq!(terminal.link_at(point(px(-4.), px(1.))), None);
        });
    }

    #[gpui::test]
    fn dragging_at_edges_extends_selection_through_history(cx: &mut gpui::TestAppContext) {
        let view = cx.new(|cx| Terminal::new("/tmp".into(), cx));
        view.update(cx, |terminal, cx| {
            let cell = size(px(8.), px(16.));
            terminal.resize(40, 4, cell);
            terminal.geometry.set(Some(Geometry {
                origin: point(px(0.), px(0.)),
                cell,
            }));
            let output = (0..30)
                .map(|n| format!("line-{n:02}"))
                .collect::<Vec<_>>()
                .join("\r\n");
            write_note(terminal, &output);
            terminal
                .mirror
                .start_selection(Kind::Simple, 3, 6, Side::Right);
            terminal.selecting = true;
            // Holding a click alone must not scroll.
            assert!(terminal.scroll_selection(cx));
            assert_eq!(terminal.mirror.display_offset(), 0);
            terminal.selection_pointer = Some(point(px(0.), px(0.)));
            assert!(terminal.scroll_selection(cx));
            assert_eq!(terminal.mirror.display_offset(), 1);
            let text = terminal.mirror.selection_text().unwrap();
            assert!(text.contains("line-25"), "{text}");
            assert!(text.contains("line-29"), "{text}");
            // A second tick at the same pointer position must keep extending.
            assert!(terminal.scroll_selection(cx));
            assert!(
                terminal
                    .mirror
                    .selection_text()
                    .unwrap()
                    .contains("line-24")
            );
            terminal.selection_pointer = Some(point(px(0.), px(32.)));
            terminal.scroll_selection(cx);
            assert_eq!(terminal.mirror.display_offset(), 2);
            // Dragging past the bottom returns toward the live screen.
            terminal.selection_pointer = Some(point(px(0.), px(80.)));
            terminal.scroll_selection(cx);
            assert_eq!(terminal.mirror.display_offset(), 0);
            terminal.stop_selection();
            assert!(!terminal.scroll_selection(cx));
        });
    }

    #[gpui::test]
    fn cells_paint_as_monocode_painted_them(cx: &mut gpui::TestAppContext) {
        let view = cx.new(|cx| Terminal::new("/tmp".into(), cx));
        view.update(cx, |terminal, _| {
            terminal.resize(10, 2, size(px(8.), px(16.)));
            write_note(terminal, "\x1b[7ma\x1b[0m\x1b[2;31mb\x1b[0m\x1b[8mc");
            let frame = terminal.mirror.frame(|_| false);
            let cells = frame.rows[0].cells.clone().unwrap();
            let foreground = content(1.);
            let row = pens(&cells, frame.top, None, foreground);
            // Inverse on the default colours: the chat surface on the text colour.
            assert_eq!(row[0].2.fg, Hsla::from(rgb(CHAT)));
            assert_eq!(row[0].3, Some(foreground));
            let mut red: Hsla = rgb(palette::indexed_rgb(1)).into();
            red.a *= 0.66;
            assert_eq!(row[1].2.fg, red);
            assert_eq!(row[1].3, None);
            assert_eq!(row[2].1, ' ');
        });
    }

    #[test]
    fn keystrokes_reach_the_terminal_as_monocode_sent_them() {
        assert_eq!(bytes("enter"), Some(b"\r".to_vec()));
        assert_eq!(bytes("shift-right"), Some(b"\x1b[1;2C".to_vec()));
        assert_eq!(bytes("ctrl-c"), Some(vec![3]));
        assert_eq!(bytes("alt-left"), Some(b"\x1bb".to_vec()));
        assert_eq!(bytes("cmd-backspace"), Some(b"\x15".to_vec()));
        assert_eq!(bytes("alt-b"), Some(b"\x1bb".to_vec()));
        // App shortcuts stay with the app.
        assert_eq!(bytes("cmd-j"), None);
        assert_eq!(bytes("cmd-shift-d"), None);
        let typed = Keystroke {
            modifiers: Modifiers::default(),
            key: "a".into(),
            key_char: Some("a".into()),
        };
        assert_eq!(keys::legacy(&key(&typed), false), Some(b"a".to_vec()));
    }

    #[test]
    fn agent_clis_map_to_their_provider() {
        assert_eq!(agent_cli("claude"), Some("claude"));
        assert_eq!(agent_cli("codex.js"), Some("codex"));
        assert_eq!(agent_cli("cursor-agent"), Some("cursor"));
        assert_eq!(agent_cli("pi-coding-agent"), Some("pi"));
        assert_eq!(agent_cli("vim"), None);
    }

    #[test]
    fn tabs_are_titled_by_their_directory() {
        assert_eq!(default_title("/work/onicode"), "onicode");
        assert_eq!(default_title("/"), "Terminal");
    }
}
