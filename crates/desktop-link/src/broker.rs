//! What a client asks a Mac's engine for, and what it hears back, before it dials desktopd.
//!
//! The engine carries no media and captures nothing (its Screen Recording grant would be the
//! app's): it brokers. `OpenDesktop` makes sure desktopd runs (launchd starts it as the app's
//! login-item agent) and answers with a [`DesktopEndpoint`]; the client dials that directly.
//! The engine's calls are `OpenDesktop`, `DesktopGrants` and `DesktopOpenGuide`
//! (`docs/onicode/decisions/remote-desktop.md`), forwardable, so a client reaches any Mac's engine
//! through its own.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::{DisplayInfo, HostGrants, Pane};

/// Where a Mac's desktopd listens and what it may do: the answer to `OpenDesktop`.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopEndpoint {
    /// The Mac's device-id, as its engine names it.
    pub device_id: String,
    /// What the Mac calls itself.
    pub host: String,
    /// desktopd's addresses on the tailnet, IPv4 first.
    pub addrs: Vec<SocketAddr>,
    /// desktopd's session wire ([`crate::WIRE`]).
    pub wire: u32,
    /// Its grants as of the answer.
    pub grants: HostGrants,
    /// Its displays as of the answer; empty while Screen Recording is off.
    pub displays: Vec<DisplayInfo>,
}

/// `DesktopOpenGuide`'s parameters: open the guide to `pane` on the Mac's own screen, for the
/// person sitting at it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OpenGuideParams {
    /// The pane.
    pub pane: Pane,
}

/// `DesktopGrants`' answer: the Mac's grants, read fresh when desktopd is not running.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GrantsReply {
    /// The grants.
    pub grants: HostGrants,
    /// desktopd is running.
    pub running: bool,
}
