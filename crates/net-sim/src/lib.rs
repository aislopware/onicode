//! Deterministic networks for onicode's transport tests.
//!
//! * [`shape`]: the link model (delay, jitter, loss, rate, queue), every draw from a seed.
//! * [`sim`]: an in-memory network noq's endpoints sit on directly, on tokio's clock, with
//!   blackouts, NAT rebinding, reordering, per-pair paths and hosts taken off the network.
//! * [`tailnet`]: a simulated tailnet on that network: nodes, users, tags, grants, `Online` flaps,
//!   DERP-latency paths, and a fake `LocalAPI` per node behind [`onicode_tailnet::Tailnet`].
//!
//! Under `#[tokio::test(start_paused = true)]` a whole QUIC connection runs in virtual time: a
//! minute's outage passes in milliseconds, and a seed repeats its run to the packet. Nothing here
//! reads a real clock or does I/O.

pub mod shape;
pub mod sim;
pub mod tailnet;

pub use shape::Link;
pub use sim::{Counts, Faults, Net, Socket};
pub use tailnet::{ApiFault, NodeId, NodeSpec, SimPath, SimTailnet};

/// Prints `seed` if the test holding it panics, so a failing run can be repeated exactly.
#[derive(Debug)]
#[must_use = "the report is printed when it is dropped during a panic"]
pub struct SeedReport {
    seed: u64,
    test: &'static str,
}

impl Drop for SeedReport {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!(
                "{}: failed with seed {} (rerun with this seed to repeat it)",
                self.test, self.seed
            );
        }
    }
}

/// A [`SeedReport`] for `seed`, naming `test` in the line it prints on failure.
pub const fn report_seed(test: &'static str, seed: u64) -> SeedReport {
    SeedReport { seed, test }
}
