# zeron — Architecture

A ground-up native rewrite of [zeron](../zeron) — a multi-device controller for coding agents
(Claude Code / Codex) — in Rust, with a gpui UI. Fresh app; no backwards compatibility required.

**Pillars (from the goal):**
- Session docs and the workspace registry are Loro CRDT docs (loro-mirror model), persisted on each
  device and synced replica to replica over the tailnet (`docs/onicode/decisions/replica-sync.md`).
  No relay, no cloud service, no sign-in: a device is who the tailnet says it is
  (`docs/onicode/decisions/topology.md`, `docs/onicode/decisions/no-edge.md`).
- Feature parity with zeron **except token-usage display** (poor fit for CRDTs; excluded).
- Frontend is **gpui** (pinned Zed rev). Virtualization + markdown techniques ported from
  **mugen + pretext** (`docs/research/mugen-pretext.md`).
- One binary, **headed or headless**. Smooth transitions/animations matching the original
  (catalog in `docs/research/feature-inventory.md` §1.12).

## 1. Topology (unchanged shape, new materials)

```
gpui UI ─ in-proc/localhost RPC ─ engine A ══ plaintext QUIC over the tailnet ══ engine B ─ RPC ─ gpui UI
                                              ║  (RPC, doc/{chatId}, registry,
                        iOS thin client ══════╝   terminal/{id}, tunnels)
```

- **Engine = backend** (was `@zeron/backend`): runs agents, terminals, repos/worktrees,
  diff sync, doc hosting. Pure Rust daemon, fully functional headless.
- **UI = viewport** (was Electron): gpui app rendering engine state. Talks the same typed RPC whether the engine is in-process or a separate daemon. Organized around **spaces** — (device, folder) pairs on any of the user's engines. The sidebar is the data: an attention-sorted Sessions list, filtered by a searchable spaces dropdown ("All spaces" included) that also hosts space management. The horizontal tabs are a **device-local viewport** onto that list (`ui-settings.json` `openTabs`, cross-space): closing a tab is local-only — archiving is an explicit sidebar action — and a sidebar click (re)opens a session as a tab. The new-session canvas carries a space picker (defaulting to the sidebar filter, else the last selected space); new sessions are minted onto the picked space's device via RPCs forwarded over the peer link.
- **Thin clients** (`crates/client`, the iOS app through `crates/mobile`): no engine of their own.
  They hold their own peer links: on macOS through the Tailscale LocalAPI like an engine, on iOS to
  a list of engines the person typed (MagicDNS names or tailnet addresses), each admitted by the
  engine's whois of the phone's address.

