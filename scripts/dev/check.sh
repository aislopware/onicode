#!/usr/bin/env bash
# The lead's check, once per batch.
#   check.sh            fmt, clippy (warnings denied), every test
#   check.sh build      the app binaries
#   check.sh lint       fmt and clippy only
#   check.sh test EXPR  tests matching a nextest filter, e.g. 'rdeps(onicode-proto)'
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/env.sh
scripts/dev/prune.sh
run() { nice -n 10 "$@"; }
case "${1:-all}" in
  build) run cargo build --locked -p onicode --bins ;;
  lint) cargo fmt --all --check && run cargo lint --locked ;;
  test) run cargo t --locked --config-file scripts/ci/nextest.toml ${2:+-E "$2"} ;;
  all) cargo fmt --all --check && run cargo lint --locked && run cargo t --locked --config-file scripts/ci/nextest.toml ;;
  *) echo "usage: $0 [build|lint|test EXPR|all]" >&2; exit 2 ;;
esac
