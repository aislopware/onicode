#!/usr/bin/env bash
# The macOS lane: the app build and the macOS-native tests. CI runs it on a GitLab office Mac
# (scripts/ci/gitlab-bridge.py, a warm target on /Volumes/CICache) or, when neither office Mac
# is free, on a hosted macos runner (cold, advisory).
#
#   mac.sh pr        the app binaries and the macOS-native tests
#   mac.sh nightly   the same, then the whole workspace under the default nextest profile
set -euo pipefail
cd "$(dirname "$0")/../.."
suite="${1:-pr}"

NEXTEST_VERSION=0.9.146
if ! cargo nextest --version 2>/dev/null | grep -q "$NEXTEST_VERSION"; then
  bin="${CARGO_HOME:-$HOME/.cargo}/bin"
  mkdir -p "$bin"
  curl --proto '=https' --tlsv1.2 -fsSL --retry 5 \
    "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$NEXTEST_VERSION/cargo-nextest-$NEXTEST_VERSION-universal-apple-darwin.tar.gz" |
    tar -xzf - -C "$bin" cargo-nextest
fi

export CARGO_PROFILE_DEV_DEBUG=0
nextest=(cargo nextest run --config-file scripts/ci/nextest.toml --locked --no-fail-fast)

cargo build --locked -p onicode --bins

# onicode-text's coretext tests are macOS-only; the ui filters cover native font geometry,
# notifications and attachment staging, whose paths and file metadata are platform-specific.
"${nextest[@]}" -p onicode-update -p onicode-preview -p onicode-ui -p onicode-text -p onicode \
  --lib --bins --tests \
  -E 'package(onicode-update) | package(onicode-preview) | binary_id(onicode-text::coretext) | binary_id(onicode::cli)
      | (binary_id(onicode-ui) & (test(native_diff_font_geometry) | test(/notify::tests::/) | test(/^attachments::tests::/)
         | test(/composer::attachment_chip_tests::/) | test(/shell::chat_dropzone_tests::/)
         | test(=queue::tests::queue_rows_label_attachment_chips_and_file_only_sends)
         | test(=transcript::tests::user_bubbles_project_attachment_chips_in_place_of_the_strip)
         | test(=transcript::tests::sent_chips_show_upload_progress_until_their_bytes_land)))'

if [ "$suite" = nightly ]; then
  NEXTEST_PROFILE=default "${nextest[@]}" --workspace --lib --bins
fi
