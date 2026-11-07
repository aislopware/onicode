#!/usr/bin/env bash
# Assembles Onicode.app from executables cargo already built; builds and launches nothing.
#
#   scripts/native/bundle-native-macos.sh [profile] [target dir] [version]
#
# Build first: `cargo build --profile <profile> -p onicode -p onicode-engine` (and
# `-p onicode-desktopd` for the desktop helper). The layout (docs/onicode/decisions/engine.md,
# "Step 8 as built"):
#
#   Contents/MacOS/onicode          the app (CFBundleExecutable)
#   Contents/MacOS/Onicode Engine   onicode-engine, which the app launches detached beside it
#   Contents/Library/LoginItems/Onicode Desktop.app   desktopd, when it was built
#
# A release profile takes packaging/macos/Info.plist, any other Info-dev.plist (its data dir
# under the target dir). Signing is ad hoc unless ONICODE_NATIVE_SIGNING_IDENTITY names a
# Developer ID (a name or its SHA-1, found in ONICODE_NATIVE_SIGNING_KEYCHAIN when that is set),
# which adds the hardened runtime, a secure timestamp and packaging/macos/Onicode.entitlements.
# Nested code is signed first, the engine under the fixed identifier "<app id>.engine", so TCC's
# requirement for it never names a binary's hash. The bundle is verified before the script ends.
# scripts/release/bundle-macos.sh runs this for a release; CI's mac job runs it on its dev build.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
os="$(uname -s)"
if [[ "$os" != Darwin ]]; then
  echo 'macOS app bundles are assembled on macOS.' >&2
  exit 1
fi
# shellcheck source=scripts/dev/env.sh
source scripts/dev/env.sh
# shellcheck source=scripts/macos-desktop-helper.sh
source scripts/macos-desktop-helper.sh

profile="${1:-release}"
profile_dir="$profile"
[[ "$profile" = dev ]] && profile_dir=debug
target="${2:-$CARGO_TARGET_DIR}/$profile_dir"
version="${3:-$(sed -n 's/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)}"
app_binary="$target/onicode"
engine_binary="$target/onicode-engine"
desktopd_binary="$target/onicode-desktopd"
for binary in "$app_binary" "$engine_binary"; do
  if [[ ! -x "$binary" ]]; then
    echo "Missing $binary: build it with cargo first." >&2
    exit 1
  fi
done

bundle="$target/Onicode.app"
macos="$bundle/Contents/MacOS"
resources="$bundle/Contents/Resources"
mkdir -p "$macos" "$resources"
# put <source> <name>: replaces the inode, so macOS never reuses an older signature's cache.
put() {
  install -m 755 "$1" "$macos/$2.next"
  mv -f "$macos/$2.next" "$macos/$2"
}
put "$app_binary" onicode
put "$engine_binary" "Onicode Engine"

if [[ "$profile" = release ]]; then
  sed "s/__VERSION__/$version/g" packaging/macos/Info.plist >"$bundle/Contents/Info.plist"
else
  data_dir="$target/onicode-data"
  sed -e "s/__VERSION__/$version/g" -e "s|__DATA_DIR__|$data_dir|g" \
    packaging/macos/Info-dev.plist >"$bundle/Contents/Info.plist"
fi
app_id="$(plutil -extract CFBundleIdentifier raw "$bundle/Contents/Info.plist")"
app_name="$(plutil -extract CFBundleName raw "$bundle/Contents/Info.plist")"

licenses="$resources/licenses"
mkdir -p "$licenses"
cp LICENSE THIRD_PARTY_NOTICES.md "$licenses/"

icns="$resources/onicode.icns"
if [[ ! -f "$icns" || packaging/macos/icon-1024.png -nt "$icns" ]]; then
  iconset="$(mktemp -d)/onicode.iconset"
  mkdir -p "$iconset"
  for size in 16 32 128 256 512; do
    sips -z "$size" "$size" packaging/macos/icon-1024.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" packaging/macos/icon-1024.png \
      --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
  done
  iconutil -c icns "$iconset" -o "$icns"
fi

if [[ -x "$desktopd_binary" ]]; then
  desktop_helper_assemble "$root" "$bundle" "$desktopd_binary" "$app_id" "$version" "$app_name Desktop"
fi

identity="${ONICODE_NATIVE_SIGNING_IDENTITY:--}"
signing=(--force --sign "$identity")
if [[ "$identity" != - ]]; then
  signing+=(--options runtime --timestamp --entitlements packaging/macos/Onicode.entitlements)
  if [[ -n "${ONICODE_NATIVE_SIGNING_KEYCHAIN:-}" ]]; then
    signing+=(--keychain "$ONICODE_NATIVE_SIGNING_KEYCHAIN")
  fi
fi
codesign "${signing[@]}" --identifier "$app_id.engine" "$macos/Onicode Engine"
if [[ -x "$desktopd_binary" ]]; then
  desktop_helper_sign "$bundle" "$app_id" "$identity" "${signing[@]:3}"
fi
codesign "${signing[@]}" "$bundle"

codesign --verify --strict --deep "$bundle"
engine_id="$(codesign --display "$macos/Onicode Engine" 2>&1 | sed -n 's/^Identifier=//p')"
if [[ "$engine_id" != "$app_id.engine" ]]; then
  echo "The engine is signed as ${engine_id:-nothing}, not $app_id.engine." >&2
  exit 1
fi
printf 'App: %s\n' "$bundle"
