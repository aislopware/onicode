//! onicode-sync — replica sync over peer streams and the local `DocsStore`.
//!
//! - [`peer_doc`]: a session doc synced replica to replica by Loro version vectors over one
//!   `doc/{chatId}` stream.
//! - [`registry`]: the workspace registry replicated by per-origin version vectors over one
//!   `registry` stream.
//! - [`DocsStore`]: SQLite snapshots, the processed-command ledger (mark before execute), the
//!   outbox of local sends the host has not acked yet, and durable sync jobs.

pub mod frames;
pub mod peer_doc;
pub mod registry;
mod store;
mod sync_jobs;

pub use store::{DocsStore, OutboxRow, StoreError};
pub use sync_jobs::FORWARD;
