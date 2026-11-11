#!/usr/bin/env bash
# Usage: install-apt-packages.sh <deb-cache-dir> <package>...
# Cached .debs install without a mirror. Otherwise apt uses the fastest mirror that
# answers, since the runner's Azure mirror, and sometimes archive.ubuntu.com, stall for hours.
set -uo pipefail

cache_dir=$1
shift

if compgen -G "$cache_dir/*.deb" > /dev/null; then
  # apt orders the pre-dependencies that dpkg -i trips over. With no sources it can't
  # prefer a mirror's copy of a cached version, so this never touches the network.
  no_sources=$(mktemp -d)
  sudo apt-get install -y -o Dir::Etc::SourceList=/dev/null -o Dir::Etc::SourceParts="$no_sources" \
    "$cache_dir"/*.deb && exit 0
  echo "Installing the cached packages failed; installing from a mirror instead."
fi

candidates=(
  http://azure.archive.ubuntu.com/ubuntu/
  http://mirrors.edge.kernel.org/ubuntu/
  http://archive.ubuntu.com/ubuntu/
  http://mirror.math.princeton.edu/pub/ubuntu/
  http://ftp.osuosl.org/pub/ubuntu/
)
codename=$(sed -n 's/^VERSION_CODENAME=//p' /etc/os-release)
probe="dists/$codename-updates/main/binary-$(dpkg --print-architecture)/Packages.xz"

# A stalled mirror times out and a slow one (under ~150 kB/s) can't fetch this
# 1.4 MB index in 10s. apt's own timeouts catch neither case reliably.
ranked=()
while read -r _ mirror; do ranked+=("$mirror"); done < <(
  for mirror in "${candidates[@]}"; do
    (time=$(curl -fsS -o /dev/null -m 10 -w '%{time_total}' "$mirror$probe") && echo "$time $mirror") &
  done | sort -n
)
echo "Mirrors by speed: ${ranked[*]:-none answered}"
test "${#ranked[@]}" -gt 0 || ranked=("${candidates[@]}")

# The image's sources read this list, and its cached indexes belong to that URI,
# so switching mirrors behind it fetches only what changed.
net=(-o Acquire::Retries=1 -o Acquire::http::Timeout=20 -o Acquire::https::Timeout=20)
# apt downloads into the cache directory itself, where no apt clean hook deletes them.
sudo mkdir -p "$cache_dir/partial"
status=1
for attempt in 1 2 3; do
  test "${#ranked[@]}" -gt 0 || break
  # Without priorities apt spreads requests over every listed mirror.
  for i in "${!ranked[@]}"; do printf '%s\tpriority:%d\n' "${ranked[$i]}" "$((i + 1))"; done |
    sudo tee /etc/apt/apt-mirrors.txt > /dev/null
  # Skip translations and the DEP-11 and command-not-found indexes these packages don't need.
  sudo timeout 90 apt-get update "${net[@]}" -o Acquire::Languages=none \
    -o Acquire::IndexTargets::deb::DEP-11::DefaultEnabled=false \
    -o Acquire::IndexTargets::deb::CNF::DefaultEnabled=false &&
    sudo timeout 90 apt-get install -y "${net[@]}" -o Dir::Cache::Archives="$cache_dir" "$@"
  status=$?
  test "$status" -eq 0 && break
  echo "apt attempt $attempt through ${ranked[0]} failed with status $status"
  ranked=("${ranked[@]:1}")
done
test "$status" -eq 0 || exit "$status"

# Only the .debs go in the cache; apt's partial directory and lock aren't readable by the runner.
sudo rm -rf "$cache_dir/partial" "$cache_dir/lock"
