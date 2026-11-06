#!/usr/bin/env bash
# The main test lane, on GitHub's hosted xcode-27 runner (the mac job in .github/workflows/ci.yml, set up by
# .github/actions/mac-ci). macOS and iOS are the platforms tested thoroughly; Linux gets only the static and
# clippy jobs.
#
#   mac.sh pr           clippy for macOS, the app binaries, every test under the ci nextest profile, and
#                       cargo-mutants on the lines the branch changed
#   mac.sh nightly      the same build and tests under the default profile, then coverage (the number that
#                       counts), Miri, a mutants shard, cargo-hack and the fuzz targets (scripts/ci/gates.sh)
#   mac.sh ios-pr       scripts/ci/ios.sh pr: the iOS lane, from the same pipeline
#   mac.sh ios-nightly  scripts/ci/ios.sh nightly
set -euo pipefail
cd "$(dirname "$0")/../.."
suite="${1:-pr}"
case "$suite" in
  pr | nightly) ;;
  ios-pr | ios-nightly) exec scripts/ci/ios.sh "${suite#ios-}" ;;
  *)
    echo "mac.sh: unknown suite $suite" >&2
    exit 2
    ;;
esac

# gpui-fast compiles its Metal shaders at build time, with Xcode's Metal toolchain, a separate
# component that .github/actions/mac-ci installs. Fail here rather than deep in a build script.
if ! xcrun metal --version >/dev/null 2>&1; then
  echo "mac.sh: no Metal toolchain (xcodebuild -downloadComponent MetalToolchain)" >&2
  exit 1
fi

. scripts/dev/tools.env
if ! cargo nextest --version 2>/dev/null | grep -q "$NEXTEST"; then
  scripts/ci/install-nextest.sh
fi

export CARGO_PROFILE_DEV_DEBUG=0
# Every build here has the shape of `cargo t` (.cargo/config.toml): the whole workspace with one
# feature set, so the app build (`cargo bins`), the tests and clippy share one set of artifacts in the
# warm target (a `-p` or another feature set re-resolves features and rebuilds dependencies). Tests are
# picked with filters, never with `-p`.
cargo lint
cargo bins

if [[ "$suite" = pr ]]; then
  NEXTEST_PROFILE=ci cargo t --locked --config-file scripts/ci/nextest.toml
  scripts/ci/gates.sh mutants-diff
  exit 0
fi

# Every gate runs, and the job fails if any did.
failed=()
gate() { "$@" || failed+=("$*"); }
gate env NEXTEST_PROFILE=default cargo t --locked --config-file scripts/ci/nextest.toml
for g in coverage miri mutants-shard hack fuzz; do
  gate scripts/ci/gates.sh "$g"
done
if ((${#failed[@]} > 0)); then
  printf 'mac.sh: failed: %s\n' "${failed[@]}" >&2
  exit 1
fi
