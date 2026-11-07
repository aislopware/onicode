//! Terminals (`docs/onicode/decisions/terminal.md`, engine.md migration step 7): every terminal's
//! PTY and its authoritative libghostty terminal live in the engine ([`TerminalHub`]), a topic per
//! terminal carries its snapshot and output to viewers, and [`TerminalService`] serves the
//! protocol's Terminal group.
mod hub;
mod service;
#[cfg(test)]
mod tests;

pub use hub::TerminalHub;
pub use service::TerminalService;
