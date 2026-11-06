//! "Paste from Other Apps": whether the Onicode app may read the general pasteboard unasked.
//!
//! macOS 15.4 and later ask the person before an app reads the general pasteboard
//! programmatically. Clipboard sync reads it on the Mac the person is not looking at, so the
//! checklist's [`crate::Privilege::Pasteboard`] row walks them through allowing it once. An app is
//! listed in System Settings only after its first prompt, so the row's first click is one read
//! that raises it; then it opens the list (`Privacy_Pasteboard`) for them to set Onicode to
//! Allow.
//!
//! The grant is the app's. The background engine is its child, so the same grant covers it.
//! `behavior` reads `NSPasteboard.accessBehavior`, which never prompts; `ask_once` prompts and
//! is called from a click alone, never from a test.

use serde::{Deserialize, Serialize};

use crate::Status;

/// What a programmatic read of the general pasteboard does (`NSPasteboardAccessBehavior`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum PasteBehavior {
    /// Never asked: a read prompts, and the app is not in the list yet.
    #[default]
    Default,
    /// Listed, set to ask: a read prompts.
    Ask,
    /// Listed, allowed: reads never prompt.
    AlwaysAllow,
    /// Listed, denied: reads fail without a prompt.
    AlwaysDeny,
}

impl PasteBehavior {
    /// Whether reads go through unasked.
    #[must_use]
    pub const fn granted(self) -> bool {
        matches!(self, Self::AlwaysAllow)
    }

    /// Whether the app is in System Settings' list yet.
    #[must_use]
    pub const fn listed(self) -> bool {
        !matches!(self, Self::Default)
    }

    /// The checklist's status for it.
    #[must_use]
    pub const fn status(self) -> Status {
        match self {
            Self::Default => Status::NotAsked,
            Self::Ask | Self::AlwaysDeny => Status::Denied,
            Self::AlwaysAllow => Status::Granted,
        }
    }

    /// What the guide asks of the person next, `None` once granted.
    #[must_use]
    pub const fn next_step(self) -> Option<&'static str> {
        match self {
            Self::Default => Some(
                "Allow the paste Onicode asks for once, then set Onicode to Allow in Paste from \
                 Other Apps.",
            ),
            Self::Ask | Self::AlwaysDeny => {
                Some("Set Onicode to Allow in Privacy & Security, Paste from Other Apps.")
            }
            Self::AlwaysAllow => None,
        }
    }
}

#[cfg(target_os = "macos")]
pub use crate::macos::paste::{ask_once, behavior, open_settings};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Privilege;

    #[test]
    fn the_list_has_its_link() {
        assert_eq!(
            Privilege::Pasteboard.settings_url(),
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Pasteboard"
        );
    }

    #[test]
    fn only_allow_is_granted_and_every_other_state_has_a_step() {
        for behavior in [
            PasteBehavior::Default,
            PasteBehavior::Ask,
            PasteBehavior::AlwaysAllow,
            PasteBehavior::AlwaysDeny,
        ] {
            assert_eq!(behavior.granted(), behavior.next_step().is_none());
            assert_eq!(behavior.granted(), behavior.status() == Status::Granted);
        }
        assert!(!PasteBehavior::Default.listed());
        assert!(PasteBehavior::AlwaysDeny.listed());
    }
}
