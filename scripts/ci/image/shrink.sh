#!/bin/bash
# The last step of build.sh make, in the guest: drop what the lane never uses. The base's disk is
# ASIF, and the guest's TRIM reaches it, so deleted files free their blocks and leave the pushed
# image; nothing needs zeroing.
set -euxo pipefail
export PATH=/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin
keep=com.apple.CoreSimulator.SimRuntime.iOS-27-2

# Every simulator runtime but iOS 27.2's release build (tvOS, watchOS, visionOS and a device
# build of iOS: about 23 GB), and every device but provision.sh's "iPhone CI".
xcrun simctl runtime list -j | python3 -c '
import json, sys
for image, r in json.load(sys.stdin).items():
    if r.get("runtimeIdentifier") != sys.argv[1]:
        print(image)' "$keep" | while read -r image; do
  xcrun simctl runtime delete "$image"
done
xcrun simctl list devices -j | python3 -c '
import json, sys
for runtime, devices in json.load(sys.stdin)["devices"].items():
    for d in devices:
        if not (runtime == sys.argv[1] and d["name"] == "iPhone CI"):
            print(d["udid"])' "$keep" | while read -r device; do
  xcrun simctl delete "$device"
done

# The base's other stacks: Flutter, the Android SDK, Ruby, an agent CLI and a GitHub runner.
rm -rf ~/flutter ~/android-sdk ~/.android ~/.pub-cache ~/.dart-tool ~/.rbenv ~/actions-runner
sudo rm -rf "/Applications/Kiro CLI.app"

# Caches and logs.
brew cleanup --prune=all -s >/dev/null 2>&1 || true
rm -rf ~/Library/Caches/* ~/Library/Developer/Xcode/DerivedData ~/Library/Logs/* ~/.cache ~/.npm 2>/dev/null || true
sudo rm -rf /Library/Caches/* /private/var/tmp/* /private/var/log/*.gz 2>/dev/null || true

# Local snapshots would keep the deleted blocks.
for date in $(tmutil listlocalsnapshotdates / 2>/dev/null | grep -E '^[0-9]' || true); do
  sudo tmutil deletelocalsnapshots "$date" || true
done
sync
xcrun simctl runtime list
df -h /
