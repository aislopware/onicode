//! The workspace (`docs/onicode/decisions/engine.md`, migration step 4): every session's summary
//! with its marks, the projects' folders, reminders, orchestration summaries, the note count and
//! the automation runs waiting to start, as the protocol's `WatchWorkspace`. The
//! [`WorkspaceHub`] makes every change to them and runs the schedulers behind them;
//! [`WorkspaceService`] serves it as the Workspace group over `onicode-rpc`; [`orchestration`]
//! is the dispatch of a reviewed plan's tasks.
mod chores;
mod hub;
#[cfg(test)]
mod model;
pub mod orchestration;
mod queue;
mod reduce;
mod service;
#[cfg(test)]
mod tests;

pub use chores::{Chores, ProviderChores};
pub use hub::{WorkspaceHub, new_session_settings};
pub use reduce::apply;
pub use service::WorkspaceService;
