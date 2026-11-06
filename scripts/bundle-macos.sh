#!/usr/bin/env bash
# Build the release binaries and assemble target/package/Onicode.app, unsigned.
# better-update (`better-update build --platform macos`, profile in eas.json)
# runs this, then signs the bundle inside out with the Developer ID identity
# from its vault, notarizes, staples, packages and uploads it.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v cargo >/dev/null 2>&1 || PATH="$HOME/.cargo/bin:$PATH"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT_DIR="$ROOT/target/package"
APP="$OUT_DIR/Onicode.app"

cd "$ROOT"
nice -n 10 cargo build --release --locked -p onicode --bins

/bin/rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/licenses/fonts"
install -m 755 "$TARGET_DIR/release/onicode" "$APP/Contents/MacOS/onicode"
# The background engine the app starts; its own process, named for Activity Monitor.
install -m 755 "$TARGET_DIR/release/onicode-engine" "$APP/Contents/MacOS/Onicode Engine"
sed "s/__VERSION__/$VERSION/" "$ROOT/dist/macos/Info.plist" >"$APP/Contents/Info.plist"
cp "$ROOT/crates/ui/assets/fonts/licenses/"* "$APP/Contents/Resources/licenses/fonts/"
cp "$ROOT/THIRD_PARTY_NOTICES.md" "$ROOT/LICENSE" "$APP/Contents/Resources/licenses/"
cp "$ROOT/crates/voice/NOTICE.md" "$APP/Contents/Resources/licenses/parakeet-v3.txt"

ICONSET="$OUT_DIR/onicode.iconset"
/bin/rm -rf "$ICONSET" && mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ROOT/dist/macos/icon-1024.png" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  retina=$((size * 2))
  sips -z "$retina" "$retina" "$ROOT/dist/macos/icon-1024.png" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/onicode.icns"
/bin/rm -rf "$ICONSET"
echo "bundled: $APP ($VERSION)"
