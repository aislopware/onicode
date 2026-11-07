//! onicode's engine: everything that does work, one per machine, outliving the window. Viewers
//! reach it over `onicode-protocol` (`docs/onicode/decisions/engine.md`). For now it is a library
//! the app runs in process: the turn runner moved out of the app, the session hub that runs turns
//! and serves the protocol's Session group, the workspace hub that serves the Workspace group and
//! runs the reminder and automation schedulers, and the files and checkout hubs that watch the
//! disk and run Git for the Files and Git groups, and the terminal hub that hosts every terminal
//! for the Terminal group. [`EngineService`] serves every group.
mod feed;
pub mod files;
mod fs_watch;
pub mod git;
mod ledger;
mod probe;
mod searches;
mod service;
pub mod session;
pub mod terminal;
pub mod turn;
pub mod workspace;

pub use service::EngineService;
