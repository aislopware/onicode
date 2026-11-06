//! onicode's engine: everything that does work, one per machine, outliving the window. Viewers
//! reach it over `onicode-protocol` (`docs/onicode/decisions/engine.md`). For now it is a library
//! the app runs in process: the turn runner moved out of the app, the session hub that runs turns
//! and serves the protocol's Session group, and the workspace hub that serves the Workspace group
//! and runs the reminder and automation schedulers. [`EngineService`] serves both groups.
mod service;
pub mod session;
pub mod turn;
pub mod workspace;

pub use service::EngineService;
