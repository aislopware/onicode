# Following upstream

Onicode builds on zeron, gpui-fast and gpui-kit (and through them zed's gpui), ghostty, libghostty-rs, noq,
the Rust toolchain and the crates in `Cargo.lock`. Each moves fast, and each move can carry a fix, a speed-up or
a feature onicode wants, or work onicode would otherwise duplicate. Onicode follows them continuously and never
blindly.

## Watching

`scripts/upstream/sources.toml` names every source, the upstream commit onicode stands on (`base`) and the
paths that matter. `scripts/upstream/watch.py`:

- `round`: one line per head move or pull request that touches those paths since the last round;
- `loop`: rounds every 5 minutes, for a Monitor in the working session;
- `status [name]`: what lies between `base` and the upstream head, commit by commit and file by file;
- `deps`: `rustup check` and `cargo upgrade --dry-run --incompatible`.

The working session checks at least every 30 minutes, and runs `deps` once a day.

## Adopting

A version bump or a fast-forward on its own is not adoption. For each change:

1. Read it: the diff, the pull request discussion, the release notes or changelog between the two versions.
2. Judge it against onicode. Does it fix something onicode hits, make a hot path faster, add an API onicode
   wants, or make an onicode patch, shim or workaround redundant? Does it fight an architecture rule?
3. Take it the best way, not necessarily upstream's way:
   - port the intent where onicode's code has diverged;
   - delete what it makes redundant;
   - improve it at the root, in our fork, when it falls short;
   - measure it when it claims to be faster.
4. Rule out what does not help, with a reason, in the commit message.
5. Move `base` in `sources.toml` in the same commit, and pins in `Cargo.toml`/`Cargo.lock`.

Open upstream pull requests are adopted the same way when they help now: merge them into our fork and finish
them, then reconcile when upstream lands them. Before building anything in GPUI or the terminal, check the open
upstream pull requests so the work is not done twice.

For the toolchain and crates, take the newest that pays off, including nightly, prerelease and breaking majors.
Fix the call sites and adopt what the new version offers. A dependency that blocks a bump or falls short is
forked under aislopware, fixed there and added to `sources.toml`.

## Studying references

`sources.toml` also lists references: Claude Code, its Agent SDKs, Claude's desktop apps, t3code, Amp, Zed's ACP
adapter, opencode and Codex. They are studied rather than built on. Each change is read for what it teaches about
driving agents, syncing sessions, recovering from failures and presenting work. Where it is better than what onicode
does, port the idea. A reference's `base` is the commit last studied.

Closed or bundled code is studied from what its vendor publishes, with REA (github.com/morluto/rea; the `rea`
MCP tools):

- The npm bundles: `@anthropic-ai/claude-code`, `@anthropic-ai/claude-agent-sdk`, `@sourcegraph/amp`.
- The Claude desktop app: its published DMG.

Fetch each version into its own empty directory under `.research/re/<name>/<version>/`. That directory is
gitignored and untrusted: never run anything from it. Use `analyze_javascript_application`,
`recover_javascript_sources` and `compare_application_versions` against the previous version.

Write what changed and what onicode should take into `.research/re/<name>/NOTES.md`. Ideas are ported; recovered
code is never committed. Nothing reads another tool's credentials or sends requests in its name.
