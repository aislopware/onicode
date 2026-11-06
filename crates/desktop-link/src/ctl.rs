//! The engine's line to desktopd on the same Mac: a Unix socket in the data directory, one JSON
//! request and one JSON reply per line.
//!
//! desktopd is a launchd agent of the app's login item (`Onicode Desktop.app`), so it has its
//! own Screen Recording and Accessibility grants and survives the app quitting. The engine
//! starts it with `launchctl kickstart` when the socket does not answer, on the label
//! `onicode_grants::bundle::helper_id` makes of the app's bundle id (a development
//! build's differs).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{DisplayInfo, HostGrants, Pane};

/// The socket's file name in the data directory.
pub const SOCKET: &str = "desktopd.sock";

/// The longest line read.
pub const MAX_LINE: usize = 1 << 20;

/// The socket's path for the engine whose data directory is `data_dir`.
#[must_use]
pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SOCKET)
}

/// Engine → desktopd.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CtlRequest {
    /// What desktopd is and may do now.
    Status,
    /// The guide to `pane` is open on this Mac's screen: desktopd waits for the grant and
    /// restarts once it is on (`onicode_grants::restart_when_granted`), since Screen
    /// Recording reads stale until the process restarts.
    GuideOpen {
        /// The pane.
        pane: Pane,
    },
}

/// desktopd → engine.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum CtlReply {
    /// The answer to [`CtlRequest::Status`].
    Status(DesktopStatus),
    /// Done.
    Ok,
    /// Refused, in desktopd's words.
    Error(String),
}

/// What desktopd is and may do.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct DesktopStatus {
    /// Its process id.
    pub pid: u32,
    /// What the Mac calls itself.
    pub host: String,
    /// Its QUIC endpoint's addresses on the tailnet; empty while the tailnet is down.
    pub addrs: Vec<SocketAddr>,
    /// Its session wire ([`crate::WIRE`]).
    pub wire: u32,
    /// Its grants.
    pub grants: HostGrants,
    /// Its displays; empty while Screen Recording is off.
    pub displays: Vec<DisplayInfo>,
    /// The helper app bundle it runs from, for the guide (`…/LoginItems/Onicode Desktop.app`);
    /// `None` when it runs bare (a development build).
    pub bundle: Option<PathBuf>,
    /// Clients linked now.
    pub clients: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_read_back() {
        let request = CtlRequest::GuideOpen {
            pane: Pane::Accessibility,
        };
        assert_eq!(
            crate::decode::<CtlRequest>(&crate::encode(&request)).unwrap(),
            request
        );
        let reply = CtlReply::Status(DesktopStatus {
            pid: 7,
            host: "mac-studio".into(),
            addrs: vec!["100.64.0.1:7434".parse().unwrap()],
            wire: crate::WIRE,
            grants: HostGrants::default(),
            displays: Vec::new(),
            bundle: None,
            clients: 0,
        });
        assert_eq!(
            crate::decode::<CtlReply>(&crate::encode(&reply)).unwrap(),
            reply
        );
    }
}
