# Explicit agent installation

Settings → Providers → **Install** runs on the selected device, as its user. Merely
listing or detecting agents never invokes these CLI installers. The table below
was checked with `curl -fsSL` against the linked vendor documentation on
2026-09-21. Commands download the vendor's current release, except for the existing
pinned Antigravity archive.

| Agent / documentation | Command (macOS and Linux) |
| --- | --- |
| [Claude Code](https://code.claude.com/docs/en/setup) | `curl -fsSL https://claude.ai/install.sh \| bash` |
| [Codex](https://github.com/openai/codex/blob/main/README.md) | `curl -fsSL https://chatgpt.com/codex/install.sh \| sh` |
| [Cursor](https://cursor.com/docs/cli/installation) | `curl https://cursor.com/install -fsS \| bash` |
| [OpenCode](https://opencode.ai/docs/) | `curl -fsSL https://opencode.ai/install \| bash` |
| [Pi](https://pi.dev/docs/latest) | `curl -fsSL https://pi.dev/install.sh \| sh` |
| [Grok](https://docs.x.ai/developers/release-notes) | `curl -fsSL https://x.ai/cli/install.sh \| bash` |
| [Hermes](https://hermes-agent.nousresearch.com/docs/getting-started/installation) | `curl -fsSL https://hermes-agent.nousresearch.com/install.sh \| bash` |
| [Devin](https://cli.devin.ai/) | macOS, when brew resolves: `brew install --cask devin-cli`; otherwise `curl -fsSL https://cli.devin.ai/install.sh \| bash` |
| [Muse Code](https://dev.meta.ai/docs/muse-code/) | `curl -fsSL https://dev.meta.ai/install.sh \| sh` |
| [fx](https://fx.sh/docs) | `curl -fsSL https://fx.sh/setup.sh \| bash` |
| Antigravity | Existing verified 1.1.1 archive in `acp::antigravity_archive`; no shell command |

Shell commands run through `sh -c`, with PATH augmented from the user's login
shell and npm toolchain.

If shell prerequisites are absent, these documented npm alternatives are used
when npm resolves: `npm install -g @openai/codex`,
`npm install -g @opencode/cli`,
`npm install -g --ignore-scripts @earendil-works/pi-coding-agent`, and
`npm install -g @xai-official/grok`. Grok's npm route is documented in its
[enterprise guide](https://docs.x.ai/build/enterprise). No action is offered when
none of the documented methods' prerequisites resolves, or for Mock. The row then
keeps a manual hint.

Install processes receive `CI=1`, `NONINTERACTIVE=1`, `TERM=dumb`, no stdin/TTY,
and no inherited `ONICODE_*` or nested Claude environment markers. Output is bounded
and credentials are redacted before errors reach Settings. Cancel, timeout
(15 minutes), or dropping the request kills the owned process tree. There is no
automatic elevation: an installer that needs sudo or an interactive prompt may
fail. Cancelling does not undo files an installer already wrote.

After installation, executable version caches for the agent are invalidated and
the normal installed probe runs again. A missing binary produces PATH guidance;
the running app's login-shell snapshot is not refreshed. Authentication remains a
separate step. Hermes may additionally require its documented ACP Python extra
for agent sessions; the Install action checks CLI detection only.

## Muse Code

`muse::MuseHarness` drives `muse serve` over the Muse Session Protocol (MSP):
newline-delimited JSON-RPC 2.0 on stdio, the wire Muse's own TypeScript SDK
(`@muse-code/sdk`) speaks. Every state-changing method is a command carrying a
client-minted UUIDv7 `commandId`; a fresh turn's id is its `turn/start` command
id. A runtime is one host with one session (`session/start`, or
`session/resume` by id); turns end on `turn/completed`, including the wake turns
Muse starts when background subagents finish. Later prompts are
`turn/start` with `ifBusy: "steer"`, stops are `turn/interrupt`, approvals are
answered from the host's own choices, and questions reach the user through
`request_input`. Models (`model/list`) and titles run on a read-only host
(`--disable-shell --disable-write --no-session-log`). The host's environment
drops an inherited `META_API_KEY` and sets `MUSE_NO_AUTO_UPDATE=1`.

`tests/muse.rs` replays sessions recorded from a real Muse Code 1.4.x host
(`tests/fixtures/muse/*.ndjson`, through `fake-muse.py`).

## Verification

The `installer-fixture` feature exposes an explicit command injection seam for
integration tests. The engine's command environment override is compiled only
for unit tests, never production. Tests exercise successful detection, failure,
missing binaries, cancellation, duplicate requests, process descendants, bounded
redaction, platform prerequisites, and version-cache invalidation.

The opt-in engine test `rpc::tests::installer_rpc_npm` runs the documented Codex
npm alternative in a disposable prefix and isolated home, through `InstallHarness`.
It asserts that the returned Settings descriptor changes from uninstalled/disabled
to installed/enabled. It never runs a vendor shell installer. Set `TMPDIR` to a
scratch directory before running it with `--ignored --nocapture`.

The iOS app adds no Install action. `HarnessDescriptor.can_install` already has
`#[serde(default)]` (`crates/engine/src/registry.rs`); iOS's `WireHarness: Decodable`
in `apps/ios/Onicode/Sync/WorkspaceStore.swift` ignores unrecognized keys, including
`canInstall`. `HarnessCatalog` continues consuming the existing mapped fields.

## fx

fx uses its native `fx acp` server. Install on macOS/Linux with
`curl -fsSL https://fx.sh/setup.sh | bash`, then run `fx login` or `fx setup`
in a terminal to configure the model provider. Onicode detects PATH and
`~/.local/bin/fx`; `FX_EXECUTABLE` overrides the executable.

Models and reasoning levels come from ACP. The provider selector is excluded
from model discovery. Explicit model selections are sent to fx and errors
propagate instead of silently running a different model. Skills use the existing
host discovery for `.agents/skills`, `.claude/skills`, and `.fx/skills`.

Onicode writes fx's `ask` policy before every prompt, including resumed sessions:
some fx releases report ask while retaining a saved automatic policy. Onicode then
decides permissions: Supervised presents requests to the user; Full access
approves them. Streaming, tool calls, cancellation and session loading use the
shared ACP driver. Login and updates remain CLI-managed.
