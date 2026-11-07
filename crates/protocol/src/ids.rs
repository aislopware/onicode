//! The protocol's ids. An [`EngineId`] is the tailnet device id bound at admission; the rest are
//! UUIDv7, minted by whoever creates the thing (usually the viewer), so a new chat, a sent prompt
//! or a queued message has its final id before any byte leaves. A session or a project on any
//! engine is named by its engine and its own id or path: [`SessionKey`] and [`ProjectRef`].
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// An engine: the tailnet device id its link was admitted under (`transport.md`), the same string
/// `onicode-peer` keys its directory and links by. An engine on the tailnet takes its node's
/// stable id; one that never joined it keeps the id it last had, else its host name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EngineId(pub String);

impl fmt::Display for EngineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A session on any engine: the engine that hosts it and the session's own id there.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionKey {
    pub engine: EngineId,
    pub session: SessionId,
}

impl SessionKey {
    /// `body` on its way to the session's engine, under `op` when it mutates.
    #[must_use]
    pub fn routed<T>(&self, op: Option<OpId>, body: T) -> crate::Routed<T> {
        crate::Routed {
            engine: Some(self.engine.clone()),
            op,
            body,
        }
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.engine, self.session)
    }
}

/// A project on any engine: the engine whose disk holds it and the path there, as that engine
/// spells it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProjectRef {
    pub engine: EngineId,
    pub path: String,
}

/// How the app spells a [`ProjectRef`] on another engine: `peer://<engine>/<path>`.
const PEER_SCHEME: &str = "peer://";

impl ProjectRef {
    /// The app's spelling of the project, or of a path inside it: `peer://`, the engine id with
    /// `%` and `/` escaped (`%25`, `%2F`) so any id reads back whole, then the path, which is
    /// absolute as every engine names its folders. A project on the app's own engine is spelled
    /// as its plain path instead, which never starts with the scheme.
    #[must_use]
    pub fn uri(&self) -> String {
        let engine = self.engine.0.replace('%', "%25").replace('/', "%2F");
        format!("{PEER_SCHEME}{engine}{}", self.path)
    }

    /// The project (or path inside one) `text` spells with [`Self::uri`]; `None` for a plain
    /// path, or for a URI without an engine or an absolute path.
    #[must_use]
    pub fn parse_uri(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(PEER_SCHEME)?;
        let (engine, path) = rest.split_at(rest.find('/')?);
        if engine.is_empty() {
            return None;
        }
        Some(Self {
            engine: EngineId(engine.replace("%2F", "/").replace("%25", "%")),
            path: path.to_owned(),
        })
    }

    /// `body` on its way to the project's engine, under `op` when it mutates.
    #[must_use]
    pub fn routed<T>(&self, op: Option<OpId>, body: T) -> crate::Routed<T> {
        crate::Routed {
            engine: Some(self.engine.clone()),
            op,
            body,
        }
    }
}

impl fmt::Display for ProjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.engine, self.path)
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

        impl FromStr for $name {
            type Err = uuid::Error;

            /// The id its `Display` wrote.
            fn from_str(text: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(text).map(Self)
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

    #[test]
    fn a_key_routes_to_its_engine() {
        let key = SessionKey {
            engine: EngineId("nMac".into()),
            session: SessionId::new(),
        };
        let routed = key.routed(None, 7);
        assert_eq!(routed.engine, Some(EngineId("nMac".into())));
        assert_eq!(routed.body, 7);
        let project = ProjectRef {
            engine: EngineId("nMac".into()),
            path: "/code".into(),
        };
        assert_eq!(project.to_string(), "nMac:/code");
    }

    #[test]
    fn a_peer_project_reads_as_a_uri() {
        let project = ProjectRef {
            engine: EngineId("nMacStudio".into()),
            path: "/Users/me/code/app".into(),
        };
        assert_eq!(project.uri(), "peer://nMacStudio/Users/me/code/app");
        assert_eq!(ProjectRef::parse_uri(&project.uri()), Some(project));
        // An id kept off the tailnet is the host name, which may hold a slash.
        let odd = ProjectRef {
            engine: EngineId("Me/50% Mac".into()),
            path: "/".into(),
        };
        assert_eq!(odd.uri(), "peer://Me%2F50%25 Mac/");
        assert_eq!(ProjectRef::parse_uri(&odd.uri()), Some(odd));
        for plain in [
            "/Users/me/code",
            "~",
            "",
            "peer://",
            "peer:///code",
            "peer://nMac",
        ] {
            assert_eq!(ProjectRef::parse_uri(plain), None, "{plain}");
        }
    }

    mod uri {
        use proptest::prelude::*;

        use crate::ids::{EngineId, PEER_SCHEME, ProjectRef};

        proptest! {
            /// Every engine id and absolute path, spaces and any script included, reads back
            /// whole.
            #[test]
            fn round_trips(engine in "\\PC{1,24}", path in "(/[^/]{0,12}){1,6}") {
                let project = ProjectRef { engine: EngineId(engine), path };
                let uri = project.uri();
                prop_assert!(uri.starts_with(PEER_SCHEME));
                prop_assert_eq!(ProjectRef::parse_uri(&uri), Some(project));
            }

            /// A path inside a project is the project's URI and the rest of the path.
            #[test]
            fn a_file_extends_its_project(path in "(/[^/]{1,12}){1,4}", file in "(/[^/]{1,12}){1,3}") {
                let project = ProjectRef { engine: EngineId("nMac".into()), path: path.clone() };
                let inside = ProjectRef { engine: EngineId("nMac".into()), path: format!("{path}{file}") };
                prop_assert_eq!(inside.uri(), format!("{}{file}", project.uri()));
            }

            /// A plain path, absolute or home-relative, is never a peer URI.
            #[test]
            fn plain_paths_are_not_uris(path in "[/~]\\PC{0,40}") {
                prop_assert_eq!(ProjectRef::parse_uri(&path), None);
            }
        }
    }
}
