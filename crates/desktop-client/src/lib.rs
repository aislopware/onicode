//! A Mac's desktop on this device: the client half of onicode's remote desktop, shared by the
//! macOS app (`onicode-ui`) and iOS (`onicode-mobile`).
//!
//! The way in is two steps. The app asks the Mac's engine for the desktop (the
//! engine's `OpenDesktop` call), which answers with a
//! [`onicode_desktop_link::broker::DesktopEndpoint`]: where the Mac's
//! `onicode-desktopd` listens on the tailnet, and what it may do. [`DesktopClient::connect`] then
//! dials desktopd directly, plaintext QUIC over the tailnet like every onicode link, and keeps
//! the link up, redialling with backoff when it drops ([`link`]).
//!
//! ```text
//! datagrams ─▶ ScreenRouter ─┬─ stream task: reassemble, NACK, refresh, report, clock probes
//!   (MEDIA)                  │      └─▶ decode thread per lane ─▶ Stitch ─▶ Present (the layer)
//!                            └─ SOUND: Opus ─▶ player
//! ```
//!
//! * [`ScreenRouter`] fans datagrams out by stream, keeping a short backlog for a stream whose
//!   first datagrams beat its `Opened` (slopty's client `screen.rs`).
//! * [`screen`] runs one task per stream: a reassembler per lane (the whole picture, or each of two
//!   stripes), a decoder on a thread of its own per lane, decode failures turned into refreshes, a
//!   decoder that stops given up, receiver reports every 50 ms and clock probes four times a
//!   second, all as `FEEDBACK` datagrams; pictures go to a [`Present`] hook on the decoder's thread
//!   the moment they come out.
//! * [`session`] is the control stream: joining, the controls ([`onicode_desktop_link::Seat`]), the
//!   grants, opening displays, and input numbered and copied in datagrams.
//! * [`fit`] places the picture in a view, letterboxed, and maps a point back to stream pixels.
//! * [`display`] is this device's key for a display a Mac makes at its size.
//!
//! Only the picture's decode is Apple's ([`decode`]); everything else is plain Rust, so the
//! tests run it with a fake decoder. Every stamp the receive path takes reads tokio's clock
//! ([`instant`]), and a fake decoder decodes in place ([`decode::VideoDecode::INLINE`]), so on a
//! paused clock a whole session, from desktopd's capture to the glass, runs in virtual time.

#![deny(unsafe_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use onicode_peer::{PeerConfig, PeerEndpoint, PeerError, Sockets, SystemSockets};
use onicode_tailnet::{Relooking, Role, Tailnet};

pub mod decode;
pub mod display;
pub mod fit;
pub mod link;
pub mod router;
pub mod screen;
pub mod session;
pub mod sound;

pub use decode::{DecodedFrame, Image, Picture, PlatformDecoder, VideoDecode};
pub use onicode_desktop_link::broker::DesktopEndpoint;
pub use onicode_desktop_link::{HostGrants, Pane, Seat};
pub use router::ScreenRouter;
pub use screen::{CursorState, Present, Presentable, ScreenHandle, ScreenStats, Stitched};
pub use session::{DesktopSession, Join, KeptCopy, Open, Phase, SessionState, StreamView};

/// Now, on tokio's clock: the system's, or a test's paused one, so a stream's timers and its
/// stamps agree in virtual time. Off a runtime (a platform decoder's own thread), the system's.
#[must_use]
pub fn instant() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// The glass's clock: [`instant`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RuntimeClock;

impl onicode_desktop_media::Clock for RuntimeClock {
    fn now(&self) -> Instant {
        instant()
    }
}

/// What reaching a desktop can fail with.
#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    /// The tailnet endpoint could not be bound, or a dial failed.
    #[error(transparent)]
    Peer(#[from] PeerError),
    /// The endpoint names no address this device can reach.
    #[error("the desktop has no address on the tailnet")]
    NoAddress,
    /// What answered is not a desktop (its hello lacks the desktop capability).
    #[error("{0} is not a desktop")]
    NotADesktop(SocketAddr),
    /// The desktop speaks another session wire.
    #[error("the desktop speaks wire {theirs}, this app {ours}")]
    WrongWire {
        /// Ours.
        ours: u32,
        /// Theirs.
        theirs: u32,
    },
}

/// This device on the tailnet, as a desktop viewer: one QUIC endpoint every desktop link of the
/// app dials from. Separate from the engine's links (`onicode_peer::PeerLinks`), which carry no
/// media: a desktop's datagrams go straight between this endpoint and desktopd's.
pub struct DesktopClient {
    endpoint: Arc<PeerEndpoint>,
}

impl std::fmt::Debug for DesktopClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopClient")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl DesktopClient {
    /// Binds on this machine's tailnet addresses, any port, as `device_id`.
    ///
    /// # Errors
    ///
    /// [`DesktopError::Peer`] when Tailscale is not up or the bind fails.
    pub async fn bind(device_id: &str) -> Result<Arc<Self>, DesktopError> {
        let mut config = PeerConfig::new(device_id, Role::Viewer);
        config.port = 0;
        Self::bind_on(Arc::new(Relooking::system()), &SystemSockets, config).await
    }

    /// [`Self::bind`] on `tailnet` with `sockets` and `config`: a simulated tailnet in tests.
    ///
    /// # Errors
    ///
    /// As [`Self::bind`].
    pub async fn bind_on(
        tailnet: Arc<dyn Tailnet>,
        sockets: &dyn Sockets,
        config: PeerConfig,
    ) -> Result<Arc<Self>, DesktopError> {
        let endpoint = PeerEndpoint::bind(tailnet, sockets, config).await?;
        Ok(Arc::new(Self { endpoint }))
    }

    /// The endpoint desktop links dial from.
    #[must_use]
    pub fn endpoint(&self) -> &Arc<PeerEndpoint> {
        &self.endpoint
    }

    /// Links to the desktop `endpoint` names as `join` says, in the background: the session
    /// dials at once, and again with backoff whenever the link drops, until it is shut down
    /// ([`DesktopSession::shutdown`]) or every handle to it is dropped. Must be called inside a
    /// tokio runtime.
    #[must_use]
    pub fn connect(self: &Arc<Self>, endpoint: DesktopEndpoint, join: Join) -> DesktopSession {
        self.connect_with::<PlatformDecoder>(endpoint, join)
    }

    /// [`Self::connect`], decoding with `D`: a test's decoder.
    #[must_use]
    pub fn connect_with<D: VideoDecode>(
        self: &Arc<Self>,
        endpoint: DesktopEndpoint,
        join: Join,
    ) -> DesktopSession<D> {
        session::DesktopSession::start(Arc::clone(self), endpoint, join)
    }
}
