#!/usr/bin/env bash
# The lead's check, once per batch. Stages marked * compile.
#   check.sh            all of the below, static first
#   check.sh static     every checker that builds nothing, in parallel (typos, taplo, shellcheck, shfmt, ruff,
#                       actionlint, zizmor, swiftlint, swift format, the coverage baseline)
#   check.sh fmt        nightly rustfmt (RUST_NIGHTLY in scripts/dev/tools.env), check only
#   check.sh deps       cargo-deny and cargo-shear: read Cargo.lock and the sources, build nothing
#   check.sh lint *     fmt and clippy (warnings denied)
#   check.sh doc *      rustdoc over the workspace, warnings denied
#   check.sh build *    the workspace binaries, in `cargo t`'s feature set (the `bins` alias)
#   check.sh test EXPR * tests matching a nextest filter, e.g. 'rdeps(onicode-proto)', under the pull-request
#                       profile `ci` (no soak tests); NEXTEST_PROFILE=default runs the nightly set
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/env.sh
. scripts/dev/tools.env
run() { nice -n 10 "$@"; }

# remote_bootstrap.sh is monocode's POSIX bootstrap for its SSH host, deleted with SSH remote (decisions/monocode-base.md).
sh_files() { git ls-files -co --exclude-standard -- '*.sh' scripts/dev/tools.env | grep -v -e '^vendor/' -e '^crates/core/src/remote/remote_bootstrap.sh$'; }
skip() {
  echo "$1 not installed (scripts/dev/setup.sh): skipped"
}

check_typos() { typos; }
check_taplo() { RUST_LOG=warn taplo fmt --check; }
check_shellcheck() { sh_files | xargs shellcheck; }
check_shfmt() { sh_files | xargs shfmt -d; }
check_ruff() { ruff format --check --quiet && ruff check --quiet; }
check_actionlint() { actionlint; }
# Online audits (impostor commits, known-vulnerable actions) need a GitHub token.
check_zizmor() {
  if [[ -n "${GH_TOKEN:-}" ]]; then
    zizmor --persona auditor --quiet .github
  else
    zizmor --persona auditor --quiet --offline .github
  fi
}
check_swiftlint() {
  local swift
  swift=$(git ls-files '*.swift')
  if [[ -z "$swift" ]]; then
    skip "swiftlint (no Swift files)"
  elif command -v swiftlint >/dev/null; then swiftlint lint --strict --quiet; else skip swiftlint; fi
}
# Xcode's swift format only: the toolchain on Linux runners is another version and formats differently.
check_swift_format() {
  local os
  os=$(uname -s)
  if [[ "$os" == Darwin ]] && swift format --version >/dev/null 2>&1; then
    swift format lint --strict --recursive --parallel scripts
  else
    skip "swift format"
  fi
}
check_coverage_baseline() { python3 scripts/ci/coverage.py rises "${BASE_REF:-origin/main}"; }

static() {
  local out names=() pids=() failed=() i
  out=$(mktemp -d)
  for name in typos taplo shellcheck shfmt ruff actionlint zizmor swiftlint swift_format coverage_baseline; do
    "check_$name" >"$out/$name" 2>&1 &
    names+=("$name")
    pids+=("$!")
  done
  for i in "${!pids[@]}"; do
    if wait "${pids[$i]}"; then
      echo "ok    ${names[$i]}"
    else
      failed+=("${names[$i]}")
      echo "FAIL  ${names[$i]}"
      sed 's/^/      /' "$out/${names[$i]}"
    fi
  done
  rm -rf "$out"
  ((${#failed[@]} == 0))
}

fmt() { cargo "+$RUST_NIGHTLY" fmt --all --check; }
deps() {
  cargo deny --locked check
  cargo shear --locked --deny-warnings
}
lint() {
  fmt
  run cargo lint
}
doc() {
  RUSTDOCFLAGS="-D warnings" run cargo doc --locked --no-deps --document-private-items --workspace
}
# NEXTEST_PROFILE also reaches the tests: the cursor_shim stress test runs 14 rounds under `ci`.
# ONICODE_NO_LOGIN_SHELL: every test process would otherwise run the developer's `$SHELL -lic` once
# (crates/harness/src/shell_env.rs) before its first agent child; the tests of that probe unset it.
tests() {
  NEXTEST_PROFILE="${NEXTEST_PROFILE:-ci}" ONICODE_NO_LOGIN_SHELL="${ONICODE_NO_LOGIN_SHELL-1}" \
    run cargo t --locked --config-file scripts/ci/nextest.toml ${1:+-E "$1"}
}

stage="${1:-all}"
case "$stage" in
  build | lint | doc | test | all) scripts/dev/prune.sh ;;
  *) ;;
esac
case "$stage" in
  static) static ;;
  fmt) fmt ;;
  deps) deps ;;
  build) run cargo bins ;;
  lint) lint ;;
  doc) doc ;;
  test) tests "${2:-}" ;;
  all)
    # The checks that build nothing run beside the ones that compile; their report follows the tests.
    quick=$(mktemp)
    (
      static
      fmt
      deps
    ) >"$quick" 2>&1 &
    quick_pid=$!
    trap 'kill "$quick_pid" 2>/dev/null || true; /bin/rm -f "$quick"' EXIT
    run cargo lint
    doc
    tests
    quick_rc=0
    wait "$quick_pid" || quick_rc=$?
    cat "$quick"
    exit "$quick_rc"
    ;;
  *)
    echo "usage: $0 [static|fmt|deps|build|lint|doc|test EXPR|all]" >&2
    exit 2
    ;;
esac
