//! The local Tailscale daemon, read through its `LocalAPI`: which peers the tailnet has and how
//! each is reached, who is behind an address that calls in, and when any of that changes.
//!
//! Ported from slopty's `slopty-tailnet` (MIT); `watch` and the [`Tailnet`] trait are new.
//!
//! onicode rides the system Tailscale client (any macOS variant, Linux `tailscaled`, the
//! Tailscale app on iOS) against Tailscale's control plane or a compatible one (Headscale,
//! slopscale). Nothing here joins a tailnet or carries a packet: peers speak plaintext QUIC over
//! the OS tunnel. What the daemon adds is what the tunnel cannot say:
//!
//! * [`locate`]: where this machine's `LocalAPI` listens, for each way Tailscale runs.
//! * [`LocalApi`]: `status`, `whois`, `ping` and `watch-ipn-bus` over it.
//! * [`status`]: the tailnet as the daemon sees it, and the path to each peer.
//! * [`whois`]: the node and user behind an address, with the grants it carries.
//! * [`policy`]: which of those onicode lets in, and in what role.
//! * [`watch`]: the daemon's change notifications.
//! * [`Tailnet`]: the seam every consumer reads through, so a simulated tailnet
//!   (`onicode-net-sim`) or a later embedded one stands in for the daemon.
//! * [`Relooking`]: this machine's daemon, found again when it is missing or stops answering.
//!
//! iOS has no `LocalAPI` a third-party app can reach; a phone learns engines from an engine.

#![deny(unsafe_code)]

mod api;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod locate;
pub mod policy;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod proc_files;
mod relook;
pub mod status;
pub mod watch;
pub mod whois;

use std::net::SocketAddr;

pub use api::{LocalApi, LocalApiError, Pong};
pub use locate::Location;
pub use policy::{CAP, Grant, Role};
pub use relook::Relooking;
pub use status::{BackendState, Node, Path, Status};
pub use watch::Notify;
pub use whois::WhoIs;

/// Notifications from [`Tailnet::watch`], in the order the daemon sent them; the channel ends
/// when the daemon closes the watch.
pub type Watch = tokio::sync::mpsc::Receiver<Notify>;

/// The tailnet as this process reads it: the system daemon's `LocalAPI` ([`LocalApi`],
/// [`Relooking`]), or a stand-in.
///
/// Every answer is a fresh read; callers cache what they need.
#[async_trait::async_trait]
pub trait Tailnet: Send + Sync + 'static {
    /// This node and every peer.
    async fn status(&self) -> Result<Status, LocalApiError>;

    /// Who sent from `from`; `None` when no node of the tailnet has that address.
    async fn whois(&self, from: SocketAddr) -> Result<Option<WhoIs>, LocalApiError>;

    /// The daemon's notifications from now on. Each says something changed (peers, state);
    /// a consumer reads [`Self::status`] again rather than trusting a notification's contents.
    async fn watch(&self) -> Result<Watch, LocalApiError>;
}

/// A field Go's JSON writes as `null` for an empty collection reads as the empty value.
fn null_as_default<'de, D, T>(de: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    Ok(<Option<T> as serde::Deserialize>::deserialize(de)?.unwrap_or_default())
}
