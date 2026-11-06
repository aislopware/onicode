# Static checks and test gates

Every language in the repository has its strictest formatter and linter, configured at the root and run by one entry
point, `scripts/dev/check.sh static`, in parallel and without compiling: typos, taplo, shellcheck with every optional
check, shfmt, ruff with every rule, actionlint, zizmor as auditor, swiftlint and swift format. Rust is formatted by a
pinned nightly rustfmt, because import grouping, comment wrapping and doc-code formatting exist only there, and
cargo-deny holds licenses, advisories, duplicate versions and git sources. A rule is turned off only where it fights
the formatter or has no bug class behind it (shellcheck's SC2250, variable braces), and an exception is one line with
its reason. Tool versions are pinned in one file that the hooks, the dev setup and CI read.

Tests are held to what they prove: no retries anywhere, leaked processes fail, a run that selects nothing fails,
coverage only rises, and mutants test the tests. Pull requests stay fast by running mutants on the changed lines
only; the whole-workspace gates (coverage, Miri, a rotating mutants shard, per-feature checks, fuzzing) run nightly.

macOS and iOS are the platforms tested thoroughly, on the office Macs, and the coverage measured there is the one that
counts. Linux keeps only the cheap gates, static checks and clippy, so cfg(linux) code still compiles; there are no
other targets.
