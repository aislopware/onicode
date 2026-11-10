#!/usr/bin/env bash
# Checks a notarized macOS zip the way the native Onicode's updater does before it installs it:
# Onicode.app alone, signed for the requirement, stapled, and answering --version.
#
#   scripts/release/verify-macos-zip.sh <zip> <version>
set -euo pipefail
fail() {
  echo "verify-macos-zip.sh: $*" >&2
  exit 1
}
[[ $# -eq 2 && -s "$1" ]] || fail 'usage: verify-macos-zip.sh <zip> <version>'
version="$2"
requirement='identifier "dev.aislopware.onicode" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] and certificate leaf[field.1.2.840.113635.100.6.1.13] and certificate leaf[subject.OU] = "UK58J62H8L"'
work="$(mktemp -d)"
trap '/bin/rm -rf "$work"' EXIT

ditto -x -k "$1" "$work"
items="$(find "$work" -mindepth 1 -maxdepth 1 ! -name __MACOSX -exec basename {} \;)"
[[ "$items" == Onicode.app ]] || fail "the zip holds ${items//$'\n'/ }, not Onicode.app alone"
app="$work/Onicode.app"
codesign --verify --deep --strict "-R=$requirement" "$app"
xcrun stapler validate "$app"
spctl --assess --type execute "$app"
reported="$("$app/Contents/MacOS/onicode" --version </dev/null)"
[[ "$reported" == "onicode $version" ]] || fail "the app reports \"$reported\", not \"onicode $version\""
