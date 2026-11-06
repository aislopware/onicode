#!/usr/bin/env bash
# One-time setup of a Mac for onicode work: the pinned nightly, every checker and test tool at its pinned
# version (scripts/dev/tools.env), and the git hooks (lefthook.yml). Safe to rerun; it only installs what is
# missing or at another version.
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/tools.env

rustup toolchain install "$RUST_NIGHTLY" --profile minimal --component rustfmt,miri,rust-src
rustup component add llvm-tools-preview

# What CMD reports as its version, empty when it is missing.
version_of() { # command [version-flag]
  case "$1" in
    cargo-*) cargo "${1#cargo-}" "${2:---version}" 2>/dev/null || true ;;
    *) "$1" "${2:---version}" 2>/dev/null || true ;;
  esac
}

# Rust-built tools: a prebuilt binary through cargo-binstall when it is there, else a locked source build.
crate() { # crate version [command]
  local have
  have=$(version_of "${3:-$1}")
  if [[ "$have" == *"$2"* ]]; then return; fi
  if command -v cargo-binstall >/dev/null; then
    cargo binstall --no-confirm --locked "$1@$2"
  else
    cargo install --locked --version "$2" "$1"
  fi
}
crate typos-cli "$TYPOS" typos
crate taplo-cli "$TAPLO" taplo
crate zizmor "$ZIZMOR"
crate cargo-deny "$CARGO_DENY"
crate cargo-shear "$CARGO_SHEAR"
crate cargo-llvm-cov "$CARGO_LLVM_COV"
crate cargo-mutants "$CARGO_MUTANTS"
crate cargo-hack "$CARGO_HACK"
crate cargo-fuzz "$CARGO_FUZZ"
have=$(version_of cargo-nextest)
if [[ "$have" != *"$NEXTEST"* ]]; then scripts/ci/install-nextest.sh; fi

have=$(version_of ruff)
if [[ "$have" != *"$RUFF"* ]]; then
  if command -v uv >/dev/null; then uv tool install --force "ruff==$RUFF"; else pip3 install --user "ruff==$RUFF"; fi
fi

# Homebrew carries one version of each; say so when it is not the pinned one.
brewed() { # formula version [command] [version-flag]
  local cmd="${3:-$1}" have
  if ! command -v "$cmd" >/dev/null; then brew install "$1"; fi
  have=$(version_of "$cmd" "${4:---version}")
  if [[ "$have" != *"$2"* ]]; then
    echo "setup.sh: $cmd is not $2 (scripts/dev/tools.env); CI uses $2" >&2
  fi
}
brewed shellcheck "$SHELLCHECK"
brewed shfmt "$SHFMT"
brewed actionlint "$ACTIONLINT"
brewed swiftlint "$SWIFTLINT" swiftlint version
brewed lefthook "$LEFTHOOK" lefthook version

lefthook install
