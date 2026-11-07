//! OS bridge. Call initialization and Dock updates on the UI thread. All other
//! results are channels so authorization/dispatch never block the UI thread.

use std::sync::mpsc::{self, Receiver};

use super::delivery::{Permission, Target};

/// What the owner did with an agent alert's button (`docs/onicode/decisions/alerts.md`).
pub struct AlertResponse {
    /// The alert's identifier.
    pub id: String,
    pub action: onicode_protocol::alerts::AlertAction,
    /// What was typed into the Reply field.
    pub text: Option<String>,
}

pub struct Notification {
    pub tag: String,
    pub subtitle: String,
    pub body: String,
    pub sound: bool,
}

#[cfg(target_os = "macos")]
#[path = "platform_macos.rs"]
mod implementation;

#[cfg(target_os = "macos")]
pub use implementation::*;

fn resolved<T>(value: T) -> Receiver<T> {
    let (sender, receiver) = mpsc::channel();
    let _ = sender.send(value);
    receiver
}