### Headed / headless
Binaries `zeron` (app + CLI) and `zeron-engine` (the app's background engine, no UI linked):
- `zeron` — headed. If an engine is already listening on the IPC port, connect to it. Otherwise
  start the **background engine** (`crates/engine/src/managed.rs`) — `zeron-engine` beside the app
  ("Zeron Engine" inside the macOS bundle), or `zeron engine` where no separate executable ships —
  and attach to it. It is spawned detached from
  the app (own session/process group, no inherited pipes), not through a service manager, so on
  macOS privacy permissions stay attributed to the app and no background-item approval is needed.
  Closing or quitting the app leaves it running with every session; it shows up as its own process.
  The app reconnects when the connection drops (starting it again if it is gone) without resetting
  what's on screen, and stops it itself only to replace the runtime. After an app update it restarts into the new version once no run or terminal is live.
  If it can't start — or the port is 0, or something else holds the port — the app embeds the
  engine **in-process** as before (RPC over an in-memory duplex — same protocol — and serving that
  engine on the IPC port); `ZERON_EMBED_ENGINE=1` forces it. An engine that went away unasked is
  started again after 1, 2, 5 and 15 s; a fifth loss within five minutes stops and shows the failure
  (with Retry) rather than embedding — an engine bug would crash an in-process engine too.
  The engine stops by itself once idle if its app is moved to the Trash or uninstalled, and its log
  (like every long-running mode's) moves to `.old` past 64 MB rather than growing for weeks.
- **Updates from another device are transactions** (`crates/update/src/transaction.rs`, `crates/engine/src/update_trial.rs`).
  The installed version changes in one step, only once a new version has proved it's ready: `app/current` on Linux, an atomic
  bundle swap on macOS. Until then every start runs the old version, which is how a new binary that exits,
  hangs or loses power gets undone. The old version copies the SQLite stores, leaves a watchdog and hands over; the new one holds
  back what it can't take back (resuming chats, queued messages) until it has committed. A failed trial restores the stores, and
  `UpdateStatus.update` says which happened under the id `ApplyUpdate` answered with. Only the SQLite stores roll back. Provider
  CLIs update under running chats where their installer keeps old files (`UpdatePlan::while_chats_run`), and wait otherwise.
- Desktop banners are the engine's (`desktop_notify.rs`, advertised as `desktop-notifications-v1`):
  finished runs, questions, failures, connection loss and agent updates keep arriving with the app
  closed. Preferences come from the app's `ui-settings.json`; "only in the background" uses window
  focus the app reports (`SetViewportFocus`). The app keeps the chimes, decided by the same baselines.
- `zeron headless` — engine only. It serves its profile over localhost IPC and, joined to the tailnet, to the user's other devices. A VPS on the tailnet can run this while a laptop's UI drives it.

### One profile per device

An engine serves one profile: its device's data directory. Its identity is the tailnet device; there is
no account, organization or sign-in. Session snapshots, the registry snapshot, run journals and the
attachment cache live under `{data_dir}/profiles/local/`; `device-id`, repository registration,
managed worktrees, agent accounts and UI settings live beside it in `{data_dir}`.

##### Remote workspace file trust boundary

Devices the tailnet admits as the owner's (or grants an onicode role) are trusted peers for remote workspace control. A peer may send forwarded workspace file requests to the device that owns a checkout; the owning engine resolves the target and enforces workspace-relative path, containment, symlink, and write-conflict checks before touching its filesystem.

Ignored-file visibility is not an authorization boundary. A remote peer may request ignored entries and then read or write them, including potentially sensitive files such as `.env`, when `includeIgnored` is enabled. `.git` remains unavailable regardless of that option. Zeron intentionally does not maintain a filename denylist because it would be incomplete and could imply a security guarantee it cannot provide.

If admitted devices must no longer trust one another with the full workspace, that policy must be enforced by the owning engine for remote requests. Hiding entries only in the UI is not a security control.

## 2. Data model — all Loro

Two persistent doc kinds, each synced replica to replica over a peer stream (`doc/{chatId}` by Loro version vectors, `registry` by per-origin version vectors):

1. **Session doc** (per chat) — the transcript + durable command queue. Schema is a Rust port of
   `packages/session-doc` (same container names/shapes): `meta` map, `messages` list (parts as list-of-maps with **LoroText bodies** — the
   measured 1.03× oplog shape; never LWW value rewrites), `commands` list with ledger rules 1–3
   (append-only per-device entries; host-only outcomes; dedupe/TTL/supersede evaluation).
   Continuation splitting at 256KB, render-only tool parts (full inputs stay in the host's local
   run journal). Constants carried over (`STREAM_COMMIT_MS=120`,
   `DO_FLUSH_MS=5s`, compaction at 8MB, retain 30d, tail 64).

2. **Workspace registry doc** (per profile) — the `registry1` snapshot stores spaces (id, deviceId, path, name?, gitDetected, checkoutId, repositoryId), the chats index (id, deviceId, title, archived, cwd, branch, checkoutId, spaceId, lastSeenAt, lastMessagePreview/At, config), devices, session-status rows, and checkout-diff summary pointers. A space is a device+folder pair in the active profile; the owning device's `SpacesSync` stamps git presence so branch pickers and the diff sidebar can gate without another RPC, plus a repository identity (the trunk's root commit, so renames, transfers and remote spellings don't split a repository; a shallow or empty history falls back to the normalized origin remote, then a device-scoped common-git-dir hash) that groups clones and worktrees of one repository in the sidebar's project mode. Each engine keeps the registry in its profile store and replicates it with the user's other engines.

   Writer discipline: each device writes its own device and session-status rows, rows for chats it hosts, and git stamps for spaces it owns. Creates, renames, archives, and seen marks are LWW sets accepted from any device. `deleteSpace` tombstones the space and every chat/session row in it in one commit. Presence is the tailnet: a device is online while a live link to it exists or the directory finds it.

   *Why one registry and not N tiny docs:* the sidebar needs one subscription for the whole list (grouping, resort animations, unseen markers). Its rows contain indexes rather than transcripts, so one local snapshot and one `registry` stream per peer remain bounded and cheap.

3. **Mirror layer** (`zeron-doc` crate) — Rust equivalent of loro-mirror: typed structs for the
   schema, **incremental** application of `doc.subscribe` diffs into cached state (no full
   re-hydration per change — this is also what fixes zeron's known O(transcript) re-projection
   inefficiency, remaining-work item 1a), and a diff-reconcile write path (evaluate `lorosurgeon`
   0.2.x as a dep; our schema is small enough to hand-roll if it doesn't fit). The UI renders
   mirror state directly with per-entry change notifications — the "endgame" the TS
   implementation documented but never reached.

### Command plane
Send/steer/interrupt/respondInput = durable command entries in the session doc (`QueueCommand`),
executed by the chat's **host** device (executor gated on chat ownership; mark-processed BEFORE
execute; steer with no live run dispatches as the next turn). Offline sends queue in the doc.
This is zeron's proven design, kept verbatim.

## 3. Cargo workspace

```
zeron/
  Cargo.toml                 # workspace
  crates/
    proto/        zeron-proto    # wire types: AgentEvent, ToolCall, RunRequest, Model,
                                 # entities, RPC envelopes (serde; ndjson framing);
                                 # `view` = the pure derivations both frontends share
                                 # (sort orders, staleness gating, grouping, boot gate)
    doc/          zeron-doc      # session-doc + workspace-registry schemas, mirror layer,
                                 # parts fold, continuations, command ledger
    sync/         zeron-sync     # replica sync over peer streams (doc VV, registry VV),
                                 # DocsStore (SQLite snapshots + processed-command
                                 # ledger + outbox)
    harness/      zeron-harness  # Harness trait + claude-code (stream-json subprocess),
                                 # codex (app-server JSON-RPC), mock; steering mailbox,
                                 # requestInput, models/reasoning/options catalogs
    engine/       zeron-engine   # sessions engine (pub/sub, run journal, recovery, stall
                                 # watchdog), doc host + command executor, repos/worktrees,
                                 # checkout-diff sync, terminals (portable-pty), uploads,
                                 # agent accounts (cred swap), peers on the tailnet
    rpc/          zeron-rpc      # UiRpc/ControlRpc: typed req/resp/stream over WS (tokio-
                                 # tungstenite), in-memory, and one QUIC stream per call
    theme/        zeron-theme    # source-neutral theme schema + built-in/custom registry,
                                 # validation, provenance, and local VS Code compiler
    voice/        zeron-voice    # desktop-local optional Parakeet model, capture and inference; no RPC/sync
    ui/           zeron-ui       # gpui app: shell, sidebar, conversation, composer,
                                 # terminal view, diff pane, settings, animation kit
  apps/
    zeron/                       # the binary (headed default, `headless` subcommand)
  docs/                          # this file + research reports
```

Engine async runtime: **tokio** throughout; the UI bridges via `gpui_tokio` (`Tokio::spawn`
futures surfaced as gpui `Task`s). In-process mode runs the engine on its own tokio runtime
thread; the UI never blocks on it.

## 4. UI plan (gpui) — parity + smoothness

Reference: `docs/research/gpui.md`, `docs/research/mugen-pretext.md`,
feature spec `docs/research/feature-inventory.md` §1.

- **Deps**: `gpui` + `gpui_platform` pinned to one Zed rev (Apache-2.0). **We do not use Zed's
  GPL crates** (`markdown`, `ui`, `theme`, `editor`) — markdown, components, and theme are ours.
- **Transcript**: gpui `list()` + `ListState::new(n, ListAlignment::Bottom, overdraw)` (sum-tree
  offsets, follow-tail). On top of it, port the mugen behaviors that gpui doesn't give us:
  - stick-to-bottom **spring** with feed-forward tracking of streaming growth; interrupt from
    *user input* (wheel-up / drag), re-engage within a 70px band; own-send re-engages + smooth
    scrolls;
  - **block-granularity rows** (one row = one markdown block / tool group, not one message) with
    stable ids `msgId#blockId`; live turn stays unsplit, re-splits on persist; optimistic echo
    rows share the client-minted id so persistence never flickers;
  - row height memoization keyed by (row id, content length, width) so a streamed token
    re-measures one row;
  - scroll-anchor absorption for above-viewport height changes.
- **Markdown** (`zeron-ui::markdown`): `pulldown-cmark` parsing on `background_spawn` with
  coalescing (Zed's proven pattern), block-level incremental re-parse of the streaming tail
  (incremark's O(delta) idea: only re-parse from the last stable block boundary), monochrome
  theme where **numbers drive layout, colors are paint**. Code blocks: monospace, no wrap ⇒
  height = lines × line-height (layout independent of highlight); syntax highlighting via
  `synoptic`/`syntect`-class tokenizer run time-sliced in the background, colors applied as text
  runs (paint-only). Streaming **fade-in veil** on newly appended text via `with_animation`
  opacity (paint-layer, never affects layout). `prefers-reduced-motion` honored.
- **Composer**: hand-rolled gpui text input (start from Zed's `examples/input.rs`: IME, selection,
  clipboard, key actions), compact↔expanded auto-flip by measured text width, auto-grow 76–260px,
  Enter/Shift+Enter, Send→Steer→Stop morph, drafts + attachments per chat, drag-drop/paste
  images, QuestionPanel (paged, 1-9 keys, 220ms auto-advance, a corner close / Escape that
  answers nothing) replacing the composer while input is requested. Pickers (harness/model, traits, repo w/ folder browser, branch w/ worktree
  toggle) as gpui popovers with `menu-in` scale/fade.
- **Terminal**: `alacritty_terminal` (vte state machine, MIT/Apache) + `portable-pty` on the
  engine side; custom gpui grid element; tabs w/ drag-reorder (150ms sliding transforms), height
  drag 160px–55vh, 80ms resize debounce, 1MB replay, detach ≠ close. No artificial latency: keys
  are written the moment they land (one write in flight per tab keeps them ordered) and output
  after a quiet spell is sent the moment it is read (only floods are paced, 4ms). Typed text goes
  through the platform text input (IME, dead keys, layout symbols); mouse/focus reporting,
  DECSCUSR cursors with a typing-aware blink, synchronized output, a contrast floor for program
  colors. A terminal on another device streams over its peer link on a `terminal/{id}` stream
  (`engine/src/terminals/link.rs`, `remote_terminals.rs`), reopened as the link comes back without
  a gap (input is numbered and acknowledged, so a reconnect neither drops nor repeats a keystroke); on
  slow links the viewer predicts the echo of typed text (`ui/src/terminal/predict.rs`).
- **Diff pane**: unified-patch parser → virtualized file/hunk/line rows, per-file collapse
  (180ms height tween), time-sliced highlight, 200ms width transition on the pane itself.
- **Animation kit** (`zeron-ui::motion`): small helpers over gpui `Animation` reproducing the
  zeron catalog — `fade-in` (0.5s, cubic-bezier(0.16,1,0.3,1), translateY 4→0), `splash-out`,
  `zeron-pulse` staggered cell wave (boot splash + loaders), `gradient-spin-pulse` matrix
  spinner (WorkingIndicator + rotating flavour word), `menu-in`/`dialog-in` scale-fades, 200ms
  ease-out width/height transitions for sidebar/panes, sidebar-resort **slide animation**
  (we own the list, so animate row positions directly — the View Transitions equivalent, 260ms
  cubic-bezier(0.22,1,0.36,1)), reduced-motion switch.
- **Theme**: independent light/dark resolved variants, theme-owned semantic/syntax/terminal
  palettes, optional interaction-accent overlays, and a device-local surface preference that
  resolves each variant's recommended frost/opaque treatment without changing theme selection.
  Forced frost derives contrast-checked tints from mapped theme surfaces. Local VS Code
  file/package compilation and imported/linked custom families retain last-known-good
  persistence. Colors remain paint-only; hairline borders and bundled Geist/Geist Mono remain
  shared presentation foundations.

## 5. Engine plan

Direct ports of zeron behaviors (spec: feature-inventory §3):
- **Sessions engine**: per-session broadcast hub; on-disk run journal (resumable `seq` replay,
  crash auto-resume); persistent steerable sessions (steering mailbox at step/turn boundary; idle
  reaper; 10min stall watchdog); recovery stamps `aborted`.
- **Doc host**: per-chat handle (serve `doc/{chatId}` to peers, VV sync, write user entries +
  stream assistant segments at 120ms commits, drain commands host-only with processed-ledger
  idempotence); SQLite snapshot store.
- **Harness** (research pending — `docs/research/harness.md`): trait mirroring zeron's
  `HarnessShape`; Claude Code via `claude` CLI stream-json in/out (control protocol for
  permissions/AskUserQuestion→requestInput, resume, steering); Codex via app-server JSON-RPC or
  `codex exec --json`; model/reasoning/option catalogs ported from `packages/harness`.
- **Repos/diffs**: git2 or `git` subprocess (subprocess — matches zeron, avoids libgit2 edge
  cases); worktrees under `~/.zeron/worktrees`; fs watchers (`notify`) + 2min repair; diff
  capture (patch + numstat + untracked, 3MiB cap, sha256) → workspace registry summary.
- **Agent accounts**: credential-slot swap (macOS Keychain via `security-framework`, files
  elsewhere), plan labels, usage probes, paste-code/browser-poll OAuth flows.

## 6. Peers

Every engine binds a QUIC endpoint on its tailnet addresses, finds the user's other engines in the
tailnet's status, and keeps one link to each (`crates/peer`). Admission is Tailscale's whois of the
caller's address. RPC, subscriptions, session docs, the registry, terminals and preview tunnels all
ride those links (`docs/onicode/decisions/topology.md`).

## 7. Parity exclusions & deliberate changes

- **Excluded**: token-usage display (profile heatmap, lifetime stats, per-message token columns,
  `WatchUsage`). Rate-limit meters on agent accounts are *kept* (separate concern; probed from
  CLIs, not CRDT-synced).
- **Changed**: Postgres entity sync/server → workspace registry replicated peer to peer; Electron/React/mugen → gpui with
  ported techniques; Node harness SDKs → subprocess protocols; WebRTC and zeron's relay → QUIC links over the tailnet.
- **Kept verbatim**: session-doc schema shape + constants, command ledger rules,
  render-parts privacy policy, UX behaviors and animation timings.

## 8. Milestones

Status legend: ✅ shipped · 🟡 shipped with named gaps.

- ✅ **M0 Scaffold** — workspace builds; `proto`/`doc` crates with ledger + parts + continuation
  unit tests; gpui hello-window runs.
- ✅ **M1 Doc + sync core** — `zeron-doc` mirror over loro; two replicas converge by version
  vectors over a simulated tailnet.
- ✅ **M2 Engine core** — Claude harness end-to-end headless: `zeron headless` runs a
  turn, journal + doc writes, recovery test.
- ✅ **M3 UI core** — shell (sidebar/panes/header), transcript (virtualized, markdown, streaming,
  stick-to-bottom), composer (send/steer/stop, question panel); local chat fully usable headed.
- ✅ **M4 Multi-device** — peer links over the tailnet, remote device control, workspace registry
  replication, presence from the links. Proven in seeded simulation (`crates/engine/tests/peer_sync.rs`):
  B queues a run into the chat doc, host A executes (mock harness), transcript and session status
  sync back to B.
- 🟡 **M5 Full surface** — terminals, diff pane, repo/branch/folder pickers + worktrees,
  agent accounts UI, settings (devices/shortcuts/archived), Codex harness. Gaps: composer
  attachment UI (engine upload RPCs exist), Cursor harness.
- 🟡 **M6 Polish** — wire reconciliation (`LocalDevice`), keyboard map, clippy/fmt sweep, Linux packaging
  (`scripts/package-linux.sh` + release profile), macOS bundling config (`packaging/macos/`,
  not executed — needs a Mac). Gaps: prefers-reduced-motion, engine hardening
  (instance lock, watchdogs).

## 9. Open questions (tracked, non-blocking)

1. `lorosurgeon` fit for the mirror write path vs hand-rolled reconcile.
2. Cursor harness (zeron has it; CLI surface for Rust TBD) — parity item, scheduled after Codex.
3. Text shaping performance for analytic row heights: gpui measures shaped text natively (Rust ⇒
   cheap), so we start with gpui `list()` measurement + memoization rather than porting pretext's
   full analytic kernel; revisit only if cold-open of huge transcripts measures slow.
