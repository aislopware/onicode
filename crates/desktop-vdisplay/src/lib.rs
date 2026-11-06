//! A virtual display sized to one client, so a worker can stream a desktop at the client's own
//! size and scale (a headless Mac mini, an iPad, a 5K client).
//!
//! [`plan()`] turns a client's [`Request`] into the display's fixed [`Descriptor`] and its
//! [`Mode`]; it is pure and compiles everywhere. On macOS, [`VirtualDisplay`] creates the
//! display through CoreGraphics' private `CGVirtualDisplay` classes, found at runtime. Elsewhere,
//! and on a macOS without them, it is [`DisplayError::Unavailable`] and the caller streams a
//! physical display instead. [`available()`] says which, creating nothing.
//!
//! On macOS the owner also serves the main thread ([`park_main`]) and enforces again on every
//! display reconfiguration ([`on_reconfiguration`]).

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

#[cfg(target_os = "macos")]
mod display;
mod plan;
#[cfg(target_os = "macos")]
mod runloop;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub use display::{VirtualDisplay, available, made_for_a_client};
pub use plan::{
    ClientKey, DEFAULT_REFRESH_HZ, Descriptor, MAX_SIDE_PIXELS, MIN_SIDE_POINTS, Mode, NAME, Plan,
    REFRESH_HZ, Request, VENDOR_ID, plan,
};
#[cfg(target_os = "macos")]
pub use runloop::{on_reconfiguration, park_main};
#[cfg(not(target_os = "macos"))]
pub use unsupported::{VirtualDisplay, available};

/// Why a virtual display could not be made or changed.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum DisplayError {
    /// The private classes or a selector are missing (or this is not macOS): stream a physical
    /// display instead.
    #[error("virtual displays are unavailable: {0}")]
    Unavailable(String),
    /// `CGVirtualDisplay` only initialises on the main thread.
    #[error("a virtual display is created and changed on the main thread")]
    NotMainThread,
    /// CoreGraphics returned nil or display 0.
    #[error("CoreGraphics refused to create the virtual display")]
    Refused,
    /// `applySettings:` returned false.
    #[error("CoreGraphics rejected the display settings")]
    Rejected,
    /// The mode is larger than the display was created for; create a new display.
    #[error("{wanted:?} pixels do not fit a display created for {max:?}")]
    Outgrown {
        /// The mode's pixels.
        wanted: (u32, u32),
        /// The descriptor's maximum.
        max: (u32, u32),
    },
    /// A display configuration call failed with this `CGError`.
    #[error("display configuration failed with CGError {0}")]
    Configure(i32),
}

/// What [`VirtualDisplay::enforce`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enforced {
    /// Not online yet, or macOS does not list the mode yet: call again.
    Pending,
    /// Already in its mode and mirroring nothing.
    Settled,
    /// The mode was set or mirroring broken; call again on the next reconfiguration.
    Applied,
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests;
