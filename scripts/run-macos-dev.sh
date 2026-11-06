#!/usr/bin/env bash
# Build and run an isolated macOS development bundle. Onicode.app may remain
# open: this bundle has a separate LaunchServices/TCC identity, data directory,
# and engine IPC port.
# Set ONICODE_DEV_BUILD_ONLY=1 to prepare the signed bundle without launching it.
#
# Signing identity (docs/onicode/decisions/onboarding-grants.md): TCC keys a grant
# to the designated requirement, which for a real certificate is the fixed
# identifier plus the certificate, so a grant survives rebuilds only while the
# identity stays the same. ONICODE_DEV_CODESIGN_IDENTITY when set; else the one
# valid "Developer ID Application" identity of team UK58J62H8L; else this
# script stops, unless ONICODE_DEV_ADHOC=1 accepts an ad hoc signature whose
# grants are gone after every build.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
DEV_ROOT="$ROOT/target/macos-dev"
APP="$DEV_ROOT/Onicode Dev.app"
CONTENTS="$APP/Contents"
DATA_DIR="${ONICODE_DEV_DATA_DIR:-$DEV_ROOT/data}"
IPC_PORT="${ONICODE_DEV_IPC_PORT:-49777}"
BUNDLE_ID="${ONICODE_DEV_BUNDLE_ID:-dev.aislopware.onicode.dev}"
DESKTOP_ID="$BUNDLE_ID.desktop"
TEAM_ID="UK58J62H8L"
# shellcheck source=scripts/macos-desktop-helper.sh
source "$ROOT/scripts/macos-desktop-helper.sh"

# dev_identity: the identity to sign with, as codesign's --sign takes it; "-"
# for ad hoc. Exits when the choice would not survive a rebuild.
dev_identity() {
  if [[ -n "${ONICODE_DEV_CODESIGN_IDENTITY:-}" ]]; then
    printf '%s\n' "$ONICODE_DEV_CODESIGN_IDENTITY"
    return
  fi
  local found count
  # By SHA-1, so two certificates with one name (a renewal) never blur.
  found="$(security find-identity -v -p codesigning 2>/dev/null \
    | sed -n "s/^ *[0-9][0-9]*) \([0-9A-F]\{40\}\) \"Developer ID Application: [^\"]*($TEAM_ID)\"\$/\1/p")"
  count="$(printf '%s' "$found" | grep -c . || true)"
  if [[ "$count" == "1" ]]; then
    printf '%s\n' "$found"
    return
  fi
  if [[ "$count" -gt 1 ]]; then
    echo "error: $count valid \"Developer ID Application\" identities of team $TEAM_ID; set ONICODE_DEV_CODESIGN_IDENTITY to one (security find-identity -v -p codesigning)." >&2
    exit 1
  fi
  if [[ "${ONICODE_DEV_ADHOC:-0}" == "1" ]]; then
    echo "warning: signing ad hoc (ONICODE_DEV_ADHOC=1): Screen Recording and Accessibility must be granted again after every build." >&2
    printf '%s\n' "-"
    return
  fi
  echo "error: no \"Developer ID Application\" identity of team $TEAM_ID. Without it Screen Recording and Accessibility grants do not survive a rebuild. Set ONICODE_DEV_CODESIGN_IDENTITY, or ONICODE_DEV_ADHOC=1 to sign ad hoc anyway." >&2
  exit 1
}
# Chosen before the build, so a missing identity costs no compile.
IDENTITY="$(dev_identity)"

if pgrep -f -x "$CONTENTS/MacOS/onicode" >/dev/null 2>&1; then
  echo "Onicode Dev is already running. Quit it before rebuilding the signed bundle." >&2
  exit 1
fi
# The app's background engine keeps running after the app quits. Rebuilding
# replaces its executable, so stop this bundle's engine (only this bundle's:
# matched by its exact path) — its sessions stop with it.
ENGINE="$CONTENTS/MacOS/Onicode Engine"
if pgrep -f -x "$ENGINE" >/dev/null 2>&1; then
  echo "Stopping Onicode Dev's background engine before rebuilding." >&2
  pkill -TERM -f -x "$ENGINE" || true
  for _ in $(seq 1 100); do
    pgrep -f -x "$ENGINE" >/dev/null 2>&1 || break
    sleep 0.1
  done
  if pgrep -f -x "$ENGINE" >/dev/null 2>&1; then
    echo "Onicode Dev's background engine didn't stop; quit it and retry." >&2
    exit 1
  fi
fi

cd "$ROOT"
cargo build -p onicode -p onicode-desktopd

mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources" "$DATA_DIR"
TARGET_DIR="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
install -m 755 "$TARGET_DIR/debug/onicode" "$CONTENTS/MacOS/onicode"
install -m 755 "$TARGET_DIR/debug/onicode-engine" "$ENGINE"
sed "s/__VERSION__/$VERSION/g" "$ROOT/packaging/macos/Info-dev.plist" >"$CONTENTS/Info.plist"
plutil -replace CFBundleIdentifier -string "$BUNDLE_ID" "$CONTENTS/Info.plist"
plutil -replace LSEnvironment.ONICODE_DATA_DIR -string "$DATA_DIR" "$CONTENTS/Info.plist"
plutil -replace LSEnvironment.ONICODE_IPC_PORT -string "$IPC_PORT" "$CONTENTS/Info.plist"
if [[ -n "${CODEX_EXECUTABLE:-}" ]]; then
  plutil -insert LSEnvironment.CODEX_EXECUTABLE -string "$CODEX_EXECUTABLE" "$CONTENTS/Info.plist"
fi
# A release feed for the in-app updater.
if [[ -n "${ONICODE_UPDATE_FEED:-}" ]]; then
  plutil -insert LSEnvironment.ONICODE_UPDATE_FEED -string "$ONICODE_UPDATE_FEED" "$CONTENTS/Info.plist"
fi

mkdir -p "$CONTENTS/Resources/licenses"
cp "$ROOT/crates/voice/NOTICE.md" "$CONTENTS/Resources/licenses"/parakeet-v3.txt

if [[ ! -f "$CONTENTS/Resources/onicode.icns" ]]; then
  ICONSET="$DEV_ROOT/onicode-dev.iconset"
  mkdir -p "$ICONSET"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ROOT/packaging/macos/icon-1024.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    retina=$((size * 2))
    sips -z "$retina" "$retina" "$ROOT/packaging/macos/icon-1024.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$ICONSET" -o "$CONTENTS/Resources/onicode.icns"
fi

# The remote-desktop host in its own helper bundle, named apart from a
# release's in Privacy & Security, with this bundle's data directory and port.
desktop_helper_assemble "$ROOT" "$APP" "$TARGET_DIR/debug/onicode-desktopd" \
  "$BUNDLE_ID" "$VERSION" "Onicode Dev Desktop"
DESKTOP_AGENT="$CONTENTS/Library/LaunchAgents/$DESKTOP_ID.plist"
plutil -insert EnvironmentVariables -dictionary "$DESKTOP_AGENT"
plutil -insert EnvironmentVariables.ONICODE_DATA_DIR -string "$DATA_DIR" "$DESKTOP_AGENT"
plutil -insert EnvironmentVariables.ONICODE_IPC_PORT -string "$IPC_PORT" "$DESKTOP_AGENT"

# Inside out under fixed identifiers: the desktop helper, the engine, then the
# bundle that seals them. No secure timestamp for a development build.
desktop_helper_sign "$APP" "$BUNDLE_ID" "$IDENTITY" --timestamp=none
codesign --entitlements "$ROOT/packaging/macos/Onicode.entitlements" --force --timestamp=none --sign "$IDENTITY" --identifier "$BUNDLE_ID.engine" "$ENGINE"
codesign --entitlements "$ROOT/packaging/macos/Onicode.entitlements" --force --timestamp=none --sign "$IDENTITY" --identifier "$BUNDLE_ID" "$APP"

# A desktopd launchd already runs from this bundle restarts into the new build.
DESKTOP_SERVICE="gui/$UID/$DESKTOP_ID"
if launchctl print "$DESKTOP_SERVICE" >/dev/null 2>&1; then
  launchctl kickstart -k "$DESKTOP_SERVICE" || true
fi

if [[ "${ONICODE_DEV_BUILD_ONLY:-0}" == "1" ]]; then
  echo "built: $APP" >&2
  exit 0
fi

echo "running Onicode Dev (bundle $BUNDLE_ID, data $DATA_DIR, IPC $IPC_PORT)" >&2
# LaunchServices must own the process. Launching Contents/MacOS/onicode directly
# makes TCC attribute Screen Recording to the terminal (Warp, Terminal, etc.).
# -W keeps the script attached until the app exits. Runtime logs remain in the
# isolated data directory (`target/macos-dev/data/logs/onicode-headed.log`).
OPEN_ENV=(
  --env "ONICODE_AUTO_UPDATE=0"
  --env "ONICODE_DATA_DIR=$DATA_DIR"
  --env "ONICODE_IPC_PORT=$IPC_PORT"
)
if [[ -n "${CODEX_EXECUTABLE:-}" ]]; then
  OPEN_ENV+=(--env "CODEX_EXECUTABLE=$CODEX_EXECUTABLE")
fi
if [[ -n "${ONICODE_UPDATE_FEED:-}" ]]; then
  OPEN_ENV+=(--env "ONICODE_UPDATE_FEED=$ONICODE_UPDATE_FEED")
fi
if [[ -n "${ONICODE_OPEN_ROUTE:-}" ]]; then
  OPEN_ENV+=(--env "ONICODE_OPEN_ROUTE=$ONICODE_OPEN_ROUTE")
fi
exec open -W "${OPEN_ENV[@]}" "$APP" --args "$@"
