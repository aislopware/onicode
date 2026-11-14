#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODULE_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_DIR="$(cd "${MODULE_DIR}/../../../.." && pwd)"
OUTPUT="${MODULE_DIR}/Vendor/libghostty-vt.xcframework"

GHOSTTY_REVISION="${GHOSTTY_REVISION:-$(cat "${REPO_DIR}/native/libghostty-vt/VERSION")}"
GHOSTTY_SOURCE_DIR="${GHOSTTY_SOURCE_DIR:-${HOME}/.cache/onicode/ghostty-${GHOSTTY_REVISION:0:8}}"
GHOSTTY_ZIG_VERSION="${GHOSTTY_ZIG_VERSION:-0.17.1}"
GHOSTTY_ZIG="${GHOSTTY_ZIG:-}"
# Keep in sync with s.platforms in OniTerminalNative.podspec.
IOS_DEPLOYMENT_TARGET="16.1"

log() {
  printf '[libghostty-vt-ios] %s\n' "$*"
}

die() {
  printf '[libghostty-vt-ios] error: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

ensure_zig() {
  if [[ -n "${GHOSTTY_ZIG}" ]]; then
    [[ -x "${GHOSTTY_ZIG}" ]] || die "GHOSTTY_ZIG is not executable: ${GHOSTTY_ZIG}"
    return
  fi
  if command -v zig >/dev/null 2>&1 && [[ "$(zig version)" == "${GHOSTTY_ZIG_VERSION}" ]]; then
    GHOSTTY_ZIG="$(command -v zig)"
    return
  fi

  local host_arch cache_dir
  host_arch="$(uname -m)"
  case "${host_arch}" in
    arm64) host_arch="aarch64" ;;
    x86_64) ;;
    *) die "unsupported macOS architecture for Zig download: ${host_arch}" ;;
  esac

  cache_dir="${HOME}/.cache/onicode/zig-${GHOSTTY_ZIG_VERSION}"
  GHOSTTY_ZIG="${cache_dir}/zig"
  if [[ -x "${GHOSTTY_ZIG}" ]]; then
    return
  fi

  require_cmd curl
  require_cmd tar
  mkdir -p "${cache_dir}"
  log "downloading Zig ${GHOSTTY_ZIG_VERSION}"
  curl -fsSL \
    "https://ziglang.org/download/${GHOSTTY_ZIG_VERSION}/zig-${host_arch}-macos-${GHOSTTY_ZIG_VERSION}.tar.xz" \
    | tar -xJ --strip-components=1 -C "${cache_dir}"
}

ensure_ghostty_source() {
  if [[ ! -d "${GHOSTTY_SOURCE_DIR}/.git" ]]; then
    log "cloning Ghostty ${GHOSTTY_REVISION}"
    git clone --filter=blob:none --no-checkout https://github.com/ghostty-org/ghostty.git \
      "${GHOSTTY_SOURCE_DIR}"
    git -C "${GHOSTTY_SOURCE_DIR}" fetch --depth=1 origin "${GHOSTTY_REVISION}"
    git -C "${GHOSTTY_SOURCE_DIR}" checkout --detach "${GHOSTTY_REVISION}"
  fi

  local actual_revision
  actual_revision="$(git -C "${GHOSTTY_SOURCE_DIR}" rev-parse HEAD)"
  [[ "${actual_revision}" == "${GHOSTTY_REVISION}" ]] || \
    die "expected Ghostty ${GHOSTTY_REVISION}, found ${actual_revision}"
  [[ -z "$(git -C "${GHOSTTY_SOURCE_DIR}" status --porcelain --untracked-files=no)" ]] || \
    die "${GHOSTTY_SOURCE_DIR} has local changes; iOS builds use unpatched upstream"
}

[[ "$(uname -s)" == "Darwin" ]] || die "building for iOS needs macOS and Xcode"
require_cmd git
require_cmd xcodebuild
require_cmd xcrun
ensure_zig
ensure_ghostty_source

build_root="$(mktemp -d)"
trap 'rm -rf "${build_root}"' EXIT

# The simulator runs on the host Mac, so its slice targets the oldest Apple silicon.
targets=(
  "ios-arm64:aarch64-ios.${IOS_DEPLOYMENT_TARGET}:"
  "ios-arm64-simulator:aarch64-ios.${IOS_DEPLOYMENT_TARGET}-simulator:apple_m1"
)

xcframework_args=()
for entry in "${targets[@]}"; do
  IFS=: read -r slice target cpu <<<"${entry}"
  prefix="${build_root}/${slice}"
  log "building ${slice} (${target})"
  (
    cd "${GHOSTTY_SOURCE_DIR}"
    "${GHOSTTY_ZIG}" build \
      -Demit-lib-vt \
      -Dtarget="${target}" \
      ${cpu:+-Dcpu="${cpu}"} \
      -Doptimize=ReleaseFast \
      -Dstrip=true \
      -p "${prefix}"
  )
  archive="${prefix}/lib/libghostty-vt.a"
  [[ -f "${archive}" ]] || die "missing static library: ${archive}"
  xcrun strip -S -x "${archive}"
  xcframework_args+=(-library "${archive}")
done

rm -rf "${OUTPUT}"
xcodebuild -create-xcframework "${xcframework_args[@]}" -output "${OUTPUT}"
log "wrote ${OUTPUT}"
