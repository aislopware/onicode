#!/usr/bin/env bash
# Golden images of the UI on the current GPUI stack (docs/onicode/golden.md). Headless: no window opens.
#   golden.sh record [TEST]   draw the scenes into crates/app/tests/golden/<scene>-<theme>.png
#   golden.sh check [TEST]    draw and compare; diffs, drawn images and stats go to $CARGO_TARGET_DIR/golden
# TEST is one test of crates/app/src/shell/golden_tests.rs (menu, peek_mid_slide, ...); all by default.
# ONICODE_GOLDEN_TOLERANCE=N lets every pixel differ by N levels per channel (default 0; antialiased edges 1).
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/env.sh

mode="${1:-}"
case "$mode" in
  record | check) ;;
  *)
    echo "usage: $0 record|check [TEST]" >&2
    exit 2
    ;;
esac
filter='test(/::golden_tests::/)'
if [[ -n "${2:-}" ]]; then
  filter="test(/::golden_tests::$2\$/)"
fi
out="$CARGO_TARGET_DIR/golden"
/bin/rm -rf "$out"

status=0
ONICODE_GOLDEN="$mode" ONICODE_NO_LOGIN_SHELL=1 \
  cargo t --locked --config-file scripts/ci/nextest.toml --run-ignored only -E "$filter" || status=$?
if [[ "$mode" == check ]] && compgen -G "$out/*.stats.txt" >/dev/null; then
  cat "$out"/*.stats.txt
fi
exit "$status"
