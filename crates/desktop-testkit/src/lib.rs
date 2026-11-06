//! What the remote-desktop crates' tests measure with, never a dependency of a shipped binary.
//!
//! - [`live`]: how a live test that cannot run here says so and returns.
//! - [`stats`]: the percentiles every measurement prints.
//! - [`process`]: a process's retired instructions, footprint, open descriptors and threads, read
//!   from the kernel with no root (`proc_pid_rusage`, `proc_pidinfo`).
//! - [`bench`](mod@bench): a measurement's samples, timed and counted in instructions, printed and
//!   written as one JSON line.
//! - [`soc`]: what the Mac's encode engines and GPU did over a span, from `IOReport`, with no root.
//! - [`alloc`]: a counting global allocator, so a test can hold a hot path to a number of
//!   allocations and bytes.
//!
//! Ported from slopty's `slopty-testkit` (MIT, same authors): the parts the desktop crates' tests
//! use.

#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod alloc;
pub mod bench;
pub mod live;
pub mod process;
pub mod soc;
pub mod stats;
