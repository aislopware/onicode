//! Constants carried over from onicode `packages/session-doc/src/constants.ts`.
//! Per the original design these are starting points — re-measure with real heavy sessions.

/// Max bytes for a single message entry before continuation splitting.
pub const MSG_INLINE_MAX: usize = 256 * 1024;
/// Host commits streamed assistant segments into the doc at this cadence (ms).
pub const STREAM_COMMIT_MS: u64 = 120;
/// Byte budget for the in-memory doc LRU on device backends.
pub const DOC_LRU_BYTE_BUDGET: usize = 80 * 1024 * 1024;
/// Number of trailing messages materialized into the tail sidecar.
pub const TAIL_MESSAGE_COUNT: usize = 64;
/// Minimum spacing between terminal output batches (ms). Output after a quiet
/// spell (an echoed keystroke) is sent at once; only a continuous stream is
/// paced, so a flood can't outrun the viewer's frames.
pub const TERMINAL_OUTPUT_BATCH_MS: u64 = 4;
/// Default TTL for durable commands.
pub const COMMAND_DEFAULT_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// Current session doc schema version (`meta.schemaVersion`).
pub const SESSION_SCHEMA_VERSION: u32 = 1;
