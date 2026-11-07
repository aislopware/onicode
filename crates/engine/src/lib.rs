//! onicode's engine: everything that does work, one per machine, outliving the window. Viewers
//! reach it over `onicode-protocol` (`docs/onicode/decisions/engine.md`). The session hub runs
//! turns and serves the Session group, the workspace hub the Workspace group and the reminder and
//! automation schedulers, the files and checkout hubs watch the disk and run Git for the Files and
//! Git groups, the accounts hub serves the Agents group, the settings hub the Engine settings
//! group, and the terminal hub hosts every terminal for the Terminal group. [`EngineService`]
//! serves every group over one store only the engine opens ([`Local`]). The `onicode-engine`
//! process serves it on a Unix socket in the data dir ([`host`]); a viewer reaches it through a
//! [`link::Link`], which connects, launches the engine when none runs, and resumes.
pub mod accounts;
pub mod app_access;
mod feed;
pub mod files;
mod fs_watch;
pub mod git;
pub mod host;
mod ledger;
pub mod link;
mod local;
mod probe;
mod searches;
mod service;
pub mod session;
pub mod settings;
pub mod settings_pages;
pub mod terminal;
pub mod testing;
pub mod turn;
pub mod work;
pub mod workspace;

pub use local::{Clock, DATABASE, Local, Options};
pub use service::{EngineGroup, EngineService, Group, VERSION, hello};
