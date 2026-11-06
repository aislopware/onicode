#!/usr/bin/env bash
# The iOS lane, on an office Mac (scripts/ci/mac.sh hands SUITE ios-pr and ios-nightly here).
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

# The first available iPhone simulator on iOS 27.2.
devices=$(xcrun simctl list --json devices available)
udid=$(
  python3 -I -c '
import json, sys
devices = json.load(sys.stdin)["devices"]
for runtime, entries in sorted(devices.items()):
    if runtime.endswith("iOS-27-2"):
        for entry in entries:
            if entry["name"].startswith("iPhone"):
                print(entry["udid"])
                sys.exit(0)
sys.exit("ios.sh: no iPhone simulator on iOS 27.2")
' <<<"$devices"
)

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
