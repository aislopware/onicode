#!/usr/bin/env bash
# The two-Mac smoke (crates/engine/tests/peers_live.rs): this build's onicode-engine copied to
# another Mac of the tailnet over SSH and started there with data and home folders of its own,
# then this Mac's engine finds it and times forwarded calls and keystroke echoes. The other engine
# is stopped and its folder removed afterwards, whatever the test did.
#   two-macs.sh SSH_HOST [TAILNET_NAME]   TAILNET_NAME defaults to the other Mac's `hostname -s`
set -euo pipefail
cd "$(dirname "$0")/../.."
. scripts/dev/env.sh

host=${1:?usage: two-macs.sh SSH_HOST [TAILNET_NAME]}
cargo build --locked --bin onicode-engine
engine="${CARGO_TARGET_DIR}/debug/onicode-engine"

# Every remote command takes its folder as an argument, so nothing expands on this side.
on_host() { ssh "${host}" bash -s -- "$@"; }

remote=$(on_host <<<'mktemp -d /tmp/onicode-two-macs.XXXXXX')
# shellcheck disable=SC2329 # run by the EXIT trap
cleanup() {
  ssh "${host}" bash -s -- "${remote}" <<'EOF' || true
if [[ -f "$1/pid" ]]; then kill "$(cat "$1/pid")" 2>/dev/null || true; fi
rm -rf "$1"
EOF
}
trap cleanup EXIT

# Stripped and compressed: a debug engine is ~75 MB, and the other Mac may sit on a slow link.
stripped=$(mktemp -d)
trap 'cleanup; rm -rf "${stripped}"' EXIT
strip -S -x -o "${stripped}/onicode-engine" "${engine}"
scp -C -q -o ServerAliveInterval=5 "${stripped}/onicode-engine" "${host}:${remote}/onicode-engine"
on_host "${remote}" <<'EOF'
mkdir -p "$1/data" "$1/home"
HOME="$1/home" ONICODE_PEERS=1 nohup "$1/onicode-engine" --data-dir "$1/data" \
  >"$1/engine.log" 2>&1 </dev/null &
echo $! >"$1/pid"
EOF
name=${2:-$(on_host <<<'hostname -s' | tr '[:upper:]' '[:lower:]')}

status=0
ONICODE_PEERS_LIVE="${name}" cargo nextest run --locked --run-ignored only --no-capture \
  -E 'binary(peers_live)' || status=$?
if [[ "${status}" -ne 0 ]]; then
  echo "--- the other Mac's engine.log"
  ssh "${host}" bash -s -- "${remote}" <<'EOF' || true
tail -40 "$1/engine.log"
EOF
fi
exit "${status}"
