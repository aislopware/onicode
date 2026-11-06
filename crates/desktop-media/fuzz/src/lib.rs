//! Fuzz targets for every decoder of the desktop media wire a peer's bytes reach: the media
//! header and cursor payload, the reassembler and its forward error correction under any loss,
//! order and damage, and the host's side of a client's feedback.
//!
//! What each target does lives here as a plain function. `fuzz_targets/` hands each one
//! libFuzzer's input under the `libfuzzer` feature (`cargo +nightly fuzz run <target>`), or, on a
//! plain build, the files named on its command line. The crate's `tests/fuzz_corpus.rs` replays
//! every committed input under `corpus/<target>/` and `regressions/<target>/` through these same
//! functions on an ordinary build, in the gate.
//!
//! A target fails by panicking. Ported from slopty's `fuzz/`, with its corpora.

pub mod datagram;
pub mod feedback;
pub mod reassemble;

/// One fuzz target: its name (the file under `fuzz_targets/` and its directory under
/// `corpus/`) and what it does with an input.
pub type Target = (&'static str, fn(&[u8]));

/// Every target.
pub const TARGETS: &[Target] = &[
    ("media_header", datagram::media_header),
    ("cursor", datagram::cursor),
    ("reassemble", reassemble::run),
    ("feedback", feedback::run),
];

/// A `fuzz_targets/` entry point: cargo-fuzz's `fuzz_target!` under the `libfuzzer` feature.
#[cfg(feature = "libfuzzer")]
#[macro_export]
macro_rules! target {
    (| $data:ident | $body:expr) => {
        ::libfuzzer_sys::fuzz_target!(|$data: &[u8]| $body);
    };
}

/// A `fuzz_targets/` entry point: without the `libfuzzer` feature, a `main` that runs every file
/// named on its command line.
#[cfg(not(feature = "libfuzzer"))]
#[macro_export]
macro_rules! target {
    (| $data:ident | $body:expr) => {
        fn main() {
            $crate::run_files(|$data: &[u8]| $body);
        }
    };
}

/// Every file named on the command line through `run`, in order.
///
/// # Panics
///
/// When a file cannot be read, or `run` panics on it.
pub fn run_files(run: impl Fn(&[u8])) {
    for name in std::env::args_os().skip(1) {
        let path = std::path::Path::new(&name);
        let input =
            std::fs::read(path).unwrap_or_else(|err| panic!("reading {}: {err}", path.display()));
        run(&input);
    }
}
