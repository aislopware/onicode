//! onicode's engine: everything that does work, one per machine, outliving the window. Viewers
//! reach it over `onicode-protocol` (`docs/onicode/decisions/engine.md`). For now it is a library
//! the app runs in process: the turn runner moved out of the app, and the session hub that runs
//! turns and serves the protocol's Session group.
pub mod session;
pub mod turn;
