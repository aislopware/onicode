//! The clipboard that follows a remote desktop window (`docs/onicode/decisions/remote-desktop.md`,
//! "The clipboard follows the window"; the design is `one-machine.md`'s "Clipboard").
//!
//! Lazy, as RDP's clipboard channel and X11 selections are: a copy is announced, and its bytes
//! move when someone pastes. Items up to 16 KiB ride inside the offer, so most pastes of text
//! are instant.
//!
//! - [`Board`] is the pasteboard seam: `MacBoard` (`NSPasteboard`, with lazy promises) on macOS,
//!   [`Held`] in memory in every test.
//! - [`Clipboard`] is one Mac's side, pure behind a [`Board`], [`Peers`] and a [`Clock`]: it
//!   watches the board while a session follows, announces offers, applies the other Mac's newer
//!   ones as promises and serves its own to the other Mac that pastes.
//! - [`Hub`] and [`Session`] put it on desktop sessions: one hub per Mac, one session per link,
//!   following while the viewer's window has focus; [`link`] carries it on the desktop link.
//! - [`types`] says which pasteboard types travel and which markers keep a copy home.
//!
//! Nothing here reads the general pasteboard in a test: tests use [`Held`], and the `MacBoard`
//! tests are opt-in and write only uniquely named pasteboards.

mod board;
mod bridge;
pub mod files;
pub mod link;
#[cfg(target_os = "macos")]
mod mac;
mod session;
mod sync;
pub mod types;

#[cfg(target_os = "macos")]
pub use board::serve_main_run_loop;
pub use board::{Board, Capped, Held, Item, Provide};
#[cfg(target_os = "macos")]
pub use mac::MacBoard;
pub use onicode_desktop_proto::clipboard::{
    Clip, ClipboardItem, ClipboardOffer, ClipboardType, Fetch, INLINE_MAX, Inline, PasteAccess,
};
pub use session::{ACCESS_EVERY, Hub, HubConfig, Line, Served, Session};
pub use sync::{
    Access, Clipboard, ClipboardSettings, Clock, Config, FETCH_DEADLINE, FETCH_MAX, FILE_DEADLINE,
    Key, POLL, PREFETCH_MAX, Peers, Received, SystemClock,
};
