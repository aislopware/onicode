//! The clipboard that follows a remote desktop window (`docs/onicode/decisions/remote-desktop.md`,
//! "The clipboard follows the window"; the design is `one-machine.md`'s "Clipboard").
//!
//! Eager and capped: a copy goes to the other Mac when it is made, so a paste there never waits
//! on the link. Items up to 16 KiB ride inside the offer; the rest of a copy, up to
//! [`SEND_MAX`] of bytes and [`FILES_MAX`] of regular files, is fetched at once on streams of
//! its own and written whole; past the caps it stays on its Mac.
//!
//! - [`Board`] is the pasteboard seam: `MacBoard` (`NSPasteboard`) on macOS, [`Held`] in memory in
//!   every test.
//! - [`Clipboard`] is one Mac's side, pure behind a [`Board`], [`Peers`] and a [`Clock`]: it
//!   watches the board while a session follows, announces offers, takes the other Mac's newer ones
//!   whole and serves its own copy's bytes to the other Mac.
//! - [`Hub`] and [`Session`] put it on desktop sessions: one hub per Mac, one session per link,
//!   following while the viewer's window has focus; [`link`] carries it on the desktop link.
//! - [`types`] says which pasteboard types travel and which markers keep a copy home.
//!
//! Nothing here reads the general pasteboard in a test: tests use [`Held`], and the `MacBoard`
//! tests are opt-in and write only uniquely named pasteboards.

mod board;
pub mod files;
pub mod link;
#[cfg(target_os = "macos")]
mod mac;
mod session;
mod sync;
pub mod types;

pub use board::{Board, Capped, Held, Item};
#[cfg(target_os = "macos")]
pub use mac::MacBoard;
pub use onicode_desktop_proto::clipboard::{
    Clip, ClipboardItem, ClipboardOffer, ClipboardType, Fetch, INLINE_MAX, Inline, PasteAccess,
};
pub use session::{ACCESS_EVERY, Hub, HubConfig, Line, Served, Session};
pub use sync::{
    Access, Clipboard, ClipboardSettings, Clock, Config, FETCH_DEADLINE, FILES_MAX, Key, POLL,
    Peers, Received, SEND_MAX, SystemClock,
};
