#!/bin/bash
# Provisions the office Macs' CI image (build.sh make) in a tart guest of
# ghcr.io/cirruslabs/macos-golden-gate-xcode:27.2: what every job would otherwise fetch and that changes
# only with a pin here. Toolchains, the registry and the target stay on the runners' CICache.
# Runs as admin (passwordless sudo) through `tart exec -i <vm> bash -s < provision.sh`.
set -euxo pipefail
export PATH=/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin
export HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1 HOMEBREW_NO_ANALYTICS=1

ZIG=0.17.0
ZIG_SHA=b607e9b9234790a008116ae5bdb71c6243b84b9fb42a53a9e70fde41c06c536a
NEXTEST=0.9.146
NEXTEST_SHA=39785160b3c2f6ed9a765049cf4fa79f3b39aa02eb7598a5a0e2a1a0b9ffb9a8
RUSTUP=1.29.3
RUSTUP_SHA=63752a1919a3a4793173ec70f8f212621a7ce38ba6a246095984482f4de20f9f
SWEEP=0.8.0

fetch() { curl --proto '=https' --tlsv1.2 -fsSL --retry 5 -o "$2" "$1"; }
check() { echo "$2  $1" | shasum -a 256 -c -; }

# The base is what the lane needs: macOS 27.2 with Xcode 27.2.
macos="$(sw_vers -productVersion)"
xcode="$(xcodebuild -version)"
[[ "$macos" == 27.2 && "${xcode%%$'\n'*}" == "Xcode 27.2" ]]

sudo mkdir -p /usr/local/bin

# Xcode's Metal toolchain: gpui-fast compiles its shaders at build time.
xcrun metal --version >/dev/null 2>&1 || xcodebuild -downloadComponent MetalToolchain
xcrun metal --version

# The iOS 27.2 simulator runtime and one iPhone settled (first boot done).
# The release runtime; the base also has 27.2 builds for other platforms and devices, which
# shrink.sh deletes.
runtime=com.apple.CoreSimulator.SimRuntime.iOS-27-2
has_runtime='import json, sys
sys.exit(not any(r["identifier"] == sys.argv[1] and r["isAvailable"] for r in json.load(sys.stdin)["runtimes"]))'
xcrun simctl list runtimes -j | python3 -c "$has_runtime" "$runtime" || xcodebuild -downloadPlatform iOS
xcrun simctl list runtimes -j | python3 -c "$has_runtime" "$runtime"
# The iOS lane targets it by name: -destination 'platform=iOS Simulator,OS=27.2,name=iPhone CI'.
device=$(xcrun simctl list devices -j | python3 -c '
import json, sys
ds = [d for d in json.load(sys.stdin)["devices"].get(sys.argv[1], []) if d["isAvailable"] and d["name"] == "iPhone CI"]
print(ds[0]["udid"] if ds else "")' "$runtime")
if [[ -z "$device" ]]; then
  type=$(xcrun simctl list runtimes -j | python3 -c '
import json, sys
r = next(r for r in json.load(sys.stdin)["runtimes"] if r["identifier"] == sys.argv[1])
ts = {t["name"]: t["identifier"] for t in r["supportedDeviceTypes"] if t["productFamily"] == "iPhone" and t["name"].startswith("iPhone")}
print(ts.get("iPhone 18 Pro") or list(ts.values())[-1])' "$runtime")
  device=$(xcrun simctl create "iPhone CI" "$type" "$runtime")
fi
# Booted once so its first-boot setup is done, then shut down: jobs boot it on demand.
xcrun simctl bootstatus "$device" -b
sleep 30
xcrun simctl shutdown "$device"

# zig for ghostty's libghostty-vt.
fetch "https://ziglang.org/download/$ZIG/zig-aarch64-macos-$ZIG.tar.xz" /tmp/zig.tar.xz
check /tmp/zig.tar.xz "$ZIG_SHA"
sudo rm -rf "/usr/local/zig-$ZIG"
sudo mkdir -p "/usr/local/zig-$ZIG"
sudo tar -xJf /tmp/zig.tar.xz -C "/usr/local/zig-$ZIG" --strip-components 1
sudo ln -sfn "/usr/local/zig-$ZIG/zig" /usr/local/bin/zig
rm /tmp/zig.tar.xz

# cargo-nextest.
fetch "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$NEXTEST/cargo-nextest-$NEXTEST-universal-apple-darwin.tar.gz" /tmp/nextest.tgz
check /tmp/nextest.tgz "$NEXTEST_SHA"
sudo tar -xzf /tmp/nextest.tgz -C /usr/local/bin cargo-nextest
rm /tmp/nextest.tgz

# rustup itself, with its proxies; no toolchain. Jobs set RUSTUP_HOME and CARGO_HOME on CICache.
fetch "https://static.rust-lang.org/rustup/archive/$RUSTUP/aarch64-apple-darwin/rustup-init" /tmp/rustup-init
check /tmp/rustup-init "$RUSTUP_SHA"
sudo install -m 755 /tmp/rustup-init /usr/local/bin/rustup
rm /tmp/rustup-init
for proxy in cargo rustc rustdoc rustfmt cargo-fmt cargo-clippy clippy-driver rust-gdb rust-gdbgui rust-lldb rust-analyzer cargo-miri; do
  sudo ln -sfn rustup "/usr/local/bin/$proxy"
done

# cargo-sweep, built with a throwaway toolchain that does not stay in the image.
export RUSTUP_HOME=/tmp/sweep-rustup CARGO_HOME=/tmp/sweep-cargo
rustup toolchain install stable --profile minimal --no-self-update
rustup default stable
cargo install --locked --quiet --root /tmp/sweep "cargo-sweep@$SWEEP"
sudo install -m 755 /tmp/sweep/bin/cargo-sweep /usr/local/bin/cargo-sweep
rm -rf /tmp/sweep /tmp/sweep-rustup /tmp/sweep-cargo
unset RUSTUP_HOME CARGO_HOME

# Quiet: no indexing, no updates.
sudo mdutil -a -i off
sudo softwareupdate --schedule off || true
sudo defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticCheckEnabled -bool false
sudo defaults write /Library/Preferences/com.apple.SoftwareUpdate AutomaticDownload -bool false
sudo defaults write /Library/Preferences/com.apple.SoftwareUpdate CriticalUpdateInstall -bool false
sudo defaults write /Library/Preferences/com.apple.SoftwareUpdate ConfigDataInstall -bool false
sudo defaults write /Library/Preferences/com.apple.commerce AutoUpdate -bool false

# The stamp, /etc/onicode-ci-image.
stamp=/tmp/onicode-ci-image
version() {
  local out
  out="$("${@:2}" 2>&1)"
  printf '%s: %s\n' "$1" "${out//$'\n'/ }" >>"$stamp"
}
made="$(date -u +%Y-%m-%dT%H:%MZ)"
printf 'made: %s from ghcr.io/cirruslabs/macos-golden-gate-xcode:27.2\n' "$made" >"$stamp"
version macos sw_vers -productVersion
version macos-build sw_vers -buildVersion
version xcode xcodebuild -version
version metal xcrun metal --version
version ios-runtime echo "$runtime"
version ios-device echo "iPhone CI $device"
version zig zig version
version nextest cargo-nextest nextest --version
version rustup rustup --version
version cargo-sweep cargo-sweep sweep --version
version git git --version
version python3 python3 --version
sudo install -m 644 "$stamp" /etc/onicode-ci-image
rm "$stamp"
