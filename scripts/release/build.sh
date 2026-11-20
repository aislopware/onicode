#!/usr/bin/env bash
# Builds one platform with better-update's production profile and prints the ids of the builds it
# made, one per line. The build's own output goes to stderr.
#
#   scripts/release/build.sh <macos|linux|ios|android> <message>
#   scripts/release/build.sh windows <message> <installer>...
#
# ios and android build from apps/mobile; macos and linux from the repo root (eas.json). Windows
# uploads installers built on a Windows host (.github/workflows/desktop-windows.yml).
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
platform="${1:-}"
if [[ "$platform" == windows ]]; then
  [[ $# -ge 3 ]] || { echo 'usage: build.sh windows <message> <installer>...' >&2; exit 2; }
else
  [[ $# -eq 2 ]] || { echo 'usage: build.sh <platform> <message>' >&2; exit 2; }
fi
case "$platform" in
  macos | linux | windows) dir="$root" ;;
  ios | android) dir="$root/apps/mobile" ;;
  *) echo "build.sh: unknown platform $platform" >&2; exit 2 ;;
esac
version="$(node -p 'require(process.argv[1]).version' "$root/apps/desktop/package.json")"

started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [[ "$platform" == windows ]]; then
  (cd "$dir" && better-update builds upload --platform windows --profile production --message "$2" "${@:3}") >&2
else
  (cd "$dir" && better-update build --platform "$platform" --profile production --message "$2") >&2
fi
ids="$(better-update builds list --limit 30 --json | jq -r --arg p "$platform" --arg v "$version" \
  --arg t "$started" '.data.items[] | select(.Platform == $p and .Version == $v and .Created >= $t) | .ID')"
[[ -n "$ids" ]] || { echo "build.sh: better-update has no $platform build of $version since $started" >&2; exit 1; }
echo "$ids"
