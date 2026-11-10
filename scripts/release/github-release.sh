#!/usr/bin/env bash
# Publishes the GitHub release for the pushed tag v<version> with the notes and the desktop builds
# attached, downloaded from better-update, plus a SHA256SUMS of them.
#
#   scripts/release/github-release.sh <version> <notes-file> <macos-build> <linux-build>...
#
# Needs gh with write access to aislopware/onicode (GH_TOKEN on Actions). The tag must exist: GitHub
# refuses a token without the workflow scope a new tag once main's workflows have moved past it.
set -euo pipefail
[[ $# -ge 4 ]] || { echo 'usage: github-release.sh <version> <notes-file> <macos-build> <linux-build>...' >&2; exit 2; }
version="$1"
notes="$2"
mac_build="$3"
shift 3
assets="$(mktemp -d)"
trap '/bin/rm -rf "$assets"' EXIT

better-update builds download "$mac_build" --output "$assets/Onicode-$version-mac-arm64.zip" >&2
for id in "$@"; do
  # Downloads land as <id>.<format>, such as <id>.appimage or <id>.deb.
  (cd "$assets" && better-update builds download "$id" >&2)
  file="$(find "$assets" -maxdepth 1 -name "$id.*")"
  case "${file##*.}" in
    appimage) mv "$file" "$assets/Onicode-$version-linux-x64.AppImage" ;;
    deb) mv "$file" "$assets/Onicode-$version-linux-x64.deb" ;;
    *) echo "github-release.sh: unexpected Linux artifact ${file##*/}" >&2; exit 1 ;;
  esac
done
(cd "$assets" && shasum -a 256 Onicode-* > SHA256SUMS)

gh release create "v$version" "$assets"/* --repo aislopware/onicode --verify-tag \
  --title "Onicode $version" --notes-file "$notes" --latest
