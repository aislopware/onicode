#!/usr/bin/env bash
# Makes the office Macs' CI image with tart and pushes it to Zot on office-1 (docs/onicode/ci.md).
#
#   build.sh make    clone the base, provision it (provision.sh), shrink it (shrink.sh)
#   build.sh push    push it as <date> and latest, then print its digest and compressed size
#
# push reads Zot's publish account from TART_REGISTRY_USERNAME/TART_REGISTRY_PASSWORD, or from the
# login keychain (generic password "zot.jmango360.dev onicode-publisher"). Pin the printed digest in
# scripts/ci/office-mac.gitlab-ci.yml and copy that file to cong.tran/onicode-ci.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

BASE=ghcr.io/cirruslabs/macos-golden-gate-xcode:27.2
NAME=onicode-ci-27
REGISTRY=zot.jmango360.dev
REPOSITORY=onicode/macos-27-ci

make() {
  tart pull "$BASE" --concurrency 8
  tart delete "$NAME-making" 2>/dev/null || true
  tart clone "$BASE" "$NAME-making"
  tart run --no-graphics --no-audio "$NAME-making" &
  local run=$!
  until tart exec "$NAME-making" true 2>/dev/null; do sleep 2; done
  tart exec -i "$NAME-making" bash -s <"$here/provision.sh"
  tart exec -i "$NAME-making" bash -s <"$here/shrink.sh"
  tart exec "$NAME-making" cat /etc/onicode-ci-image
  tart stop "$NAME-making" --timeout 120
  wait "$run" || true
  tart delete "$NAME" 2>/dev/null || true
  tart rename "$NAME-making" "$NAME"
}

push() {
  export TART_REGISTRY_USERNAME="${TART_REGISTRY_USERNAME:-onicode-publisher}"
  if [[ -z "${TART_REGISTRY_PASSWORD:-}" ]]; then
    TART_REGISTRY_PASSWORD="$(security find-generic-password -s "$REGISTRY $TART_REGISTRY_USERNAME" -w)"
    export TART_REGISTRY_PASSWORD
  fi
  local date
  date="$(date -u +%Y-%m-%d)"
  # Zot ends any request after 60 s: each layer goes in 100 MB requests. A push again skips the
  # layers Zot already has.
  local tries=0
  until tart push "$NAME" "$REGISTRY/$REPOSITORY:$date" "$REGISTRY/$REPOSITORY:latest" \
    --concurrency 8 --chunk-size 100; do
    tries=$((tries + 1))
    [[ "$tries" -lt 5 ]] || exit 1
  done
  local tmp
  tmp="$(mktemp -d)"
  printf 'user = "%s:%s"\n' "$TART_REGISTRY_USERNAME" "$TART_REGISTRY_PASSWORD" \
    | curl -fsS --config - -D "$tmp/headers" -o "$tmp/manifest" \
      -H 'Accept: application/vnd.oci.image.manifest.v1+json' \
      "https://$REGISTRY/v2/$REPOSITORY/manifests/$date"
  local digest
  digest="$(awk 'tolower($1) == "docker-content-digest:" { print $2 }' "$tmp/headers" | tr -d '\r')"
  echo "image: $REGISTRY/$REPOSITORY@$digest"
  python3 -c 'import json, sys; m = json.load(open(sys.argv[1])); print("layers:", len(m["layers"]), "compressed GB:", round((m["config"]["size"] + sum(l["size"] for l in m["layers"])) / 1e9, 2))' "$tmp/manifest"
  rm -rf "$tmp"
}

case "${1:-}" in
  make) make ;;
  push) push ;;
  *)
    echo "usage: $0 make|push" >&2
    exit 2
    ;;
esac
