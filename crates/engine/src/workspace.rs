//! The workspace (`docs/onicode/decisions/engine.md`, migration step 4): every session's summary
//! with its marks, the projects' folders, reminders and the note count, as the protocol's
//! `WatchWorkspace`. The [`WorkspaceHub`] makes every change to them and runs the schedulers
//! behind them, the automations of this engine's projects among them; [`WorkspaceService`]
//! serves it as the Workspace group over `onicode-rpc`.
#[cfg(test)]
mod automation_tests;
mod chores;
mod hub;
#[cfg(test)]
mod model;
mod queue;
mod reduce;
mod service;
#[cfg(test)]
mod tests;

pub use chores::{Chores, ProviderChores};
pub use hub::WorkspaceHub;
pub use reduce::apply;
pub use service::WorkspaceService;
