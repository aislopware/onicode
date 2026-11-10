#!/usr/bin/env bash
# Builds one platform with better-update's production profile and prints the ids of the builds it
# made, one per line. The build's own output goes to stderr.
#
#   scripts/release/build.sh <macos|linux|ios|android> <message>
#
# ios and android build from apps/mobile; macos and linux from the repo root (eas.json).
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
[[ $# -eq 2 ]] || { echo 'usage: build.sh <platform> <message>' >&2; exit 2; }
platform="$1"
case "$platform" in
  macos | linux) dir="$root" ;;
  ios | android) dir="$root/apps/mobile" ;;
  *) echo "build.sh: unknown platform $platform" >&2; exit 2 ;;
esac
version="$(node -p 'require(process.argv[1]).version' "$root/apps/desktop/package.json")"

started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
(cd "$dir" && better-update build --platform "$platform" --profile production --message "$2") >&2
ids="$(better-update builds list --limit 30 --json | jq -r --arg p "$platform" --arg v "$version" \
  --arg t "$started" '.data.items[] | select(.Platform == $p and .Version == $v and .Created >= $t) | .ID')"
[[ -n "$ids" ]] || { echo "build.sh: better-update has no $platform build of $version since $started" >&2; exit 1; }
echo "$ids"
