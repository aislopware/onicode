# Onicode

A native control plane for coding agents: many clients drive many servers peer to peer over a tailnet.

## Upstream

- `upstream` is zeronsh/zeron. Crate, module and `ZERON_*` names stay as upstream has them, so its commits
  cherry-pick cleanly; only user-visible text, the bundle id, the data dir and the IPC port are Onicode's.
- Changes from upstream and from the forks (gpui-fast, ghostty, libghostty-rs, noq) are adopted commit by commit
  after review, never by a blind merge.

## Architecture rules

- No relay, cloud service or app-layer crypto: peers speak plaintext QUIC over the tailnet and admit by tailscale whois.
- Remote desktop streams whole displays; per-window remoting is not built.

## Checks

- Build the app: `cargo build -p zeron --bins`.
- Subagents edit a whole batch without building or testing; the lead runs the checks once per batch.

## Tests

- Prove behaviour with seeded simulation, property, model or metamorphic tests and fuzzing over committed corpora;
  e2e stays a short smoke.
- A flaky test gets a deterministic repro, never a rerun.
- Tests that touch screen capture, audio output or windows are opt-in, and never prompt, play sound or take focus.

## Conventions

- No backward compatibility before release: replace and delete the old. Lints are never loosened.
- Agents are driven only through their published doors; never send requests in Claude Code's name or read its credentials.
- A decision worth a paragraph goes in `docs/onicode/decisions/` in the same change.
