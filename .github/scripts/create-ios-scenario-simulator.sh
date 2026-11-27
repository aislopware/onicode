#!/usr/bin/env bash
# Creates the slim "Scenario iPhone" the iOS scenarios run on. A first boot runs a data migration
# that can take ten minutes, so the self-hosted runner pays it once per runtime and device type in
# a template it keeps, and each run clones that. A hosted runner starts clean and creates one.
# Runs on macOS bash 3.2.
set -euo pipefail

name="Scenario iPhone"
# Remote push (apsd) and universal links (swcd) stay on.
keep=store,web

# The newest iOS runtime and the first iPhone it supports: not every device type boots on every
# runtime. Runtimes for one device only, such as iOS-27-2-iPhone19-4, are left out.
read -r runtime type < <(xcrun simctl list runtimes available -j | jq -er '
  [.runtimes[] | select(.platform == "iOS" and (.identifier | test("iOS-[0-9-]+$")))]
  | max_by(.version | split(".") | map(tonumber))
  | "\(.identifier) \(first(.supportedDeviceTypes[] | select(.productFamily == "iPhone")).identifier)"')

command -v simslim > /dev/null || brew install mobai-app/tap/simslim
# clone arrived in simslim 0.15; a runner's older install upgrades once.
simslim --help 2>&1 | grep -q '^  clone ' || brew upgrade mobai-app/tap/simslim

if [[ "${RUNNER_ENVIRONMENT:-}" != self-hosted ]]; then
  udid="$(xcrun simctl create "$name" "$type" "$runtime")"
  simslim on "$udid" --except "$keep" --preserve-boot-state
  exit 0
fi

# The runner takes one job at a time, so no other run builds a template meanwhile. A half-made
# one keeps its "preparing" name, which reset-ios-scenarios.sh deletes.
template="$name template ${runtime##*.} ${type##*.}"
preparing="$name preparing ${runtime##*.} ${type##*.}"
template_udid="$(xcrun simctl list devices -j | jq -r --arg runtime "$runtime" --arg name "$template" \
  'first(.devices[$runtime][]? | select(.name == $name and .isAvailable) | .udid) // empty')"
if [[ -z "$template_udid" ]]; then
  echo "Preparing $template; its first boot migrates data and runs once."
  udid="$(xcrun simctl create "$preparing" "$type" "$runtime")"
  xcrun simctl bootstatus "$udid" -b
  SIMSLIM_BOOT_TIMEOUT=30m simslim on "$udid" --except "$keep"
  xcrun simctl shutdown "$udid"
  xcrun simctl rename "$udid" "$template"
  template_udid="$udid"
fi

# simslim's clone carries the template's disable overrides, which simctl clone drops, and rebases
# the paths a raw clone leaves pointing into the template.
simslim clone "$template_udid" "$name"
