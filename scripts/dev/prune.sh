#!/usr/bin/env bash
# Keep the dev target dir in budget: over 20 GB, drop artifacts unused for 3 days and stale
# incremental state; over 30 GB, start clean (one cold build is ~6 min).
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/env.sh
size() { du -sg "$CARGO_TARGET_DIR" 2>/dev/null | awk '{print $1}'; }
[ "$(size)" -le 20 ] && exit 0
if command -v cargo-sweep >/dev/null; then cargo sweep --time 3 >/dev/null 2>&1 || true; fi
find "$CARGO_TARGET_DIR"/*/incremental -mindepth 1 -maxdepth 1 -mtime +1 -exec /bin/rm -rf {} + 2>/dev/null || true
if [ "$(size)" -gt 30 ]; then
  echo "prune: target dir over 30 GB, cleaning" >&2
  /bin/rm -rf "$CARGO_TARGET_DIR"
fi
