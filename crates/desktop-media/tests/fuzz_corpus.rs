//! Every committed fuzz input replays here on an ordinary build, in the gate:
//! `fuzz/corpus/<target>/` (the inputs that reach what the fuzzers reached) and
//! `fuzz/regressions/<target>/` (a crash found and fixed), each through its target, the same
//! functions `fuzz/fuzz_targets/` hands to libFuzzer.

#[cfg(test)]
#[path = "../fuzz/src/datagram.rs"]
pub mod datagram;
#[cfg(test)]
#[path = "../fuzz/src/feedback.rs"]
pub mod feedback;
#[cfg(test)]
#[path = "../fuzz/src/reassemble.rs"]
pub mod reassemble;

#[cfg(test)]
mod replay {
    use std::fs;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};

    /// A fuzz target: its name and the function libFuzzer hands each input.
    type Target = (&'static str, fn(&[u8]));

    /// As `onicode_desktop_media_fuzz::TARGETS`.
    const TARGETS: &[Target] = &[
        ("media_header", crate::datagram::media_header),
        ("cursor", crate::datagram::cursor),
        ("reassemble", crate::reassemble::run),
        ("feedback", crate::feedback::run),
    ];

    /// The most bytes one target's committed corpus may hold.
    const CORPUS_BYTES: u64 = 2 << 20;

    fn fuzz() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz")
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort();
        names
    }

    /// Every input under `dir/<target>/` through its target; the ones that panic, and how many
    /// ran.
    fn replay(dir: &Path) -> (Vec<PathBuf>, usize) {
        let mut failed = Vec::new();
        let mut ran = 0;
        for target in names_in(dir) {
            let (_, run) = TARGETS
                .iter()
                .find(|(name, _)| *name == target)
                .unwrap_or_else(|| panic!("{} names no fuzz target", dir.join(&target).display()));
            for file in names_in(&dir.join(&target)) {
                let path = dir.join(&target).join(file);
                let bytes = fs::read(&path).unwrap();
                ran += 1;
                if catch_unwind(AssertUnwindSafe(|| run(&bytes))).is_err() {
                    failed.push(path);
                }
            }
        }
        (failed, ran)
    }

    #[test]
    fn every_corpus_and_regression_input_replays_clean() {
        for dir in ["corpus", "regressions"] {
            let (failed, ran) = replay(&fuzz().join(dir));
            assert!(ran > 0, "{dir} holds nothing");
            assert!(
                failed.is_empty(),
                "{} of {ran} {dir} inputs fail: {failed:#?}",
                failed.len()
            );
        }
    }

    /// Every target has an entry point, a committed corpus, and none outgrows [`CORPUS_BYTES`].
    #[test]
    fn every_target_has_an_entry_point_and_a_small_corpus() {
        let manifest = fs::read_to_string(fuzz().join("Cargo.toml")).unwrap();
        for (name, _) in TARGETS {
            assert!(
                fuzz().join(format!("fuzz_targets/{name}.rs")).is_file(),
                "{name}"
            );
            assert!(
                manifest.contains(&format!("path = \"fuzz_targets/{name}.rs\"")),
                "{name}"
            );
            let dir = fuzz().join("corpus").join(name);
            let files = names_in(&dir);
            assert!(!files.is_empty(), "corpus/{name} holds nothing");
            let bytes: u64 = files
                .iter()
                .map(|f| fs::metadata(dir.join(f)).unwrap().len())
                .sum();
            assert!(bytes <= CORPUS_BYTES, "corpus/{name} holds {bytes} bytes");
        }
    }

    /// The shapes a target meets first: empty, one byte, and inputs past every length check.
    #[test]
    fn the_first_shapes_run_through_every_target() {
        let shapes: [&[u8]; 4] = [&[], &[0], &[0xff; 64], &[1; 4096]];
        for (name, run) in TARGETS {
            for shape in shapes {
                let ok = catch_unwind(AssertUnwindSafe(|| run(shape))).is_ok();
                assert!(ok, "{name} on {} bytes", shape.len());
            }
        }
    }
}
