//! Mobile access: a paired phone browses and drives the desktop workspace over
//! the QUIC link in `onicode-mobile-link`. The host here is app-agnostic; the
//! app answers [`host::Event`]s from its own state.
pub mod catalog;
pub mod host;
pub mod notify;
mod router;
pub mod watch;

pub use host::{
    Device, Event, Invite, MobileHost, Push, PushRegistration, Request, Status, WatchStart,
};
pub use onicode_mobile_link::Address;
