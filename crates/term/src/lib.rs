//! Onicode's terminals on libghostty-vt.
//!
//! The engine hosts every terminal: a [`pty`] running the login shell [`shell`] builds, and a
//! [`host::Host`], the one libghostty terminal that answers the program's queries and encodes the
//! viewers' input against its modes and keeps the program's OSC 7501 records ([`program`]). A
//! viewer attaches with the host's snapshot, its renderable prefix first ([`snapshot`]), and then
//! follows its output; its [`mirror::Mirror`] replays both with PTY write-back off and hands the
//! view rows to paint.
pub mod host;
pub mod keys;
pub mod label;
pub mod links;
pub mod mirror;
pub mod palette;
pub mod program;
pub mod pty;
pub mod select;
pub mod shell;
pub mod snapshot;

use libghostty_vt::Terminal;
use libghostty_vt::terminal::CursorStyle;

/// TerminalView's `scrollback`, kept by the host and every mirror.
pub const SCROLLBACK_LINES: usize = 2000;

/// What the host and every mirror start with: TerminalView's scrollback, palette and blinking
/// beam. A program changes these for itself; these are what a reset returns to.
fn configure(term: &mut Terminal<'static, 'static>) {
    let _ = term.set_scrollback_max_lines(Some(SCROLLBACK_LINES));
    let _ = term.set_default_color_palette(Some(palette::palette()));
    let _ = term.set_default_cursor_style(Some(CursorStyle::Bar));
    let _ = term.set_default_cursor_blink(Some(true));
}

/// libghostty fails to make a terminal only when it cannot allocate.
fn oom() -> ! {
    std::alloc::handle_alloc_error(std::alloc::Layout::new::<[u8; 4096]>())
}
