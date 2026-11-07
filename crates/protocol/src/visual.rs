//! A session visual's document and the bounded native tree its program renders: data, never HTML
//! or a host API. The program itself runs in the viewer's sandboxed interpreter.
mod model;

pub use model::*;

/// Conservative per-interpreter bound for transcript cache accounting.
pub const MEMORY_LIMIT_BYTES: usize = 16 * 1024 * 1024;
