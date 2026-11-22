#!/usr/bin/env bash
# Records a shipped release in Sentry: its commits, its finish, and a production deploy, so issues
# name suspect commits and the release that fixed them. Run from a checkout with full history.
#
#   scripts/release/sentry-release.sh <project> <version>
#
# Needs SENTRY_AUTH_TOKEN, SENTRY_URL and SENTRY_ORG; without the token it does nothing. A failure
# only warns: a release must not wait on Sentry.
set -uo pipefail
[[ $# -eq 2 ]] || { echo 'usage: sentry-release.sh <project> <version>' >&2; exit 2; }
[[ -n "${SENTRY_AUTH_TOKEN:-}" ]] || exit 0

project="$1"
release="$1@$2"
# sentry-cli names the commits' repository after the git remote they come from.
repo="${GITHUB_REPOSITORY:-aislopware/onicode}"
if ! git remote get-url "$repo" > /dev/null 2>&1; then
  git remote add "$repo" "$(git remote get-url origin)"
  trap 'git remote remove "$repo"' EXIT
fi

cli() { npx --yes @sentry/cli@3.8.2 "$@"; }
if cli releases new --project "$project" "$release" \
  && SENTRY_VCS_REMOTE="$repo" cli releases set-commits --local --ignore-missing "$release" \
  && cli releases finalize "$release" \
  && cli deploys new --release "$release" --env production; then
  echo "Recorded $release in Sentry."
else
  echo "::warning::Recording $release in Sentry failed."
fi
