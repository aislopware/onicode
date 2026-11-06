//! The daemon's change notifications (`GET /localapi/v0/watch-ipn-bus?mask=…`, `ipn.Notify`).
//!
//! The daemon streams one JSON object per line. onicode reads a notification as "something
//! changed" and reads `status` again, which answers in well under a millisecond: the shape of a
//! peer patch differs between daemon versions, and `status` is the one answer read everywhere.
//! A peer's `Online` is a hint from the control plane; whether a link is down is the link's
//! own liveness to decide.

use serde::Deserialize;

use crate::BackendState;

/// `ipn.NotifyInitialStatus`: the first notification carries the state and status.
pub const NOTIFY_INITIAL_STATUS: u64 = 1 << 14;
/// `ipn.NotifyPeerChanges`: peers added or removed.
pub const NOTIFY_PEER_CHANGES: u64 = 1 << 12;
/// `ipn.NotifyPeerPatches`: a peer's fields changed (`Online` among them).
pub const NOTIFY_PEER_PATCHES: u64 = 1 << 15;
/// The mask onicode watches with.
pub const MASK: u64 = NOTIFY_INITIAL_STATUS | NOTIFY_PEER_CHANGES | NOTIFY_PEER_PATCHES;

/// One notification, reduced to what onicode reads of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notify {
    /// The daemon's new state, when the notification carries one.
    pub state: Option<BackendState>,
    /// What the daemon reported as an error, when it did.
    pub error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Raw {
    #[serde(default)]
    state: Option<i64>,
    #[serde(default)]
    err_message: Option<String>,
}

impl Notify {
    /// One line of the watch; `None` for a blank line or one that is not a JSON object.
    #[must_use]
    pub fn parse(line: &[u8]) -> Option<Self> {
        let line = line.trim_ascii();
        if line.is_empty() {
            return None;
        }
        let raw: Raw = serde_json::from_slice(line).ok()?;
        Some(Self {
            state: raw.state.map(BackendState::from_ipn),
            error: raw.err_message.filter(|e| !e.is_empty()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_reads_its_state_and_error_and_nothing_else_matters() {
        assert_eq!(
            Notify::parse(b"{\"State\":6}\n").unwrap().state,
            Some(BackendState::Running)
        );
        assert_eq!(
            Notify::parse(b"{\"State\":2}").unwrap().state,
            Some(BackendState::NeedsLogin)
        );
        assert_eq!(
            Notify::parse(b"{\"State\":99}").unwrap().state,
            Some(BackendState::Other)
        );
        let patch = Notify::parse(br#"{"PeerChanges":[{"NodeID":3,"Online":false}]}"#).unwrap();
        assert_eq!(
            patch,
            Notify::default(),
            "a change with nothing read is still a change"
        );
        let err = Notify::parse(br#"{"ErrMessage":"gone"}"#).unwrap();
        assert_eq!(err.error.as_deref(), Some("gone"));
        assert!(Notify::parse(b"  \n").is_none());
        assert!(Notify::parse(b"<html>").is_none());
        assert_eq!(MASK, 0xd000);
    }
}
