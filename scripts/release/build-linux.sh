#!/usr/bin/env bash
# Builds the Linux AppImage and deb into release/linux. eas.json's custom.linux command, so
# `better-update build --platform linux` runs it in its staged copy of the repo.
#
#   scripts/release/build-linux.sh [x64|arm64]
#
# The build runs in a Linux container (Docker or OrbStack) because the packaged native modules and
# Rust helpers must be Linux builds; the host's node_modules never enter it.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
arch="${1:-x64}"
case "$arch" in
  x64) platform=linux/amd64 ;;
  arm64) platform=linux/arm64 ;;
  *) echo "build-linux.sh: unknown arch $arch" >&2; exit 2 ;;
esac
pnpm_version="$(node -p 'require(process.argv[1]).packageManager.split("@")[1]' "$root/package.json")"
image="onicode-linux-build:$arch"
out="$root/release/linux"

docker build --platform "$platform" --build-arg "PNPM_VERSION=$pnpm_version" \
  --tag "$image" --file "$root/scripts/release/linux.Dockerfile" "$root/scripts/release"
mkdir -p "$out"
docker run --rm --platform "$platform" \
  --volume "$root:/src:ro" --volume "$out:/out" \
  --volume "onicode-linux-cache-$arch:/root/.cache" \
  --volume "onicode-linux-pnpm-$arch:/root/.local/share/pnpm" \
  --volume "onicode-linux-cargo-$arch:/usr/local/cargo/registry" \
  --env ONICODE_RELEASE_CHANNEL="${ONICODE_RELEASE_CHANNEL:-stable}" --env ARCH="$arch" \
  "$image" bash -euo pipefail -c '
    mkdir /work
    tar -C /src --exclude=node_modules --exclude=./release --exclude="./native/*/target" -cf - . \
      | tar -C /work -xf -
    cd /work
    pnpm install --frozen-lockfile
    export PATH="/work/node_modules/.bin:$PATH"
    node scripts/build-desktop-artifact.ts --platform linux --target AppImage --arch "$ARCH" \
      --output-dir /out --verbose
  '
