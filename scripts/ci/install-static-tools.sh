#!/usr/bin/env bash
# The static job's checkers that taiki-e/install-action does not carry, from their release assets on x86_64
# Linux, each verified against its SHA-256 in scripts/dev/tools.env.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/tools.env

bin="$HOME/.local/bin"
mkdir -p "$bin"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

fetch() { # name url sha256
  curl --proto '=https' --tlsv1.2 -fsSL --retry 5 -o "$tmp/$1" "$2"
  echo "$3  $tmp/$1" | sha256sum -c -
}

fetch ruff.tar.gz "https://github.com/astral-sh/ruff/releases/download/$RUFF/ruff-x86_64-unknown-linux-gnu.tar.gz" \
  "$RUFF_SHA256_LINUX"
tar -xzf "$tmp/ruff.tar.gz" -C "$tmp"
install -m 0755 "$tmp/ruff-x86_64-unknown-linux-gnu/ruff" "$bin/ruff"

fetch actionlint.tar.gz \
  "https://github.com/rhysd/actionlint/releases/download/v$ACTIONLINT/actionlint_${ACTIONLINT}_linux_amd64.tar.gz" \
  "$ACTIONLINT_SHA256_LINUX"
tar -xzf "$tmp/actionlint.tar.gz" -C "$bin" actionlint

fetch swiftlint.zip "https://github.com/realm/SwiftLint/releases/download/$SWIFTLINT/swiftlint_linux_amd64.zip" \
  "$SWIFTLINT_SHA256_LINUX"
unzip -q -o "$tmp/swiftlint.zip" -d "$tmp/swiftlint"
install -m 0755 "$tmp/swiftlint/swiftlint-static" "$bin/swiftlint"

echo "$bin" >>"${GITHUB_PATH:-/dev/null}"
"$bin/ruff" --version
"$bin/actionlint" --version
"$bin/swiftlint" version
