# Onicode for iOS

A UIKit viewport onto the onicode mesh, built on the Rust mobile core. The phone
is a **peer device** on your tailnet: it links to the computers listed in
Settings → Computers over QUIC, mirrors their workspace registry, joins per-chat
session rooms, and drives their engines through the durable command ledger. No
agent runs on the phone.

**Rust decides what to paint and where; Swift paints, scrolls and handles
gestures.** Everything in Rust is platform-neutral — the future Android app
links the same library. See [`docs/mobile-rewrite.md`](../../docs/mobile-rewrite.md).

## Build & run

Requires Xcode 26+ and a Rust toolchain with the iOS targets:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cd apps/ios
xcodebuild -project Onicode.xcodeproj -scheme Onicode \
  -destination 'platform=iOS Simulator,name=iPhone 17 Pro' build
```

The Onicode target's **Rust core** build phase runs `scripts/ios/build-core.sh`,
which builds `crates/mobile` for the active platform (always optimized — the
`mobile` cargo profile) and refreshes the committed UniFFI bindings in
`Onicode/Core/Generated/`. `ONICODE_SKIP_CORE=1` reuses the last built library
when iterating on Swift only.

## Layout

```
Onicode/
  App/         AppDelegate, SceneDelegate, AppModel (owns CoreClient; maps
               workspace snapshots to view models; Keychain credentials)
  Core/        Generated UniFFI bindings; Fonts (the exact bytes Rust measures,
               registered with CoreText) + CoreText fallback measurer
  Design/      Palette (Onicode color roles → light/dark), Glass helpers,
               cell-grid status glyph, harness brand marks
  Transcript/  TranscriptListView (virtualized scroll host over LayoutFrame),
               RowView/RowModel (CoreText painter for Rust display lists,
               streaming veil), FrameRelay
  Session/     SessionViewController, SessionSource (Core/Fixture), new-session
               canvas
  Companions/  What works alongside a session: the companions panel (sheet on
               iPhone, inspector column beside the session on iPad / unfolded
               iPhone Duo), subagent transcripts, side chats, terminals (grid
               painter + key bar over the core's emulator)
  Composer/    ComposerBar (glass capsule ⇄ card with inline context chips;
               send/queue/steer/stop), question and queue panels, attachments
  Threads/     Sessions (foldable sidebar sections)/folder/search lists,
               cells, new-project folder browser
  Shell/       Tab bar (Sessions, Settings, search; "New session" accessory
               with live summary), Settings → Computers
  Debug/       Transcript lab + hitch meter
OnicodeLiveActivity/  Widget extension: the voice call's Live Activity (Dynamic
               Island + Lock Screen), with the orb's states exported from
               onicode-orb as vector assets
OnicodeShared/   Compiled into the app and the extension: the activity's
               attributes and its mute/end App Intents
```

### Companions

A session's subagents, side chats and terminals (`Companions/`):

- **Subagents** come off the transcript (`SessionHandle.subagents()`): a
  spawn's card opens its own transcript, read-only — the host's live doc
  while it runs, its uploaded copy once done (`TranscriptView.attachSubagent`).
- **Side chats** are child chats: a fresh one shares its parent's host,
  folder and agent (`CoreClient.createSideChat`; closed unused, it's
  deleted), a fork copies the conversation through its latest reply
  (`forkSideChat`, answered by the chat's host).
- **Terminals** are PTYs on the chat's host. `crates/mobile/src/terminal`
  runs the desktop's emulator and echo predictor (`crates/term`) over the
  client's resumable `TerminalLink`; Swift paints frames and sends keys.
  Terminals persist per chat and reattach after a relaunch.

### Transcript pipeline

1. `onicode-client` applies session-doc updates incrementally (O(changed
   entries)) and publishes immutable snapshots.
2. `TranscriptView.attach(client, chatId)` subscribes **in Rust**; the layout
   thread turns entries into rows (one per markdown block / user message / tool
   group), reparses streaming markdown incrementally, and measures every row
   with `onicode-text` at the viewport width.
3. Each pass publishes a `LayoutFrame`: exact heights + prefix-sum offsets.
   `rowsIn(y0, y1)` is a binary search; `display(i)` builds a display list
   (text runs at exact positions, boxes, links, scrollers, widgets).
4. `TranscriptListView` positions reusable `RowView`s at those offsets and
   paints runs with CoreText at the Rust coordinates — measurement and
   rendering can't disagree. Display models for rows beyond the viewport are
   prefetched off the main thread.
5. Agent visuals (`show_visual` Luau scripts) get a worker thread each
   (`crates/mobile/src/visual`): it runs the script, lays the node tree out,
   draws canvases and charts as vector paint, renders scenes and shaders with
   wgpu, and reports its height back to the row. `VisualView` paints what
   arrives (rasterized on the worker thread) and forwards touches; the demo's
   "Agent visuals" chat (`-demo -route chat:chat-visuals`) has one of each.

## Launch arguments

| Arg | Effect |
| --- | --- |
| `-demo` | Offline demo workspace (Rust `DemoHost`: registry, docs, streaming replies) |
| `-fast` / `-longreply` | Demo stream speed / reply length |
| `-big` / `-huge` | Demo transcripts with 120 / 600 turns |
| `-route chat:<id>` / `new` / `more` / `search` | Open a screen at launch |
| `-companions` / `-terminal [-terminal-demo]` / `-subagent <tool id>` / `-side-chat <id>` | With `-route chat:<id>`: open its companions panel, a terminal (typing a scripted demo), a subagent, or a side chat (DEBUG) |
| `-engines <a,b>` | The computers to reach, comma-separated, in place of Settings → Computers (DEBUG; saved) |
| `-harness <id>` | Default harness for new sessions (`mock` for live-stack tests) |
| `-bench` (with `-lab`) | Display-link fling benchmark; writes `Documents/bench.json` |
| `-measureopen` | Session open latency; writes `Documents/open.json` |
| `-lab [-turns N] [-autostream] [-autoscroll] [-top] [-meter]` | Transcript lab over fixture markdown, with an on-screen hitch meter |

## Tests

```sh
# Rust
cargo test -p onicode-text -p onicode-markdown -p onicode-client -p onicode-mobile
cargo test --release -p onicode-mobile --lib bench_layout -- --ignored --nocapture

# Line-break accuracy vs CoreText (same font bytes), flows, hitch benchmarks
xcodebuild … -only-testing:OnicodeTests/LineBreakAccuracyTests test
xcodebuild … -only-testing:OnicodeUITests/SessionFlowTests test
xcodebuild … -only-testing:OnicodeUITests/ScrollPerformanceTests test
```

Live stack (a headless engine on a computer on this Mac's tailnet; see
`OnicodeUITests/LiveStackTests.swift`):

```sh
ONICODE_DATA_DIR=/tmp/e2e ONICODE_HARNESS=mock target/debug/onicode headless &
TEST_RUNNER_ONICODE_LIVE_ENGINE=<its tailnet name> xcodebuild … -only-testing:OnicodeUITests/LiveStackTests test
```

The test launches the app with `-harness mock`: a chat's configured harness
wins over the engine's `ONICODE_HARNESS` default, so without it a real agent
would run.

## TestFlight release

Run the **TestFlight** workflow from GitHub Actions on `main`. It installs the
Rust iOS targets, selects the next build number from App Store Connect,
archives (device, Release — the Rust core builds in the archive) with
automatic signing, and uploads a TestFlight build (internal, or external with
beta review). Secrets: `AC_API_KEY_P8`, `AC_API_KEY_ID`,
`AC_API_ISSUER_ID`.
