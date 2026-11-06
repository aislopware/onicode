#!/usr/bin/env bash
# The Linux Rust tests that used to run as separate `cargo test` invocations in
# session-sync-regressions, cursor-compatibility and preview-tests, as ONE
# build and ONE nextest run (separate invocations recompiled the same
# dependency graph with different feature sets).
#
# Same coverage as the old invocations:
#   onicode-harness  every test binary (cursor-compatibility ran `-p onicode-harness`
#                  plus `--test pi_rpc` with native-fixture; native-fixture only
#                  adds the pi_rpc fixture binaries, so it is on for the whole build)
#   onicode-preview  every test binary (preview-tests)
#   onicode-doc      lib + persistence and memory regressions + attachments_roundtrip
#   onicode-sync, onicode-update   lib only (session-sync-regressions)
#   onicode-cli, onicode-launch, onicode-notify   `onicode`'s command line, the engine
#                  helper's start-up and the onicode:// links, shared by the apps
#   onicode-engine   lib + the integration tests named below. Engine has many other
#                  integration binaries (live agents etc.) that no workflow ran, so
#                  they are listed instead of globbed to avoid compiling them.
#   voice (moved from voice-tests.yml offline-linux, which ran them as 14 separate
#   `cargo test` builds): onicode-voice-media, onicode-voice-session and onicode-orb
#                  lib, onicode-proto lib `voice` tests, onicode-mcp lib, onicode-rpc
#                  device_room, engine voice_runtime and device_routing. They need no
#                  audio stack, and harness already depends on all of them.
# nextest does not run doctests; these crates have none (their doc comments
# contain no Rust code blocks).
# Extra arguments are passed to nextest (the nightly job adds `--release`).
# NEXTEST_PROFILE=ci (pull requests and main) leaves out the slow and soak tests
# that scripts/ci/nextest.toml lists; the nightly runs use the default profile.
set -euo pipefail
cd "$(dirname "$0")/../.."

# Test binaries that run nothing on Linux are left out: they would still be
# compiled and linked. These are files whose every test is #[ignore]d because
# it needs a live service or a real CLI.
skip=" managed_install muse_account_live pi_live real_acp_lifecycle real_quiet_survey coordinator "
tests=()
for f in crates/harness/tests/*.rs crates/preview/tests/*.rs crates/doc/tests/*.rs; do
  name="$(basename "$f" .rs)"
  [[ "$skip" == *" $name "* ]] || tests+=(--test "$name")
done
# `misc` (harness, engine and preview): the small suites, one binary each.
# The orb geometry tests live in onicode-orb's `geometry`.
for t in misc geometry session_publication restart_resume side_chats message_queue voice_runtime device_routing device_room; do
  tests+=(--test "$t")
done

exec cargo nextest run --config-file scripts/ci/nextest.toml --locked --no-fail-fast \
  -p onicode-harness -p onicode-engine -p onicode-sync -p onicode-update -p onicode-doc -p onicode-preview \
  -p onicode-mcp -p onicode-proto -p onicode-rpc -p onicode-orb -p onicode-voice-session -p onicode-voice-media \
  -p onicode-cli -p onicode-launch -p onicode-notify \
  --features onicode-harness/native-fixture \
  --lib --bins "${tests[@]}" \
  -E 'not ((package(onicode-proto) & not test(voice)) | (package(onicode-rpc) & kind(lib)))' "$@"
