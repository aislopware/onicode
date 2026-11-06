//! Onicode mobile core — the UniFFI surface shared by the iOS and Android apps.
//!
//! - [`client_ffi`]: account, workspace and session state (wraps `onicode-client`).
//! - [`layout`]: analytic transcript layout — markdown → measured display lists
//!   (wraps `onicode-markdown` + `onicode-text`).
//! - [`orb`]: the desktop's voice orb as paintable frames (wraps `onicode-orb`).
//! - [`caption`]: the voice caption's streaming veil (wraps `onicode-veil`).

uniffi::setup_scaffolding!("onicode_core");

pub mod caption;
mod client_ffi;
pub mod layout;
pub mod orb;
pub mod terminal;
pub mod visual;
pub mod wallpaper;

/// Version handshake: the Swift/Kotlin bindings must match the linked library.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
