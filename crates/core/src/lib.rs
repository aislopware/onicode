//! The engine's IO: the store, the provider runs, the disk and the network. The data it reads and
//! writes, and the pure helpers over it, live in `onicode-protocol`; each module here re-exports
//! its protocol twin at the same path.
pub mod accounts;
pub mod app_access;
pub mod automations;
pub mod catalog;
pub mod checkpoint;
pub mod cli;
pub mod composer;
pub mod desktop_preferences;
pub mod external_editor;
pub mod files;
pub mod follow_up;
pub mod git;
pub mod harness_updates;
pub mod inbox;
pub mod mcp;
pub mod notes;
pub mod process;
pub mod project_appearance;
pub mod provider;
pub mod resources;
pub mod scm;
pub mod search;
pub mod session_groups;
pub mod session_title;
pub mod skills;
pub mod store;
pub mod transcript;
pub mod usage;
pub mod visual;
pub mod visual_bridge;
pub mod worktree_management;

pub use onicode_protocol::{
    agent, b64, context, diff, new_id, orchestration, response, session_filters, time, tool,
    user_links,
};
