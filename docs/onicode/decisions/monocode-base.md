# Base: monocode native instead of zeron

Onicode's app and core are now monocode's native preview (hardbeat920/monocode at 1de99e7b, MIT):
`crates/app` is its GPUI app (`monocode-gpui`) and `crates/core` its core (`monocode-core`); its patched GPUI
snapshot was replaced by aislopware/gpui-fast (`rendering.md`). zeron's engine, harness, UI, sync, proto and client
are deleted. The owner judges monocode's interface the best of its kind. It is kept exactly: layout, motion,
colours and every icon. Everything under it is Onicode's to optimise. UI and UX changes come from monocode upstream
only (`scripts/upstream/sources.toml`).

The study behind this is in `.research/monocode/` (ui.md, core.md, carry-over.md; not committed).

## What stays Onicode's
The tailnet transport (net, net-sim, tailnet, peer, rpc, the vendored noq), remote desktop (the ten desktop crates),
grants, update and launch came over unchanged. Transfer, clipboard, the libghostty terminal, alerts and
keep-awake, and the iOS app return by intent from main's history, on the new base.

## Shape
monocode runs turns inside the app, and its remote machines run a Node host over SSH. Onicode needs one headless
engine per machine that outlives the window and serves every client over the tailnet. So `crates/core` becomes that
engine's library: turns, store and providers move out of the app process into an `onicode-engine`, and the app,
other Macs and the iPhone all talk to engines on one protocol over `peer`.

The engine owns its sessions and every viewer watches: a viewer gets a snapshot, then the event stream from that
point, and mints ids for what it sends, so its own echo shows at once. zeron's Loro replicas, outbox and mailbox are
not carried over. A control plane has one authority per session, and the host is always it.

## Features
Kept: everything the shell shows. That covers the sidebar, live agents card and peek; tabs, decks and splits; the
transcript with its reveal pacing, phases, tasks and Mermaid; the composer with its pickers, queue/steer and
BTW; approvals and questions; the terminal dock; the explorer, editor, search, changes, diff and review panes; quick
open and the quick composer; notes and automations; the GitHub inbox; accounts; settings and keybindings;
the decorations (mascots, ink), which are part of the feel.

Dropped:
- **Monos.** A 17k-line persona-agent suite (habits, heartbeat, soul, menu-bar chat) whose runtime is unverified
  even upstream. It is a product of its own, not part of driving agents. What only Monos read goes with it:
  the contributions recorder, the paged history of indexed Mono chats and the app MCP server. The menu-bar
  status item stays, holding the quick composer and Quit, and closing the window hides to it.
- **The phone link and Expo app.** Its own QUIC with mTLS, router port mapping and Expo's push cloud break Onicode's
  rules; Onicode's native iOS app replaces them.
- **SSH remote and the Node host.** Replaced by engines on the tailnet.
- **Credential reads.** Usage meters for Claude, Pi, Devin and OpenCode read those tools' credentials or call
  private endpoints; Codex's meter uses its app-server and stays. The "Identify as Monocode to Claude" relabelling
  and the deletion of Claude's Keychain item go too.
- **The Tauri data readers.** The app keeps its own data in `~/Library/Application Support/Onicode`; the Tauri
  database import, the web-view preference reads and the old `<db>.gpui.json` settings import are gone.
- **Updater and star prompt.** monocode's Minisign feed is replaced by Onicode's updater, and the "star us" prompt
  goes.
- **Windows and Linux paths.** Out of Onicode's platforms (`platform.md`).

## Look on Onicode's stack
monocode's look rests on its patched GPUI. Onicode keeps the look and replaces the stack: the app moves to gpui-fast
and gpui-kit, and every effect monocode's patches give (backdrop blur, edge fades, vibrancy, the shared pulse, the
motion curves) is rebuilt there to the same constants, studied from monocode's code rather than approximated. Where
gpui-fast or gpui-kit lacks something the layout needs, it is built into the fork.

