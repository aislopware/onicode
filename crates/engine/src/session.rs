//! Live sessions (`docs/onicode/decisions/engine.md`, migration step 3). The [`SessionHub`] runs
//! a session's turns, splits each `turn::Update` into `SessionEvent`s numbered by a
//! `Cursor {epoch, seq}`, keeps the recent ones in a ring for watchers that come back, and queues
//! them for every watcher. [`SessionService`] serves it as the protocol's Session group over
//! `onicode-rpc`; [`Viewer`] is a watcher's side, folding what arrives back into `turn::Update`s
//! for a view that still merges those.
mod asides;
mod hub;
#[cfg(test)]
mod model;
mod queue;
mod reduce;
mod service;
mod split;
#[cfg(test)]
mod tests;
mod viewer;

pub use hub::{Runner, SessionHub, Spawn, ThreadSpawn, TurnJob};
pub use reduce::apply;
pub use service::SessionService;
pub use viewer::{Expect, Viewer};
