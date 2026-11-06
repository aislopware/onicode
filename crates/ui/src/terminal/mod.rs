//! Terminal panel (feature-inventory §1.10): an `alacritty_terminal`-backed
//! emulator fed by the engine's PTY stream over the generic RPC client.
//!
//! - [`emulator`] — pure Term + vte state machine (bytes in, grid out), shared with the mobile apps
//!   (`onicode-term`);
//! - [`view`] — cell palette, key/pointer/focus encodings, the ordered input queue, and the custom
//!   grid-painting element;
//! - [`predict`] — local echo prediction for terminals on other devices (`onicode-term` too);
//! - [`panel`] — session-scoped tabs, subscriptions with reconnect backoff, drag-reorder, and the
//!   Cmd/Ctrl+J toggle action.
//!
//! Method names come from `onicode_rpc::methods` and wire types from
//! `onicode_proto` (`TerminalSession`, `TerminalEvent`) — the same contract the
//! engine serves (feature-inventory §2.1).

pub(crate) mod dock;
pub mod panel;
pub mod view;
pub use onicode_term::{emulator, predict};
