//! Who is behind an address (`GET /localapi/v0/whois?addr=ip:port`, `apitype.WhoIsResponse`).
//!
//! `WireGuard` binds a tailnet source address to the node that holds its key, so the daemon's
//! answer names the machine and the user that sent a packet, and the application grants the
//! tailnet's policy gives it (<https://tailscale.com/kb/1537/grants-app-capabilities>).
//!
//! Ported from slopty's `slopty-tailnet` `whois.rs` (MIT).

use std::collections::BTreeMap;

use serde::Deserialize;

/// The node and user behind an address, and the application grants it carries.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct WhoIs {
    /// The node.
    pub node: WhoNode,
    /// Its owner (for a tagged node, the placeholder `tagged-devices` profile).
    pub user_profile: UserProfile,
    /// Application capabilities by name, each with the JSON values the grants gave it.
    #[serde(default, deserialize_with = "crate::null_as_default")]
    pub cap_map: BTreeMap<String, Vec<serde_json::Value>>,
}

/// The node behind an address (`tailcfg.Node`, the fields read here).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct WhoNode {
    /// Its stable id at the control plane, as `status` names it ([`crate::Node::id`]).
    #[serde(rename = "StableID", default)]
    pub stable_id: String,
    /// Its `MagicDNS` name, with a trailing dot.
    pub name: String,
    /// The user it belongs to.
    pub user: i64,
    /// Its tags; a tagged node belongs to no user.
    #[serde(default, deserialize_with = "crate::null_as_default")]
    pub tags: Vec<String>,
    /// Its tailnet addresses as prefixes (`100.64.0.3/32`).
    #[serde(default, deserialize_with = "crate::null_as_default")]
    pub addresses: Vec<String>,
}

/// A user of the tailnet.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UserProfile {
    /// The user's id at the control plane.
    #[serde(rename = "ID")]
    pub id: i64,
    /// The name they log in with.
    pub login_name: String,
    /// The name they show.
    #[serde(default)]
    pub display_name: String,
}

impl WhoIs {
    /// Whether the node carries a tag, and so belongs to no user.
    #[must_use]
    pub const fn tagged(&self) -> bool {
        !self.node.tags.is_empty()
    }
}

/// Whois fixtures, shared with the other modules' tests.
#[cfg(test)]
pub mod tests {
    use super::*;

    /// A whois as tailscale 1.102 writes it for an untagged node of user 2, with no grants.
    pub const MINE: &str = r#"{
      "Node": { "ID": 1, "StableID": "n1", "Name": "laptop.tail1234.ts.net.", "User": 2,
        "Tags": null, "Addresses": ["100.64.0.4/32", "fd7a:115c:a1e0::4/128"] },
      "UserProfile": { "ID": 2, "LoginName": "me@example.com", "DisplayName": "Me" },
      "CapMap": null
    }"#;

    /// A tagged node with an onicode grant.
    pub const GRANTED: &str = r#"{
      "Node": { "ID": 7, "StableID": "n9", "Name": "ci.tail1234.ts.net.", "User": 9,
        "Tags": ["tag:ci"], "Addresses": ["100.64.0.9/32"] },
      "UserProfile": { "ID": 9, "LoginName": "tagged-devices", "DisplayName": "Tagged Devices" },
      "CapMap": { "github.com/aislopware/onicode": [ { "roles": ["engine"] } ],
                  "example.com/cap/other": [ {} ] }
    }"#;

    #[test]
    fn a_whois_names_the_node_its_user_and_its_grants() {
        let mine: WhoIs = serde_json::from_str(MINE).unwrap();
        assert_eq!(mine.user_profile.id, 2);
        assert_eq!(mine.node.stable_id, "n1");
        assert!(!mine.tagged() && mine.cap_map.is_empty());
        let ci: WhoIs = serde_json::from_str(GRANTED).unwrap();
        assert!(ci.tagged());
        assert_eq!(ci.cap_map.len(), 2);
    }
}
