//! Remote desktop's session, around the screen protocol of `onicode_desktop_proto`.
//!
//! A client reaches a Mac's desktop in two steps. It asks the Mac's engine over RPC
//! ([`broker`]): the engine starts or finds `onicode-desktopd` through its local control socket
//! ([`ctl`]) and answers with where desktopd listens and what the Mac may do. The client then
//! dials desktopd's own QUIC endpoint on the tailnet ([`PORT`]), and the link's control stream
//! carries this crate's envelope: [`Up`] from the client, [`Down`] from desktopd, one JSON
//! frame each (`onicode_net::stream::frame`). Media never takes the control stream: it rides
//! `onicode_desktop_media::wire` datagrams on `MEDIA` and `FEEDBACK`.
//!
//! What the envelope adds to slopty's screen protocol:
//!
//! - **A join and a welcome.** The client says who it is and whether it wants the controls;
//!   desktopd answers with its displays, its grants and the seat ([`Down::Welcome`]).
//! - **One controller, any number of viewers** ([`Seat`]). Every client gets its own capture and
//!   encode at its own rate (slopty's multi-client ruling: per-viewer encodes), but only the
//!   controller's input reaches the Mac. Taking the controls is asked for, and the last to ask
//!   holds them: they are the owner's own devices, and the one in front of the person is the one
//!   asking.
//! - **Grants.** A Mac whose Screen Recording or Accessibility grant is off refuses every session
//!   with [`Down::NotPermitted`], naming what is missing, and keeps the link so the client hears
//!   the grants come on ([`Down::Grants`]).
//! - **Numbered input with datagram copies** ([`order`], [`copy`]): each input and quality change
//!   carries a number, goes on the control stream and again as a datagram on [`copy::CHANNEL`];
//!   desktopd applies whichever copy comes first, each once and in order.

#![forbid(unsafe_code)]

use bytes::Bytes;
pub use onicode_desktop_proto::screen::{DisplayInfo, ScreenEvent, ScreenRequest};
use serde::{Deserialize, Serialize};

pub mod broker;
pub mod copy;
pub mod ctl;
pub mod order;

/// The session wire's generation, said in [`Up::Join`] and [`Down::Welcome`]. Bumped with any
/// change a peer of the last generation could misread; peers refuse every other.
pub const WIRE: u32 = 1;

/// desktopd's UDP port on every tailnet address: the engine's port (`onicode_net::transport::
/// PORT`, 7433) plus one.
pub const PORT: u16 = 7434;

/// The capability desktopd's hello carries, so a client knows it dialled a desktop and not an
/// engine.
pub const CAP_DESKTOP: &str = "desktop";

/// The longest control frame either end reads: a cursor picture at 2× is under 20 KB, a
/// listing a few hundred bytes.
pub const MAX_FRAME: usize = 1 << 20;

/// A System Settings privacy pane desktopd needs turned on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Pane {
    /// Screen & System Audio Recording: capture.
    ScreenRecording,
    /// Accessibility: posting input, the cursor's picture, the focused field.
    Accessibility,
}

/// What desktopd may do on its Mac, as `onicode_grants::Grants` read it (preflight only,
/// never a prompt).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct HostGrants {
    /// Screen Recording.
    pub screen_recording: bool,
    /// Accessibility (`AXIsProcessTrusted`).
    pub accessibility: bool,
    /// Posting events (`CGPreflightPostEventAccess`), granted with Accessibility.
    pub post_event: bool,
}

impl HostGrants {
    /// The panes still off, Screen Recording first.
    #[must_use]
    pub fn missing(&self) -> Vec<Pane> {
        let mut missing = Vec::new();
        if !self.screen_recording {
            missing.push(Pane::ScreenRecording);
        }
        if !self.accessibility || !self.post_event {
            missing.push(Pane::Accessibility);
        }
        missing
    }

    /// Whether desktopd may stream and take input.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.missing().is_empty()
    }
}

/// Who holds the controls of a Mac's desktop, as one client sees it.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Seat {
    /// The controller's name (its [`Up::Join`] name), `None` while nobody holds the controls.
    pub controller: Option<String>,
    /// This client is the controller: its input reaches the Mac.
    pub you: bool,
    /// Clients linked, this one included.
    pub clients: u32,
}

