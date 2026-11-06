# Onicode

A native control plane for coding agents: many clients drive many servers peer to peer over a tailnet.

## Upstream

- `upstream` is zeronsh/zeron. Onicode owns its code outright: rename, restructure or delete anything that
  makes it better.
- Changes from upstream and from the forks (gpui-fast, ghostty, libghostty-rs, noq) are ported by intent: an agent
  reads each change, judges it, and re-implements what helps. Never a git merge or rebase.
- `scripts/upstream/watch.py` reports what moved; check it at least every 30 minutes and adopt per
  `docs/onicode/upstream.md`.

## Architecture rules

- No relay, cloud service or app-layer crypto: peers speak plaintext QUIC over the tailnet and admit by tailscale whois.
- Remote desktop streams whole displays from a Mac host to Mac and iOS clients; per-window remoting and other
  platforms are not built.

## Checks

- `scripts/dev/check.sh` runs fmt, clippy and the tests; `build`, `lint` or `test '<nextest filter>'` run one part.
  It keeps one warm target dir on the internal SSD (`scripts/dev/env.sh`); select tests with a filter, not `-p`.
- Subagents edit a whole batch without building or testing; the lead runs the checks once per batch.

## Tests

- Prove behaviour with seeded simulation, property, model or metamorphic tests and fuzzing over committed corpora;
  e2e stays a short smoke.
- A flaky test gets a deterministic repro, never a rerun.
- Tests that touch screen capture, audio output or windows are opt-in, and never prompt, play sound or take focus.

## Conventions

- No backward compatibility before release: replace and delete the old. Lints are never loosened.
- Toolchains, libraries and tools are the newest that measurably pay off: nightly, alpha, beta, RC or a pinned git
  commit are fine, pinned exactly, with the reason in the commit.
- Agents are driven only through their published doors; never send requests in Claude Code's name or read its credentials.
- A decision worth a paragraph goes in `docs/onicode/decisions/` in the same change.
