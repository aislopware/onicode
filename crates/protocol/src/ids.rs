//! The protocol's ids. An [`EngineId`] is the tailnet device id bound at admission; the rest are
//! UUIDv7, minted by whoever creates the thing (usually the viewer), so a new chat, a sent prompt
//! or a queued message has its final id before any byte leaves.
use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An engine: the tailnet device id its link was admitted under (`transport.md`), the same string
/// `onicode-peer` keys its directory and links by.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EngineId(pub String);

impl fmt::Display for EngineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

macro_rules! uuid_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A fresh id. UUIDv7, so ids sort by when they were minted.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            /// A fresh id, as [`Self::new`].
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

uuid_id! {
    /// A session, on whichever engine hosts it.
    SessionId
}

uuid_id! {
    /// A prompt, steer or queued message: a viewer's echo of it is replaced in place.
    MessageId
}

uuid_id! {
    /// A mutating call: the host answers a repeat with its first receipt.
    OpId
}

uuid_id! {
    /// A terminal on its engine.
    TerminalId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_v7_and_sort_by_mint_order() {
        let first = SessionId::new();
        let second = SessionId::new();
        assert_eq!(first.0.get_version_num(), 7);
        assert!(first < second, "{first} then {second}");
    }

    #[test]
    fn ids_are_bare_strings_on_the_wire() {
        let id = OpId(Uuid::from_u128(0x0190_0000_0000_7000_8000_0000_0000_0001));
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"01900000-0000-7000-8000-000000000001\""
        );
        let engine = EngineId("nTailnetDevice".into());
        assert_eq!(
            serde_json::to_string(&engine).unwrap(),
            "\"nTailnetDevice\""
        );
    }
}
