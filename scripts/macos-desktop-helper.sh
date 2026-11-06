# shellcheck shell=bash
# Sourced by scripts/bundle-macos.sh, scripts/package-macos.sh and scripts/run-macos-dev.sh: puts
# onicode-desktopd into its helper bundle inside Onicode.app, with the launchd agent plist
# SMAppService registers (docs/onicode/decisions/onboarding-grants.md; the layout is
# onicode_grants::bundle's).
#
#   Contents/Library/LoginItems/Onicode Desktop.app   desktopd as "Onicode Desktop", LSUIElement
#   Contents/Library/LaunchAgents/<app id>.desktop.plist

# desktop_helper_path <app>: the helper bundle inside <app>.
desktop_helper_path() {
  printf '%s\n' "$1/Contents/Library/LoginItems/Onicode Desktop.app"
}

# desktop_helper_assemble <root> <app> <desktopd> <app id> <version> <name>
# Builds the helper bundle from the desktopd binary at <desktopd>, its bundle id <app id>.desktop
# and its display name <name>, with the app's icon, and writes the agent plist beside it.
desktop_helper_assemble() {
  local root="$1" app="$2" binary="$3" app_id="$4" version="$5" name="$6"
  local helper agents id="$4.desktop"
  helper="$(desktop_helper_path "$app")"
  agents="$app/Contents/Library/LaunchAgents"
  /bin/rm -rf "$helper"
  mkdir -p "$helper/Contents/MacOS" "$helper/Contents/Resources" "$agents"
  install -m 755 "$binary" "$helper/Contents/MacOS/Onicode Desktop"
  sed "s/__VERSION__/$version/g" "$root/packaging/macos/Desktop-Info.plist" >"$helper/Contents/Info.plist"
  plutil -replace CFBundleIdentifier -string "$id" "$helper/Contents/Info.plist"
  plutil -replace CFBundleName -string "$name" "$helper/Contents/Info.plist"
  plutil -replace CFBundleDisplayName -string "$name" "$helper/Contents/Info.plist"
  cp "$app/Contents/Resources/onicode.icns" "$helper/Contents/Resources/onicode.icns"
  /bin/rm -f "$agents"/*.desktop.plist
  sed -e "s/__LABEL__/$id/g" -e "s/__APP_ID__/$app_id/g" \
    "$root/packaging/macos/desktop-agent.plist" >"$agents/$id.plist"
}

# desktop_helper_sign <app> <app id> <identity> [codesign options...]
# Signs the helper bundle under the fixed identifier <app id>.desktop, so TCC's designated
# requirement is that identifier and the certificate, never the binary's hash. Call it first:
# nested code is signed inside out, before the bundle that seals it.
desktop_helper_sign() {
  local helper id="$2.desktop" identity="$3"
  helper="$(desktop_helper_path "$1")"
  shift 3
  codesign --force "$@" --sign "$identity" --identifier "$id" "$helper"
}
