#!/usr/bin/env bash
# The iOS lane, on a GitHub-hosted xcode-27 runner (the ios job in .github/workflows/ci.yml).
#
#   ios.sh pr       swift format (Xcode's, which the Linux static job lacks), the mobile core for
#                   aarch64-apple-ios-sim, the committed bindings checked for drift, and the OnicodeTests unit
#                   target on an iOS 27.2 simulator, headless
#   ios.sh nightly  the same, then one short UI smoke test
set -euo pipefail
cd "$(dirname "$0")/../.."
suite="${1:-pr}"

swift format lint --strict --recursive --parallel apps/ios scripts

# Builds the core and regenerates the committed Swift bindings, C header and module map.
scripts/ios/build-core.sh iphonesimulator
if ! git diff --exit-code -- apps/ios/Onicode/Core/Generated apps/ios/CoreFFI; then
  echo "ios.sh: the committed bindings are stale; run scripts/ios/build-core.sh and commit the result" >&2
  exit 1
fi

# An iPhone simulator on iOS 27.2 or newer. A hosted image may ship the runtime with no device, or only an
# older runtime: then a device is created, after downloading the iOS platform if no 27.2 runtime exists.
pick() {
  python3 -I -c '
import json, subprocess, sys
def version(identifier):
    # com.apple.CoreSimulator.SimRuntime.iOS-27-2, sometimes with a non-numeric suffix after the version.
    parts = identifier.rsplit(".", 1)[-1].split("-")
    if parts[0] != "iOS":
        return ()
    numbers = []
    for part in parts[1:]:
        if not part.isdigit():
            break
        numbers.append(int(part))
    return tuple(numbers)
devices = json.loads(subprocess.check_output(["xcrun", "simctl", "list", "--json", "devices", "available"]))
for runtime, entries in sorted(devices["devices"].items(), key=lambda kv: version(kv[0]), reverse=True):
    if version(runtime) >= (27, 2):
        for entry in entries:
            if entry["name"].startswith("iPhone"):
                print(entry["udid"])
                sys.exit(0)
runtimes = json.loads(subprocess.check_output(["xcrun", "simctl", "list", "--json", "runtimes", "available"]))
for runtime in sorted(runtimes["runtimes"], key=lambda r: version(r["identifier"]), reverse=True):
    if version(runtime["identifier"]) >= (27, 2):
        phones = [t for t in runtime.get("supportedDeviceTypes", []) if t["name"].startswith("iPhone")]
        if phones:
            udid = subprocess.check_output(
                ["xcrun", "simctl", "create", "onicode-ci", phones[-1]["identifier"], runtime["identifier"]]
            )
            print(udid.decode().strip())
            sys.exit(0)
'
}
udid=$(pick)
if [[ -z "$udid" ]]; then
  echo "ios.sh: no iOS 27.2 simulator runtime; downloading the iOS platform" >&2
  xcodebuild -downloadPlatform iOS
  udid=$(pick)
fi
if [[ -z "$udid" ]]; then
  echo "ios.sh: no iPhone simulator on iOS 27.2 or newer" >&2
  exit 1
fi

tests=(-only-testing:OnicodeTests)
if [[ "$suite" == nightly ]]; then
  tests+=(-only-testing:OnicodeUITests/SessionFlowTests/testFrontPageShowsFoldersAndRecents)
fi

# The core is already built above; the Rust core build phase reuses it.
stamp=$(date +%s)
ONICODE_SKIP_CORE=1 xcodebuild test \
  -project apps/ios/Onicode.xcodeproj \
  -scheme Onicode \
  -destination "id=$udid" \
  -derivedDataPath target/ios-derived \
  -resultBundlePath "target/ios-results/$stamp.xcresult" \
  CODE_SIGNING_ALLOWED=NO \
  "${tests[@]}"
