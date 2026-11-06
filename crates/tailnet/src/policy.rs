//! Which callers onicode lets in, and in what role, from what the tailnet says of them.
//!
//! Ported from slopty's `slopty-tailnet` `policy.rs` (MIT), with onicode's roles.
//!
//! * A node of the same user as this one is the owner's own machine: every role.
//! * Any other node, a tagged one included (a VPS `tag:onicode`), needs a grant carrying the
//!   capability [`CAP`] with the roles it may take, e.g. in the tailnet policy:
//!
//!   ```jsonc
//!   "grants": [{ "src": ["tag:onicode"], "dst": ["autogroup:member"], "ip": ["*"],
//!                "app": { "github.com/aislopware/onicode": [{ "roles": ["engine", "viewer"] }] }
//! }]   ```
//!
//!   Tailscale's control plane, Headscale from 0.29 and slopscale hand such grants to the
//!   destination verbatim. The name is a domain onicode controls, since `tailscale.com/…` is
//!   reserved.
//! * Everything else is refused.

use serde::{Deserialize, Serialize};

use crate::WhoIs;

/// The application capability a tailnet grant gives onicode's roles under.
pub const CAP: &str = "github.com/aislopware/onicode";

/// What a peer may be to an onicode engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Another engine: it serves what it owns and replicates with this one.
    Engine,
    /// A thin client (a phone, the CLI): it reads and drives, and serves nothing.
    Viewer,
}

/// A grant's value under [`CAP`]: `{ "roles": [...] }`.
#[derive(Deserialize)]
struct Value {
    #[serde(default)]
    roles: Vec<Role>,
}

/// The roles a caller may take, and whether it is the owner's own machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Grant {
    /// The same tailnet user as this node.
    pub owner: bool,
    engine: bool,
    viewer: bool,
}

impl Grant {
    /// The owner's own machines: every role.
    pub const OWNER: Self = Self {
        owner: true,
        engine: true,
        viewer: true,
    };

    /// Whether the caller may take `role`.
    #[must_use]
    pub const fn allows(self, role: Role) -> bool {
        match role {
            Role::Engine => self.engine,
            Role::Viewer => self.viewer,
        }
    }

    /// Whether the caller may take any role at all.
    #[must_use]
    pub const fn any(self) -> bool {
        self.engine || self.viewer
    }

    /// This grant with `role` added.
    #[must_use]
    pub const fn with(mut self, role: Role) -> Self {
        match role {
            Role::Engine => self.engine = true,
            Role::Viewer => self.viewer = true,
        }
        self
    }

    /// What the tailnet lets `caller` do on a node owned by `owner` (`None` when this node is
    /// tagged and so owned by nobody).
    #[must_use]
    pub fn of(caller: &WhoIs, owner: Option<i64>) -> Self {
        if !caller.tagged() && owner == Some(caller.user_profile.id) {
            return Self::OWNER;
        }
        caller
            .cap_map
            .get(CAP)
            .into_iter()
            .flatten()
            .filter_map(|v| Value::deserialize(v).ok())
            .flat_map(|v| v.roles)
            .fold(Self::default(), Self::with)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::whois::tests::{GRANTED, MINE};

    fn whois(json: &str) -> WhoIs {
        serde_json::from_str(json).unwrap()
    }

    /// The user's own machine may do anything; the same machine on someone else's node, or
    /// on a tagged node, may do nothing without a grant.
    #[test]
    fn the_users_own_machines_are_let_in_and_nobody_else_is() {
        assert_eq!(Grant::of(&whois(MINE), Some(2)), Grant::OWNER);
        assert!(
            !Grant::of(&whois(MINE), Some(3)).any(),
            "another user's node"
        );
        assert!(
            !Grant::of(&whois(MINE), None).any(),
            "this node is tagged: no owner to match"
        );
    }

    /// A grant gives the roles it names and no others, whoever owns this node; a value that
    /// is not onicode's shape is ignored rather than refusing the rest.
    #[test]
    fn a_grant_gives_the_roles_it_names() {
        let tagged = Grant::of(&whois(GRANTED), None);
        assert!(tagged.allows(Role::Engine) && !tagged.allows(Role::Viewer) && !tagged.owner);
        let odd = GRANTED.replace(
            r#"[ { "roles": ["engine"] } ]"#,
            r#"[ "junk", { "roles": ["viewer"] } ]"#,
        );
        let odd = Grant::of(&whois(&odd), Some(2));
        assert!(odd.allows(Role::Viewer) && !odd.allows(Role::Engine));
    }
}
