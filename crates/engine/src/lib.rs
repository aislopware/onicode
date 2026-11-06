//! onicode's engine: everything that does work, one per machine, outliving the window. Viewers
//! reach it over `onicode-protocol` (`docs/onicode/decisions/engine.md`). For now it is a library
//! the app runs in process; the turn runner is the first part moved out of the app.
pub mod turn;
