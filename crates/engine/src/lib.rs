//! onicode's engine: everything that does work, one per machine, outliving the window. Viewers
//! reach it over `onicode-protocol` (`docs/onicode/decisions/engine.md`). For now it is a library
//! the app runs in process: the turn runner moved out of the app, the session hub that runs turns
//! and serves the protocol's Session group, the workspace hub that serves the Workspace group and
//! runs the reminder and automation schedulers, and the files and checkout hubs that watch the
//! disk and run Git for the Files and Git groups, the accounts hub behind the Agents group, the
//! settings hub behind the Engine settings group, and the terminal hub that hosts every terminal
//! for the Terminal group. [`EngineService`] serves every group, and [`Local`] is one engine in the
//! viewer's process, over a store only it opens.
pub mod accounts;
pub mod app_access;
mod feed;
pub mod files;
mod fs_watch;
pub mod git;
mod ledger;
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
pub use service::{EngineService, Group};
