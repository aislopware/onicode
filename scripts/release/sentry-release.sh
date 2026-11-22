#!/usr/bin/env bash
# Records a shipped release in Sentry: its commits, its finish, and a production deploy, so issues
# name suspect commits and the release that fixed them. Run from a checkout with full history.
#
#   scripts/release/sentry-release.sh <project> <version>
#
# Needs SENTRY_AUTH_TOKEN, SENTRY_URL and SENTRY_ORG, from the environment or the repository's
# .env.local or .env; without the token it does nothing. A failure only warns: a release must not
# wait on Sentry.
set -uo pipefail
[[ $# -eq 2 ]] || { echo 'usage: sentry-release.sh <project> <version>' >&2; exit 2; }
root="$(cd "$(dirname "$0")/../.." && pwd)"

# The environment wins, then .env.local, then .env, as for the builds.
for name in SENTRY_AUTH_TOKEN SENTRY_URL SENTRY_ORG; do
  [[ -n "${!name:-}" ]] && continue
  for file in "$root/.env.local" "$root/.env"; do
    value="$(sed -n "s/^${name}=//p" "$file" 2> /dev/null | tail -1)"
    value="${value%\"}" value="${value#\"}"
    if [[ -n "$value" ]]; then
      export "$name=$value"
      break
    fi
  done
done
[[ -n "${SENTRY_AUTH_TOKEN:-}" ]] || exit 0

project="$1"
release="$1@$2"
# sentry-cli names the commits' repository after this value rather than a git remote's URL.
export SENTRY_VCS_REMOTE="${GITHUB_REPOSITORY:-aislopware/onicode}"

# Bounded, so a hung request warns instead of holding the release. macOS has no `timeout`.
cli() {
  if command -v timeout > /dev/null; then
    timeout 120 npx --yes @sentry/cli@3.8.2 "$@"
  else
    npx --yes @sentry/cli@3.8.2 "$@"
  fi
}
if cli releases new --project "$project" "$release" \
  && cli releases set-commits --local --ignore-missing "$release" \
  && cli releases finalize "$release" \
  && cli deploys new --release "$release" --env production; then
  echo "Recorded $release in Sentry."
else
  echo "::warning::Recording $release in Sentry failed."
fi
