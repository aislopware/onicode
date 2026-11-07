#!/usr/bin/env bash
# The macOS release build that better-update runs (eas.json, profile production;
# docs/onicode/decisions/releases.md): the app, the engine and desktopd in the release profile,
# assembled into Onicode.app by scripts/native/bundle-native-macos.sh and signed inside out with
# the Developer ID identity better-update hands its custom build, the engine as
# `Contents/MacOS/Onicode Engine` under `<app id>.engine`. The bundle lands at
# target/package/Onicode.app, where better-update checks every signature (re-signing only what
# fails, identifiers and entitlements kept), notarizes, staples, zips and uploads it.
#
#   better-update build --platform macos --profile production
#
# Run by hand, without better-update's variables, the bundle is signed ad hoc (or as
# ONICODE_NATIVE_SIGNING_IDENTITY says): a check of the release layout, never shipped. Apple
# silicon only: macOS 27.2, the floor, runs on nothing else, so there is nothing to lipo.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
os="$(uname -s)"
arch="$(uname -m)"
if [[ "$os" != Darwin || "$arch" != arm64 ]]; then
  echo 'bundle-macos.sh: a release builds on an Apple silicon Mac.' >&2
  exit 1
fi
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
# shellcheck source=scripts/dev/env.sh
source scripts/dev/env.sh

# better-update's vault identity, by its SHA-1, in the keychain it unlocked for this build.
export ONICODE_NATIVE_SIGNING_IDENTITY="${BETTER_UPDATE_MACOS_SIGNING_IDENTITY:-${ONICODE_NATIVE_SIGNING_IDENTITY:-}}"
export ONICODE_NATIVE_SIGNING_KEYCHAIN="${BETTER_UPDATE_MACOS_KEYCHAIN:-${ONICODE_NATIVE_SIGNING_KEYCHAIN:-}}"

nice -n 10 cargo build --release --locked -p onicode -p onicode-engine -p onicode-desktopd --bins

# A clean bundle: nothing an earlier build left in it is sealed into this one.
/bin/rm -rf "$CARGO_TARGET_DIR/release/Onicode.app"
scripts/native/bundle-native-macos.sh release "$CARGO_TARGET_DIR"

package="$root/target/package"
mkdir -p "$package"
/bin/rm -rf "$package/Onicode.app"
ditto "$CARGO_TARGET_DIR/release/Onicode.app" "$package/Onicode.app"
printf 'Release bundle: %s\n' "$package/Onicode.app"
