//! onicode-sync — replica sync over peer streams and the local `DocsStore`.
//!
//! - [`peer_doc`]: a session doc synced replica to replica by Loro version vectors over one
//!   `doc/{chatId}` stream.
//! - [`registry`]: the workspace registry replicated by per-origin version vectors over one
//!   `registry` stream.
//! - [`DocsStore`]: SQLite snapshots, the processed-command ledger (mark before execute), the
//!   outbox of local sends the host has not acked yet, and durable sync jobs.
//! - [`socket`], [`dial`], [`wake`]: the WebSocket helpers the device room still uses.

pub mod budget;
pub mod dial;
pub mod frames;
pub mod net_path;
pub mod peer_doc;
pub mod registry;
pub mod socket;
mod store;
mod sync_jobs;
mod types;
pub mod wake;

pub use store::{DocsStore, OutboxRow, StoreError};
pub use sync_jobs::FORWARD;
pub use types::SyncError;