A dropped feature must not leave a hole in the layout. Each place it filled (a settings page, a footer meter, a
sidebar entry, an onboarding step) gets Onicode's equivalent: tailnet machines for SSH remote, the iPhone app's
pairing for the phone link, Onicode's updater for monocode's, and usage from the agents' own published streams for
the dropped meters. A gap is left empty only when filling it would be complex and useless.

## Order
1. Import and prune: the import landed first; the dropped features go next.
2. gpui-fast under the app, its renderer patches (blur, fades, alpha, display-link recovery, paint-only notify)
   reproduced there, then a retained-mode audit.
3. The headless engine and its protocol over `peer`, then the app on it.
4. The libghostty terminal in the dock and panes.
5. Transfer, clipboard and explorer across machines.
6. Alerts and keep-awake, remote desktop and onboarding grants.
7. The iOS app on the engine protocol.

## Error-prone and extremely complex features go
Ruled 2026-11-07 by the owner: a feature that is error-prone and extremely complex is deleted or simplified, with
nothing left dormant.
- **The legacy store layout and monocode-data import.** The store had two schemas behind every method (the
  prototype `native_*` tables and monocode's sessions layout) while production used only the second; it now has
  that one layout, opened by `Store::open` (or `Store::in_memory`), and reads no monocode desktop preferences.
- **Inbox trackers other than GitHub.** Linear, Jira, GitLab and Azure DevOps went with their adapters, token file
  formats, connection UI and automation triggers; the Inbox reads GitHub through `gh`, and the media fetcher stays
  for GitHub's attachments.
- **The regex commit card.** The transcript's commit card was built by regex over the agent's shell command and
  git's locale-dependent output; the engine now reads git itself (`LC_ALL=C`) around each turn.
- **Usage limits and attachments guessed from text.** A limit is the one the provider reports (Claude's rate-limit
  info or error code, Codex's `usageLimitExceeded`), never limit wording in a transcript or status, and editing the
  last turn trusts its saved attachments rather than guessing them from an old Markdown link format.
- **Session visuals and `/visualize`.** Agent-written JavaScript ran in QuickJS behind a hand-written, token-guarded
  loopback TCP protocol and an MCP stdio server inside the app and engine binaries, injected into every provider's
  config; charts in answers are Markdown and Mermaid now.
- **The arcade games and the composer runner mascot.** JavaScript physics stepped every frame in QuickJS, and the
  app panicked if its interpreter failed to start; the empty session keeps the ink canvas, and projects keep their
  mascots.
- **The editor's QuickJS analysis path.** A second analysis beside Tree-sitter re-evaluated a 1.2 MB CodeMirror
  bundle per keystroke for completions and grammar-less files, and a 2.1 MB Prettier bundle for Format and format on
  save. Tree-sitter is the only syntax (a file without a grammar is plain text with its git gutter); Find and
  Replace run on the `regex` crate and staging or reverting a hunk is ported to Rust; completions, Emmet and
  formatting are gone. With these three, `rquickjs` leaves the build.
- **Orchestrator mode.** A JSON plan parsed out of the model's free text drove a dispatcher that started workers by
  dependency and file scope on one checkout, behind a sidebar tree, hidden worker sessions and a coordinator that
  wrote back into the lead's chat; upstream monocode #1334 removes it. `/orchestrator`, the plan card, the
  orchestration summaries and `SaveOrchestration` are gone.
- **Operator mode.** A bridge socket and per-thread grants let an agent drive Onicode through an `onicode app` CLI,
  with receipts, hidden launched chats and a codex network exception; upstream monocode #1334 replaces it.
  `/operator`, the App access group and the `app` subcommand are gone.
- **The resource pressure chip.** It guessed CPU pressure from thresholds and timer lateness, parsed `ps`, and
  killed agent tools by pid; the footer chip, its dialog and `AgentProcesses`/`StopAgentTool` are gone, and a
  session's Stop button stops its turn.
