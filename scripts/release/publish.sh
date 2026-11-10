#!/usr/bin/env bash
# Publishes the desktop app at apps/desktop's version to better-update's `latest` feeds from this
# machine. The Release workflow does the same on GitHub Actions (docs/operations/release.md):
#
#   1. checks the tree is clean and HEAD is main as pushed;
#   2. builds macOS (signed and notarized by better-update) and Linux;
#   3. checks the notarized zip the way the updater of the native Onicode does before it installs it;
#   4. releases the macOS and Linux builds to `latest` with the notes file;
#   5. points the Homebrew cask in aislopware/homebrew-tap at the new macOS release.
#
#   scripts/release/publish.sh <notes-file>
#
# Run on the release Mac with better-update and gh logged in and Docker (or OrbStack) running.
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

# 1. Release main as pushed.
[[ -z "$(git status --porcelain)" ]] || fail 'the work tree has changes'
[[ "$(git rev-parse --abbrev-ref HEAD)" == main ]] || fail 'not on main'
git fetch --quiet origin main
[[ "$(git rev-parse HEAD)" == "$(git rev-parse origin/main)" ]] || fail 'HEAD is not origin/main'
message="Onicode $version ($(git rev-parse --short HEAD))"
work="$(mktemp -d)"
trap '/bin/rm -rf "$work"' EXIT

# 2. Build.
mac_build="$(scripts/release/build.sh macos "$message" | head -1)"
linux_builds=()
while IFS= read -r id; do linux_builds+=("$id"); done < <(scripts/release/build.sh linux "$message")

# 3. Check the zip.
better-update builds download "$mac_build" --output "$work/Onicode.zip"
scripts/release/verify-macos-zip.sh "$work/Onicode.zip" "$version"

# 4. Release.
better-update macos release create "$mac_build" --channel latest --notes-file "$notes"
better-update linux release create "${linux_builds[@]}" --channel latest --notes-file "$notes"

# 5. Homebrew.
scripts/release/bump-cask.sh "$version" "$(shasum -a 256 "$work/Onicode.zip" | cut -d' ' -f1)"
echo "publish.sh: Onicode $version is on the latest macOS and Linux feeds and in aislopware/tap"
