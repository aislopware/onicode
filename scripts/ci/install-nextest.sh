#!/usr/bin/env bash
# Install the pinned cargo-nextest release (scripts/dev/tools.env) on macOS after verifying its SHA-256,
# instead of trusting a third-party install action.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/tools.env

os=$(uname -s)
if [[ "$os" != Darwin ]]; then
  echo "install-nextest.sh: macOS only" >&2
  exit 1
fi
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl --proto '=https' --tlsv1.2 -fsSL --retry 5 -o "$tmp/nextest.tar.gz" \
  "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$NEXTEST/cargo-nextest-$NEXTEST-universal-apple-darwin.tar.gz"
echo "$NEXTEST_SHA256_MAC  $tmp/nextest.tar.gz" | shasum -a 256 -c -
bin="${CARGO_HOME:-$HOME/.cargo}/bin"
mkdir -p "$bin"
tar -xzf "$tmp/nextest.tar.gz" -C "$bin" cargo-nextest
cargo nextest --version
