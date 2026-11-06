# shellcheck shell=bash
# Sourced by the dev scripts: one warm target dir on the internal SSD (the repo's volume is slower and
# its fseventsd storms stall builds), eight jobs under nice so the desktop stays responsive.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/onicode/target.noindex}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"
export NEXTEST_TEST_THREADS="${NEXTEST_TEST_THREADS:-8}"
if [[ ! -d "$CARGO_TARGET_DIR" ]]; then
  mkdir -p "$CARGO_TARGET_DIR"
  command -v tmutil >/dev/null && tmutil addexclusion "$CARGO_TARGET_DIR" 2>/dev/null || true
fi
