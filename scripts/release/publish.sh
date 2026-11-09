#!/usr/bin/env bash
# Publishes the desktop app at apps/desktop's version to better-update's `latest` feeds
# (docs/operations/release.md):
#
#   1. checks the tree is clean and HEAD is main as pushed;
#   2. builds macOS (signed and notarized by better-update) and Linux;
#   3. checks the notarized zip the way the updater of the native Onicode does before it installs it;
#   4. releases the macOS and Linux builds to `latest` with the notes file.
#
#   scripts/release/publish.sh <notes-file>
#
# Run on the release Mac with better-update logged in and Docker (or OrbStack) running.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
fail() {
  echo "publish.sh: $*" >&2
  exit 1
}
[[ $# -eq 1 && -s "$1" ]] || fail 'usage: publish.sh <notes-file>'
notes="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
version="$(node -p 'require("./apps/desktop/package.json").version')"
requirement='identifier "dev.aislopware.onicode" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] and certificate leaf[field.1.2.840.113635.100.6.1.13] and certificate leaf[subject.OU] = "UK58J62H8L"'

# 1. Release main as pushed.
[[ -z "$(git status --porcelain)" ]] || fail 'the work tree has changes'
[[ "$(git rev-parse --abbrev-ref HEAD)" == main ]] || fail 'not on main'
git fetch --quiet origin main
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || fail 'HEAD is not origin/main'
head="$(git rev-parse --short HEAD)"
work="$(mktemp -d)"
trap '/bin/rm -rf "$work"' EXIT

# 2. Build. Each build of this version made since `started` is ours.
started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
builds_since() {
  better-update builds list --limit 30 --json | jq -r --arg p "$1" --arg v "$version" --arg t "$started" \
    '.data.items[] | select(.Platform == $p and .Version == $v and .Created >= $t) | .ID'
}
better-update build --platform macos --profile production --message "Onicode $version ($head)"
mac_build="$(builds_since macos | head -1)"
[[ -n "$mac_build" ]] || fail "better-update has no macOS build of $version made since $started"
better-update build --platform linux --profile production --message "Onicode $version ($head)"
linux_builds=()
while IFS= read -r id; do linux_builds+=("$id"); done < <(builds_since linux)
[[ ${#linux_builds[@]} -gt 0 ]] || fail "better-update has no Linux build of $version made since $started"

# 3. The zip must hold Onicode.app alone, signed for the requirement, stapled, and answer --version.
better-update builds download "$mac_build" --output "$work/Onicode.zip"
mkdir "$work/unzip"
ditto -x -k "$work/Onicode.zip" "$work/unzip"
items="$(find "$work/unzip" -mindepth 1 -maxdepth 1 ! -name __MACOSX -exec basename {} \;)"
[[ "$items" == Onicode.app ]] || fail "the zip holds ${items//$'\n'/ }, not Onicode.app alone"
app="$work/unzip/Onicode.app"
codesign --verify --deep --strict "-R=$requirement" "$app"
xcrun stapler validate "$app"
spctl --assess --type execute "$app"
reported="$("$app/Contents/MacOS/onicode" --version </dev/null)"
[[ "$reported" == "onicode $version" ]] || fail "the app reports \"$reported\", not \"onicode $version\""

# 4. Release.
better-update macos release create "$mac_build" --channel latest --notes-file "$notes"
better-update linux release create "${linux_builds[@]}" --channel latest --notes-file "$notes"
echo "publish.sh: Onicode $version is on the latest macOS and Linux feeds"
