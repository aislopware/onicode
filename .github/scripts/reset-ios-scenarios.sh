#!/usr/bin/env bash
# Leaves the self-hosted macOS runner as the next iOS scenarios run expects it: no simulator
# running, no scenario simulator left over, and nothing listening on the scenario ports. The
# runner kills only processes that carry its tracking variable, which e2e does not pass to the
# Metro and server it starts.
# shellcheck disable=SC2046,SC2086 # PID lists split on purpose; macOS bash 3.2 has no mapfile.
set -uo pipefail

xcrun simctl shutdown all
xcrun simctl list devices -j |
  jq -r '.devices[][] | select(.name == "Scenario iPhone") | .udid' |
  while read -r udid; do xcrun simctl delete "$udid"; done

ports=("${ONICODE_SCENARIO_SERVER_PORT:-13990}" "${ONICODE_SCENARIO_METRO_PORT:-8190}")
listeners() { for port in "${ports[@]}"; do lsof -t -nP -iTCP:"$port" -sTCP:LISTEN; done; }
pids="$(listeners)"
[[ -z "$pids" ]] && exit 0
echo "Stopping what still listens on ${ports[*]}: $pids"
kill $pids
for _ in {1..20}; do
  [[ -z "$(listeners)" ]] && exit 0
  sleep 0.5
done
kill -9 $(listeners) 2>/dev/null
[[ -z "$(listeners)" ]] || { echo "::error::Ports ${ports[*]} are still in use."; exit 1; }
