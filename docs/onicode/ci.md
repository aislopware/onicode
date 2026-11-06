# CI

`.github/workflows/ci.yml` runs on pull requests, main and nightly at 02:23 ICT. `CI result` is the one required check;
it needs `static`, `lint`, `mac` and `ios`. macOS and iOS are tested thoroughly on the office Macs. Linux gets only the
static and clippy gates, and nothing else is a target.

| Job | Where | What |
|---|---|---|
| `static` | ubuntu-24.04 | `scripts/dev/check.sh static`, `fmt` and `deps`: every checker that builds nothing |
| `lint` | ubuntu-24.04 | `cargo lint` for Linux, so cfg(linux) code still compiles; rustdoc, warnings denied |
| `mac` | GitLab office Mac | `scripts/ci/mac.sh`: clippy for macOS, the app build, every test, diff-scoped mutants |
| `ios` | GitLab office Mac | `scripts/ci/ios.sh`: the mobile core for the simulator, binding drift, `OnicodeTests` |

Pull requests and main use the nextest `ci` profile (`scripts/ci/nextest.toml`), which leaves out the slow and soak
tests. Nightly runs the default profile, then `scripts/ci/gates.sh` on the Mac, and one UI smoke test on the simulator.

## Static checks

`scripts/dev/check.sh static` runs these at once, over the whole tree; their configs are at the repository root.

| Checker | Config | Covers |
|---|---|---|
| typos | `_typos.toml` | every text file; a deliberate fragment carries `spellchecker:disable-line` |
| taplo | `taplo.toml` | TOML formatting, Cargo manifests included |
| shellcheck, shfmt | `.shellcheckrc`, `.editorconfig` | shell scripts, every optional check but SC2250 |
| ruff | `ruff.toml` | Python: every rule, formatted |
| actionlint, zizmor | | workflows and the composite action; zizmor as `auditor` |
| swiftlint, swift format | `.swiftlint.yml`, `.swift-format` | Swift; today's oversized declarations are held in `apps/ios/.swiftlint-baseline.json`, and swift format runs on macOS only |
| coverage baseline | `scripts/ci/coverage-baseline.txt` | the baseline never goes down |

`check.sh fmt` is nightly rustfmt (`rustfmt.toml` uses unstable options); `check.sh deps` is cargo-deny (`deny.toml`:
advisories, licenses, duplicate versions, sources) and cargo-shear. Neither builds anything, but both need a settled
`Cargo.lock`. Every tool's version is pinned in `scripts/dev/tools.env`, which the static job and
`scripts/dev/setup.sh` both read.

## Hooks

`scripts/dev/setup.sh` installs the pinned tools and lefthook's hooks (`lefthook.yml`). pre-commit runs the static
checkers and rustfmt on the staged files only, in parallel; pre-push runs `check.sh lint`.

## Test gates

- nextest: no retries in any profile, a leaked process or handle fails the test, a hung test is killed, and a run
  that selects no tests fails. A flaky test gets a deterministic repro.
- Coverage: `gates.sh coverage`, cargo-llvm-cov over `cargo t`'s tests on macOS, the number that counts. It fails
  below `scripts/ci/coverage-baseline.txt`; raise the baseline when a run measures more.
- Mutants: `gates.sh mutants-diff` on pull requests, on the lines the branch changed; nightly runs one of thirty
  shards of the whole workspace (`.cargo/mutants.toml`).
- Miri: `onicode-visual` and `onicode-visual-gpu`, whose unsafe code is plain Rust. Every other crate's unsafe is a
  foreign call, which Miri cannot run.
- cargo-hack: every crate checks with each of its features alone.
- Fuzz: each cargo-fuzz target under `crates/*/fuzz` runs a minute from its committed corpus `fuzz/corpus/<target>`.
  There are none yet.

## Office Macs

The two office Mac runners on gitlab.jmango360.com (`office-mac-runner-02`, then `office-mac-runner`) run tart guests
with a persistent `/Volumes/CICache`. The GitLab project `cong.tran/onicode-ci` holds only
`scripts/ci/office-mac.gitlab-ci.yml`, as its `.gitlab-ci.yml`. The `mac` and `ios` jobs call
`scripts/ci/gitlab-bridge.py` with SUITE `pr`/`nightly` and `ios-pr`/`ios-nightly`, so each is its own pipeline and
the two can run on both Macs at once. The bridge triggers that pipeline for the commit and mirrors the result. If the
first Mac does not start within 2 minutes, it moves to the second, then waits up to an hour on the first. There is no
hosted fallback: onicode needs macOS 27.2, which hosted runners do not have. A run without the secrets (a fork pull
request) skips both jobs with a notice.

The guests run `zot.jmango360.dev/onicode/macos-27-ci`, pinned by digest, from Zot on office-1 (the office LAN's
registry; the runners read it as `onicode-runners`, from the masked project variables `ONICODE_ZOT_USER` and
`ONICODE_ZOT_SECRET`). `scripts/ci/image/build.sh` makes it from `ghcr.io/cirruslabs/macos-golden-gate-xcode:27.2`
and pushes it: macOS 27.2 and Xcode 27.2 with the Metal toolchain, the iOS 27.2 simulator runtime and a settled,
shut-down `iPhone CI` device, zig, cargo-nextest, cargo-sweep, rustup without a toolchain, Spotlight and software
updates off. It drops the base's other simulator runtimes, Flutter and the Android SDK, 49 GB compressed against the
base's 77. Rebuild it when one of those pins changes, then pin the printed digest and copy the pipeline file to the
project. A Mac's first pull is about an hour; a job triggered without a SHA only pulls it and prints its versions.

The source, toolchain (with the iOS targets), registry and target sit at the same path on CICache every run, and
`mac.sh` builds everything with `cargo t`'s workspace and feature set, so builds stay incremental and the suites
share one set of artifacts. CICache is shared with other projects, so onicode's target is held under 60 GB without
wiping it: `cargo sweep` drops what an older toolchain built, then the least recently used units down to 45 GB.
Fork pull requests get no secrets, so untrusted code never reaches CICache.

Secrets, in the `office-mac` environment of aislopware/onicode:
- `GITLAB_TRIGGER_TOKEN`: the project's pipeline trigger token.
- `GITLAB_READ_TOKEN`: a Developer project access token with `api`, which reads jobs and cancels the pipeline
  that lost the race.

## Caches

`Swatinem/rust-cache` for `lint`; the office Macs keep theirs on CICache. Only main saves, so pull requests never evict
main's cache under the 10 GB limit.
