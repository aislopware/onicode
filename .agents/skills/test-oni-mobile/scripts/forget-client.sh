#!/usr/bin/env bash
# Removes the connection pair-client.sh added from Onicode Dev on the device, by the environment
# ID pair-client.sh printed. An ID the app has not saved changes nothing.

set -euo pipefail

usage() {
  echo "Usage: $0 <environment-id> <agent-device-command> <target-args...>" >&2
  exit 2
}

[[ $# -ge 3 && -n "${1//[[:space:]]/}" ]] || usage

environment_id="$1"
agent_device_command="$2"
shift 2

deep_link="$(ENVIRONMENT_ID="$environment_id" node - <<'NODE'
const query = new URLSearchParams({ environmentId: process.env.ENVIRONMENT_ID });
process.stdout.write(`onicode-dev://connections/remove?${query}`);
NODE
)"

if ! "$agent_device_command" open dev.aislopware.onicode.dev "$deep_link" "$@" \
  >/dev/null 2>&1; then
  echo "AgentDevice could not open the remove link. Check the Device panel, or remove the environment in Settings." >&2
  exit 1
fi

echo "Asked Onicode Dev to forget environment $environment_id."
