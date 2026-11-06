#!/usr/bin/env bash
# This checkout's app against the staging edge: the whole stack production
# runs (edge, vault, Durable Objects, Workflows, R2 on Cloudflare; real Boat
# machines; Stripe in test mode; WorkOS sign-in in its test environment),
# deployed beside production as `--env staging` (edge/wrangler.jsonc).
# Nothing here runs on this Mac but the app, so a session, a tunnel or a
# terminal going away stops nothing.
#
# The app is the dev bundle (scripts/run-macos-dev.sh) with its own data dir
# and engine port, so it runs beside your installed Onicode. Sign in as usual;
# Cloud, Providers → Cloud and Billing all talk to staging.
#
#   scripts/staging-app.sh            # build and open
#   ONICODE_DEV_BUILD_ONLY=1 scripts/staging-app.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export ONICODE_EDGE_URL="${ONICODE_EDGE_URL:-https://onicode-edge-staging.sylasincorporated.workers.dev}"
export ONICODE_DEV_DATA_DIR="${ONICODE_DEV_DATA_DIR:-$HOME/.onicode-staging}"
export ONICODE_DEV_IPC_PORT="${ONICODE_DEV_IPC_PORT:-27871}"
# Staging signs in through WorkOS's test environment (its own users and
# orgs), on a loopback callback of its own so it never contends with the
# installed app's (27641). WorkOS → test → Redirects must list
# http://127.0.0.1:27642/callback.
export ONICODE_WORKOS_CLIENT_ID="${ONICODE_WORKOS_CLIENT_ID:-client_01KWD0E9SYPV2WE4QN9TP9DP0W}"
export ONICODE_CALLBACK_PORT="${ONICODE_CALLBACK_PORT:-27642}"
# One valid Apple Development identity by hash: several certificates share a
# name, which `codesign` refuses as ambiguous (revoked ones are skipped).
if [[ -z "${ONICODE_DEV_CODESIGN_IDENTITY:-}" ]]; then
  ONICODE_DEV_CODESIGN_IDENTITY="$(security find-identity -v -p codesigning 2>/dev/null |
    awk '/Apple Development/ && !/REVOKED/ { print $2; exit }')"
  export ONICODE_DEV_CODESIGN_IDENTITY="${ONICODE_DEV_CODESIGN_IDENTITY:--}"
fi

health="$(curl -fsS --max-time 10 "$ONICODE_EDGE_URL/health" || true)"
case "$health" in
  *'"ok":true'*) ;;
  *) echo "staging edge not answering at $ONICODE_EDGE_URL ($health)" >&2; exit 1 ;;
esac
echo "staging: $ONICODE_EDGE_URL · data $ONICODE_DEV_DATA_DIR · engine :$ONICODE_DEV_IPC_PORT" >&2
exec "$ROOT/scripts/run-macos-dev.sh" "$@"
