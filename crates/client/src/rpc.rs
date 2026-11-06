//! Host RPC vocabulary: what a viewer asks an execution host over its peer
//! link (catalogs, refs, folders, uploads, queue actions), and
//! the capability gates that decide whether to ask at all.

use std::sync::Arc;

pub use onicode_proto::{FolderEntry, FolderListing, RepoRef, WorktreeSpec};

/// Engine capability strings a device row advertises (`Device::capabilities`).
/// Capabilities, not semver: a personal integration build can share an
/// upstream version without the doc/RPC surface.
pub mod capability {
    pub const MESSAGE_QUEUE_V1: &str = "message-queue-v1";
    pub const MESSAGE_QUEUE_ACTIONS_V1: &str = "message-queue-actions-v1";
    pub const MESSAGE_QUEUE_ATTACHMENTS_V1: &str = "message-queue-attachments-v1";
    pub const MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1: &str =
        "message-queue-clean-attachment-text-v1";
    pub const MESSAGE_QUEUE_EDIT_LEASE_V1: &str = "message-queue-edit-lease-v1";
    /// The host creates projectless chats' scratch folders.
    pub const PROJECTLESS_SCRATCH_V1: &str = onicode_proto::capabilities::PROJECTLESS_SCRATCH_V1;
    /// The host can move a chat to another provider between turns.
    pub const PROVIDER_SWITCH_V1: &str = onicode_proto::capabilities::PROVIDER_SWITCH_V1;

    /// Every queue capability (demo hosts advertise the full surface).
    pub const ALL_QUEUE: &[&str] = &[
        MESSAGE_QUEUE_V1,
        MESSAGE_QUEUE_ACTIONS_V1,
        MESSAGE_QUEUE_ATTACHMENTS_V1,
        MESSAGE_QUEUE_CLEAN_ATTACHMENT_TEXT_V1,
        MESSAGE_QUEUE_EDIT_LEASE_V1,
    ];
}

/// Upload progress callback: fraction in `0.0..=1.0` of the file's bytes the
/// host has committed.
pub type ProgressFn = Arc<dyn Fn(f64) + Send + Sync>;

/// Relay method names (single source of truth: `onicode_rpc::methods`).
pub mod methods {
    pub use onicode_rpc::methods::*;
}