/// Client → desktopd, on the control stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Up {
    /// The first frame: who the client is and what it wants. Answered with [`Down::Welcome`],
    /// or [`Down::NotPermitted`] while the Mac's grants are off.
    Join {
        /// [`WIRE`].
        wire: u32,
        /// What the person calls the device (`MacBook Pro`, `iPhone`).
        name: String,
        /// Take the controls on joining.
        control: bool,
        /// Hear the Mac's sound.
        sound: bool,
    },
    /// A screen request that is not numbered ([`ScreenRequest::numbered`] is `None`).
    Screen(ScreenRequest),
    /// A numbered screen request: input and quality changes. Also sent as a datagram copy.
    Numbered(order::Numbered),
    /// Take the controls, or give them up.
    Control {
        /// Take them.
        take: bool,
    },
    /// Hear the Mac's sound, or stop.
    Sound {
        /// Hear it.
        on: bool,
    },
}

/// desktopd → client, on the control stream.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Down {
    /// The answer to [`Up::Join`] when the Mac may stream.
    Welcome {
        /// [`WIRE`].
        wire: u32,
        /// What the Mac calls itself.
        host: String,
        /// Its displays, listed with CoreGraphics (no prompt).
        displays: Vec<DisplayInfo>,
        /// Its grants.
        grants: HostGrants,
        /// The controls.
        seat: Seat,
    },
    /// The Mac may not stream: these panes are off there. The link stays, and
    /// [`Self::Grants`] follows when they change; desktopd then restarts to read them (Screen
    /// Recording reads stale until it does), and the client joins again.
    NotPermitted {
        /// What is off.
        missing: Vec<Pane>,
    },
    /// The Mac's grants changed.
    Grants(HostGrants),
    /// The controls changed hands, or a client came or went.
    Seat(Seat),
    /// A screen event.
    Screen(ScreenEvent),
    /// A numbered request was refused because this client does not hold the controls. Sent
    /// once per run of refused input, not per event.
    NotController,
}

/// Errors reading a frame.
#[derive(Debug)]
pub struct BadFrame(pub serde_json::Error);

impl std::fmt::Display for BadFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad desktop frame: {}", self.0)
    }
}

impl std::error::Error for BadFrame {}

/// One frame's payload: `message` as JSON.
///
/// # Panics
///
/// Never: every type here serializes.
#[must_use]
pub fn encode<T: Serialize>(message: &T) -> Bytes {
    Bytes::from(serde_json::to_vec(message).unwrap_or_default())
}

/// A frame's payload read back.
///
/// # Errors
///
/// [`BadFrame`] when it is not one.
pub fn decode<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, BadFrame> {
    serde_json::from_slice(payload).map_err(BadFrame)
}

#[cfg(test)]
mod tests {
    use onicode_desktop_proto::DisplayId;
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn every_frame_reads_back() {
        let ups = [
            Up::Join {
                wire: WIRE,
                name: "iPhone".into(),
                control: true,
                sound: false,
            },
            Up::Screen(ScreenRequest::List),
            Up::Control { take: false },
            Up::Sound { on: true },
        ];
        for up in ups {
            assert_eq!(decode::<Up>(&encode(&up)).unwrap(), up);
        }
        let downs = [
            Down::Welcome {
                wire: WIRE,
                host: "mac-studio".into(),
                displays: vec![DisplayInfo {
                    id: DisplayId(1),
                    w: 1512.0,
                    h: 982.0,
                    scale: 2.0,
                    hz: 120.0,
                }],
                grants: HostGrants::default(),
                seat: Seat {
                    controller: Some("MacBook Pro".into()),
                    you: false,
                    clients: 2,
                },
            },
            Down::NotPermitted {
                missing: vec![Pane::ScreenRecording],
            },
            Down::NotController,
        ];
        for down in downs {
            assert_eq!(decode::<Down>(&encode(&down)).unwrap(), down);
        }
        assert!(decode::<Up>(b"{").is_err());
    }

    #[test]
    fn grants_name_what_is_off() {
        assert_eq!(
            HostGrants::default().missing(),
            vec![Pane::ScreenRecording, Pane::Accessibility]
        );
        let screen_only = HostGrants {
            screen_recording: true,
            accessibility: true,
            post_event: false,
        };
        assert_eq!(screen_only.missing(), vec![Pane::Accessibility]);
        let all = HostGrants {
            screen_recording: true,
            accessibility: true,
            post_event: true,
        };
        assert!(all.complete());
    }
}
