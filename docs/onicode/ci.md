# CI

`.github/workflows/ci.yml` runs on pull requests, main and nightly at 02:23 ICT. `CI result` is the one required check;
it needs `static` and `mac`. Onicode is built for macOS and iOS alone, tested on GitHub's hosted Macs; `static` runs on
Linux only because its checkers build nothing. The iOS lane comes back with the iOS client, last in the plan.

| Job | Where | What |
|---|---|---|
| `static` | ubuntu-24.04 | `scripts/dev/check.sh static`, `fmt` and `deps`: every checker that builds nothing |
| `mac` | xcode-27 | `scripts/ci/mac.sh`: clippy and rustdoc for macOS, the app build, every test, diff-scoped mutants |

Pull requests and main use the nextest `ci` profile (`scripts/ci/nextest.toml`), which leaves out the slow and soak
tests. Nightly runs the default profile, then `scripts/ci/gates.sh` on the Mac.

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

## Hosted Macs

`mac` runs on GitHub's `xcode-27` image: macOS 27 on 3 M1 cores, 7 GB of RAM and 14 GB of disk. Standard
runners are free and unlimited for a public repository, but an organization on the Free plan runs at most 5 macOS jobs
at once, across all its repositories (libghostty-rs's prebuilts included); the rest queue. `.github/actions/mac-ci`
selects Xcode 27.2 (the image defaults to 27.1), deletes the other Xcodes for disk room, installs the Metal toolchain
and the Rust toolchain, and restores the cache. Fork pull requests run with no secrets.

## Caches

`Swatinem/rust-cache` for `mac`. Its caches are keyed by the image version, because build
scripts record absolute paths into Xcode (ort-sys links its clang_rt), and carry onnxruntime's download cache. Only
main saves, so pull requests never evict main's cache under the 10 GB limit.
