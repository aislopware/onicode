#!/usr/bin/env bash

set -euo pipefail

usage() {
  echo "Usage: $0 <server-port> <base-dir> <mobile-origin> <agent-device-command> <target-args...>" >&2
  exit 2
}

[[ $# -ge 5 ]] || usage

server_port="$1"
base_dir="$2"
mobile_origin="$3"
agent_device_command="$4"
shift 4

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if ! pairing_output="$({
  ONICODE_PORT="$server_port" node apps/server/src/bin.ts auth pairing create \
    --base-dir "$base_dir" \
    --base-url "$mobile_origin" \
    --ttl 15m \
    --label "agent-mobile"
} 2>&1)"; then
  echo "Could not mint a mobile pairing credential." >&2
  exit 1
fi

pairing_url="$(printf '%s\n' "$pairing_output" | sed -n 's/^Pair URL: //p' | tail -n 1)"
if [[ -z "$pairing_url" ]]; then
  echo "Could not parse the mobile pairing URL." >&2
  exit 1
fi

deep_link="$(PAIRING_URL="$pairing_url" node - <<'NODE'
const query = new URLSearchParams({
  pairingUrl: process.env.PAIRING_URL,
  autoConnect: "1",
});
process.stdout.write(`onicode-dev://connections/new?${query}`);
NODE
)"

if ! "$agent_device_command" open dev.aislopware.onicode.dev "$deep_link" "$@" \
  >/dev/null 2>&1; then
  echo "AgentDevice could not open the pairing route. Check the Device panel and retry with a fresh credential." >&2
  exit 1
fi

echo "Opened the existing Add Environment route with a fresh pairing credential."

# forget-client.sh takes this id to remove the connection at teardown.
if environment_id="$(curl -fsS --max-time 5 "http://127.0.0.1:$server_port/.well-known/onicode/environment" |
  node -e 'process.stdout.write(JSON.parse(require("node:fs").readFileSync(0, "utf8")).environmentId)')"; then
  echo "Environment ID: $environment_id"
else
  echo "Could not read the server's environment ID from port $server_port." >&2
fi
