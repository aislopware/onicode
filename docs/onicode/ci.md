# CI

`.github/workflows/ci.yml` runs on pull requests, main and nightly at 02:23 ICT. `CI result` is the one required check.

| Job | Where | What |
|---|---|---|
| `lint` | ubuntu-24.04 | `cargo fmt --check`, `cargo lint` (clippy on every target, warnings denied) |
| `core` | ubuntu-24.04 | `scripts/ci/test-core.sh`: every non-UI Rust test, one build, one nextest run |
| `ui` | ubuntu-24.04 | `onicode-ui` library tests, headless layout |
| `ios-core` | ubuntu-24.04 | mobile core tests and the generated Swift bindings, when the mobile core changes |
| `mac` | GitLab office Mac | `scripts/ci/mac.sh`: the app build and the macOS-native tests |
| `mac-hosted` | macos-latest | the same, cold, when no office Mac is free or the run has no secrets |

Pull requests and main use the nextest `ci` profile (`scripts/ci/nextest.toml`), which leaves out the slow and soak
tests; nightly runs the default profile, the release profile and the whole workspace on the office Mac.

## Office Macs

The two office Mac runners on gitlab.jmango360.com (`office-mac-runner-02`, then `office-mac-runner`) run tart guests
with a persistent `/Volumes/CICache`. The GitLab project `cong.tran/onicode-ci` holds only
`scripts/ci/office-mac.gitlab-ci.yml`, as its `.gitlab-ci.yml`. The `mac` job calls `scripts/ci/gitlab-bridge.py`.
The bridge triggers that pipeline for the commit and mirrors the result. If the first Mac does not start within
2 minutes, it moves to the second, and then falls back to hosted.

The source, toolchain and target sit at the same path on CICache every run, so builds stay incremental. The
onicode target is capped at 60 GB, since CICache is shared with other projects. Fork pull requests get no secrets,
so untrusted code never reaches CICache.

Secrets on aislopware/onicode:
- `GITLAB_TRIGGER_TOKEN`: the project's pipeline trigger token.
- `GITLAB_READ_TOKEN`: a project access token with `read_api`.

## Caches

`Swatinem/rust-cache`, one key per job (`lint`, `core`, `ui`, `ios-core`, plus `ci-macos` for the fallback). Only
main saves, so pull requests never evict main's caches under the 10 GB limit.
