//! Search lives in `onicode-protocol`; searching a project's files stays here.
pub mod project;

pub use onicode_protocol::search::*;

#[cfg(test)]
mod tests;
