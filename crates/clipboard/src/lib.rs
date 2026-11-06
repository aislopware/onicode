//! The clipboard that follows the owner between machines (`docs/onicode/decisions/one-machine.md`).
//!
//! Lazy, as RDP's clipboard channel and X11 selections are: a copy is announced, and its bytes
//! move when someone pastes. Items up to 16 KiB ride inside the offer, so most pastes of text
//! are instant.
//!
//! - [`Board`] is the pasteboard seam: `MacBoard` (`NSPasteboard`, with lazy promises) on macOS,
//!   [`Held`] in memory elsewhere and in every test.
//! - [`Clipboard`] is one engine's side, pure behind a [`Board`], [`Peers`] and a [`Clock`]: it
//!   watches the board while someone listens, announces offers, applies peers' newer ones as
//!   promises, serves its own to a peer that pastes, and follows a remote desktop session.
//! - [`types`] says which pasteboard types travel and which markers keep a copy home.
//!
//! Nothing here reads the general pasteboard in a test: tests use [`Held`], and the `MacBoard`
//! tests are opt-in and write only uniquely named pasteboards.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

mod board;
mod bridge;
pub mod files;
#[cfg(target_os = "macos")]
mod mac;
mod sync;
pub mod types;

pub use board::{Board, Capped, Held, Item, Provide, serve_main_run_loop};
#[cfg(target_os = "macos")]
pub use mac::MacBoard;
pub use onicode_proto::clipboard::{
    ClipboardFetchRequest, ClipboardItem, ClipboardOffer, ClipboardSettings,
    ClipboardSettingsRequest, ClipboardType, INLINE_MAX,
};
pub use sync::{
    Access, Clipboard, Clock, Config, FETCH_DEADLINE, FETCH_MAX, FILE_DEADLINE, Key, POLL,
    PREFETCH_MAX, PasteAccess, Peers, Received, SystemClock,
};
