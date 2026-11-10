#!/usr/bin/env bash
# Points the `onicode` cask in aislopware/homebrew-tap at the latest macOS release of <version>.
# The cask's download URL needs the release id, so its version is `<version>,<release id>`.
#
#   scripts/release/bump-cask.sh <version> <zip sha256>
#
# Needs SSH push access to aislopware/homebrew-tap (the Release workflow uses a deploy key).
set -euo pipefail
[[ $# -eq 2 ]] || { echo 'usage: bump-cask.sh <version> <sha256>' >&2; exit 2; }
version="$1"
sha256="$2"
release="$(better-update macos release list --json |
  jq -r --arg v "$version" '[.data.items[] | select(.Channel == "latest" and .Version == $v)][0].ID // empty')"
[[ -n "$release" ]] || { echo "bump-cask.sh: better-update lists no latest macOS release of $version" >&2; exit 1; }
tap="$(mktemp -d)"
trap '/bin/rm -rf "$tap"' EXIT

git clone --quiet --depth 1 git@github.com:aislopware/homebrew-tap.git "$tap"
sed -i.bak -e "s/^  version \".*\"$/  version \"$version,$release\"/" \
  -e "s/^  sha256 \".*\"$/  sha256 \"$sha256\"/" "$tap/Casks/onicode.rb"
/bin/rm "$tap/Casks/onicode.rb.bak"
if git -C "$tap" diff --quiet; then
  echo "bump-cask.sh: the cask already points at $version,$release" >&2
  exit 0
fi
git -C "$tap" commit --quiet -am "onicode $version"
git -C "$tap" push --quiet
