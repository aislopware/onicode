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
