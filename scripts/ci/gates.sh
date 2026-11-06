#!/usr/bin/env bash
# The heavy test gates. scripts/ci/mac.sh runs them on the hosted Mac: mutants-diff on pull requests, the rest
# nightly. Each one compiles; tools are installed at their pinned versions (scripts/dev/tools.env).
#
#   gates.sh coverage        cargo-llvm-cov over `cargo t`'s tests, with the ratchet in scripts/ci/coverage.py
#   gates.sh mutants-diff    cargo-mutants on the lines this branch changed since it left main
#   gates.sh mutants-shard   one shard of every mutant in the workspace, a different shard each night
#   gates.sh miri            Miri on the crates whose unsafe code is plain Rust
#   gates.sh hack            cargo-hack: every crate checks with each of its features alone
#   gates.sh fuzz            each cargo-fuzz target for a minute, from its committed corpus
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/tools.env

# A pinned cargo subcommand, built once into CARGO_HOME (rust-cache keeps ~/.cargo/bin).
tool() { # crate version
  local have
  have=$("$1" --version 2>/dev/null || cargo "${1#cargo-}" --version 2>/dev/null || true)
  if [[ "$have" != *"$2"* ]]; then cargo install --locked --version "$2" "$1"; fi
}

features=(--workspace --features onicode-harness/native-fixture)

case "${1:?usage: gates.sh coverage|mutants-diff|mutants-shard|miri|hack|fuzz}" in
  coverage)
    tool cargo-llvm-cov "$CARGO_LLVM_COV"
    rustup component add llvm-tools-preview
    cargo llvm-cov nextest --no-report --locked "${features[@]}" --lib --bins --tests \
      --config-file scripts/ci/nextest.toml --profile ci --no-tests=fail
    cargo llvm-cov report --summary-only
    cargo llvm-cov report --json --summary-only --output-path target/coverage.json
    cargo llvm-cov report --html --output-dir target/coverage-html
    python3 scripts/ci/coverage.py check target/coverage.json
    ;;
  mutants-diff)
    tool cargo-mutants "$CARGO_MUTANTS"
    git fetch --no-tags origin main
    base=$(git merge-base origin/main HEAD)
    git diff "$base" HEAD -- '*.rs' >target/branch.diff
    if [[ ! -s target/branch.diff ]]; then
      echo "gates.sh: no Rust changes since main; no mutants"
      exit 0
    fi
    # No baseline: mac.sh pr has just passed the same suite on this tree, and cargo-mutants' baseline would
    # test only the mutated packages with `-p`, which fails when one lacks the harness feature. Without a
    # baseline the timeout is not derived from one: a missed mutant runs the whole suite (~5 min).
    NEXTEST_PROFILE=ci cargo mutants --in-place --in-diff target/branch.diff --output target \
      --baseline=skip --timeout 900
    ;;
  mutants-shard)
    tool cargo-mutants "$CARGO_MUTANTS"
    # Thirty shards: the whole workspace every thirty nights.
    day=$(date +%j)
    shard=$((10#$day % 30))
    NEXTEST_PROFILE=ci cargo mutants --in-place --shard "$shard/30" --output target
    ;;
  miri)
    rustup toolchain install "$RUST_NIGHTLY" --profile minimal --component miri,rust-src
    # Every other crate's unsafe is a foreign call (libc, objc2, CoreText, WebKit), which Miri cannot run.
    MIRIFLAGS="-Zmiri-strict-provenance" cargo "+$RUST_NIGHTLY" miri test --locked \
      -p onicode-visual -p onicode-visual-gpu --lib
    ;;
  hack)
    tool cargo-hack "$CARGO_HACK"
    # Its own target: per-feature builds would otherwise churn the warm one.
    CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}/hack" cargo hack check --locked --workspace --each-feature \
      --no-dev-deps
    ;;
  fuzz)
    shopt -s nullglob
    manifests=(crates/*/fuzz/Cargo.toml)
    if ((${#manifests[@]} == 0)); then
      echo "gates.sh: no fuzz targets yet"
      exit 0
    fi
    tool cargo-fuzz "$CARGO_FUZZ"
    rustup toolchain install "$RUST_NIGHTLY" --profile minimal
    for manifest in "${manifests[@]}"; do
      dir=$(dirname "$manifest")
      targets=$(cd "$dir/.." && cargo "+$RUST_NIGHTLY" fuzz list)
      for target in $targets; do
        (cd "$dir/.." && cargo "+$RUST_NIGHTLY" fuzz run "$target" "fuzz/corpus/$target" -- -max_total_time=60)
      done
    done
    ;;
  *)
    echo "gates.sh: unknown gate $1" >&2
    exit 2
    ;;
esac
