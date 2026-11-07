//! Remote desktop's control types, what the control stream carries between a client and
//! `onicode-desktopd`: the displays a host lists, opening and shaping a stream, screen input by
//! key position, the host's events, keys and modifiers, and the clipboard's messages
//! ([`clipboard`]; the pasteboard and the sync logic are `onicode-clipboard`'s).
//!
//! Ported from slopty's `slopty-proto::{screen, input}` and `slopty-core`'s display
//! id (MIT, same authors). Whole displays only. Media itself (frames, parity, NACKs, reports,
//! clock probes) is `onicode_desktop_media::wire`, on datagrams.

#![forbid(unsafe_code)]

use std::fmt;

use serde::{Deserialize, Serialize};

pub mod clipboard;
pub mod input;
pub mod screen;

pub use onicode_desktop_media::wire::StreamId;

/// A display on the host, in the host's own numbering (the `CGDirectDisplayID` on macOS). It
/// means something only on the host that listed it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Debug)]
#[serde(transparent)]
pub struct DisplayId(pub u32);

impl fmt::Display for DisplayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "display#{}", self.0)
    }
}
