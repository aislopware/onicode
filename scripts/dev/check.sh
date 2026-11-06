#!/usr/bin/env bash
# The lead's check, once per batch. Stages marked * compile.
#   check.sh            all of the below, static first
#   check.sh static     every checker that builds nothing, in parallel (typos, taplo, shellcheck, shfmt, ruff,
#                       actionlint, zizmor, swiftlint, swift format, the coverage baseline)
#   check.sh fmt        nightly rustfmt (RUST_NIGHTLY in scripts/dev/tools.env), check only
#   check.sh deps       cargo-deny and cargo-shear: read Cargo.lock and the sources, build nothing
#   check.sh lint *     fmt and clippy (warnings denied)
#   check.sh doc *      rustdoc over the workspace, warnings denied
#   check.sh build *    the workspace binaries
#   check.sh test EXPR * tests matching a nextest filter, e.g. 'rdeps(onicode-proto)'
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/env.sh
. scripts/dev/tools.env
run() { nice -n 10 "$@"; }

sh_files() { git ls-files -co --exclude-standard -- '*.sh' scripts/dev/tools.env | grep -v '^vendor/'; }
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
  if command -v swiftlint >/dev/null; then swiftlint lint --strict --quiet --baseline apps/ios/.swiftlint-baseline.json; else skip swiftlint; fi
}
# Xcode's swift format only: the toolchain on Linux runners is another version and formats differently.
check_swift_format() {
  local os
  os=$(uname -s)
  if [[ "$os" == Darwin ]] && swift format --version >/dev/null 2>&1; then
    swift format lint --strict --recursive --parallel apps/ios scripts
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
  RUSTDOCFLAGS="-D warnings" run cargo doc --locked --no-deps --document-private-items --workspace --features onicode-harness/native-fixture
}
tests() { run cargo t --locked --config-file scripts/ci/nextest.toml ${1:+-E "$1"}; }

stage="${1:-all}"
case "$stage" in
  build | lint | doc | test | all) scripts/dev/prune.sh ;;
  *) ;;
esac
case "$stage" in
  static) static ;;
  fmt) fmt ;;
  deps) deps ;;
  build) run cargo build --locked --workspace --bins --features onicode-harness/native-fixture ;;
  lint) lint ;;
  doc) doc ;;
  test) tests "${2:-}" ;;
  all)
    static
    fmt
    deps
    run cargo lint
    doc
    tests
    ;;
  *)
    echo "usage: $0 [static|fmt|deps|build|lint|doc|test EXPR|all]" >&2
    exit 2
    ;;
esac
