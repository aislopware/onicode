//! The Desktop group (`docs/onicode/decisions/remote-desktop.md`): what a viewer asks a Mac's
//! engine before it dials that Mac's `onicode-desktopd` itself. The engine captures nothing and
//! carries no media: it brokers. [`OpenDesktop`] starts desktopd if it is down and says where it
//! listens; [`DesktopGrants`] reads what it may do; [`DesktopOpenGuide`] opens System Settings at
//! a pane for the person at that Mac. All three are forwardable, so the app reaches any Mac's
//! engine through its own.
//!
//! The shapes are the desktop session's own (`onicode-desktop-link`'s broker types), re-exported
//! here so a viewer names them through the protocol.
pub use onicode_desktop_link::broker::{DesktopEndpoint, GrantsReply};
pub use onicode_desktop_link::{DisplayInfo, HostGrants, Pane};
use serde::{Deserialize, Serialize};

use crate::Call;

/// Make sure the Mac's desktopd runs and say where it listens, with its grants and displays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenDesktop {}

impl Call for OpenDesktop {
    type Params = Self;
    type Reply = DesktopEndpoint;

    const NAME: &'static str = "OpenDesktop";
}

/// The Mac's grants, as desktopd reads them (started to ask, if it was down).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopGrants {}

impl Call for DesktopGrants {
    type Params = Self;
    type Reply = GrantsReply;

    const NAME: &'static str = "DesktopGrants";
}

/// Open System Settings at `pane` on the Mac's own screen, for the person sitting at it, and tell
/// desktopd, which restarts once the grant is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopOpenGuide {
    pub pane: Pane,
}

impl Call for DesktopOpenGuide {
    type Params = Self;
    type Reply = ();

    const NAME: &'static str = "DesktopOpenGuide";
}
