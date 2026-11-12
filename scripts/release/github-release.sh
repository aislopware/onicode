#!/usr/bin/env bash
# Publishes the GitHub release for the pushed tag v<version> with the notes. It carries no builds:
# the desktop app is downloaded from better-update's feeds alone, and updates itself from there.
#
#   scripts/release/github-release.sh <version> <notes-file>
#
# Needs gh with write access to aislopware/onicode (GH_TOKEN on Actions). The tag must exist: GitHub
# refuses a token without the workflow scope a new tag once main's workflows have moved past it.
set -euo pipefail
[[ $# -eq 2 ]] || { echo 'usage: github-release.sh <version> <notes-file>' >&2; exit 2; }

gh release create "v$1" --repo aislopware/onicode --verify-tag \
  --title "Onicode $1" --notes-file "$2" --latest
