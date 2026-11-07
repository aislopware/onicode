# Engine: one headless engine per machine, one protocol for every viewer

monocode runs turns inside the window (`crates/app/src/runtime.rs` `run_turn`, spawned from `conversation.rs`)
and reaches other machines through an SSH-tunnelled Node host. Onicode moves everything that does work into
`onicode-engine`, a process per machine that outlives the window. The app, other Macs and the iPhone become
viewers of engines over one protocol, `onicode-protocol`, carried by `onicode-rpc` envelopes over `peer`.

## Authority
- A session belongs to the engine that runs it, its host, and only the host writes its state. Viewers send
  commands and watch: a snapshot at a `Cursor {epoch, seq}`, then every event after it. `seq` counts one
  session's events, and `epoch` names the engine run that numbered them.
- Each session keeps a ring of its recent events (2048 events or 4 MiB). A viewer that comes back with a cursor
  the ring still covers gets the missed events. Anything else, including an engine restart (new epoch), gets a
  fresh snapshot. Settled rows are already in SQLite, so the ring holds only what a reconnect needs and is
  never persisted.
- There are no replicas. Zeron's Loro docs, registry version vectors and mailboxes are not coming back;
  `topology.md`, `replica-sync.md` and `local-feel.md` say so since step 9.
- With its host offline, a remote session shows the viewer's last snapshot, read-only and marked offline;
  commands for it wait in the forwarding engine's durable `outbox` table.

## Ids
- `EngineId` is the tailnet device id bound at admission (`transport.md`). `SessionId`, `MessageId`, `OpId` and
  `TerminalId` are UUIDv7, minted by whoever creates the thing, usually the viewer, so a new chat, a sent prompt
  or a queued message has its final id before any byte leaves.
- The store's `i64` rowids stay inside the engine, and `native_sessions` gains a unique `uid`. Prompt rows carry
  their `MessageId` (`transcript::Row` gains `id`), so a viewer's echo is replaced in place.
- A session elsewhere is `SessionKey {engine, session}`, and a project is `ProjectRef {engine, path}`. Together
  they replace the `remote://` cwd scheme and its 82 call sites.
- Every mutating call carries an `OpId`. The host keeps a processed-op ledger (`ops` table, pruned after
  7 days) and answers a repeated op with its first receipt. That makes every retry safe: a resend after a
  reconnect, the outbox, a phone in the background.

## Protocol
`crates/protocol` (`onicode-protocol`) holds plain serde types and no IO. The data types the UI renders (`Row`,
`Approval`, `Question`, `agent::Settings`, `TurnInfo`, `context::Usage`, `follow_up::State`, `tool`
presentation) are re-exported from `onicode-core` until step 6. Then they move into the protocol crate, and
`crates/app` stops depending on core.

```rust
pub const PROTOCOL: u32 = 2;
pub trait Call  { const NAME: &'static str; const AGENT: bool = false; type Params; type Reply; }
pub trait Watch { const NAME: &'static str; type Params; type State; type Event; }
pub struct Cursor { pub epoch: u64, pub seq: u64 }
pub enum Item<S, E> { Snapshot { at: Cursor, state: S }, Events { first: u64, last: u64, events: Vec<E> }, Resync }
pub struct Routed<T> { pub engine: Option<EngineId>, pub op: Option<OpId>, pub body: T }
pub enum Outcome { Applied, Rejected(Rejection) } // Rejection: AlreadyResolved, StaleRev, Gone, Denied(String)

pub enum SessionEvent {
    Rows { start: usize, rows: Vec<Row> },  // replace from `start`: runtime::Update's `start`/`rows`
    Append { row: usize, text: String },    // the row's text grew by `text`: the streaming fast path
    Status { status: String, turn: Option<TurnState> },
    Approval(Approval), ApprovalResolved(String), Question(Question), QuestionResolved(String),
    Queue { rev: u64, state: follow_up::State },
    Settings(agent::Settings), Title(String), ProviderId(String), Model(String), Worktree(String),
    Context(context::Usage), Metrics(TurnMetrics), UsageLimit(Option<usage::Limit>),
    Background(Vec<BackgroundTask>), Commands(Vec<NativeCommand>), Handoff(Option<Handoff>),
    TurnFinished { turn: usize, info: TurnInfo },
    Receipt { op: OpId, outcome: Outcome },  // what a pending echo waits for
}
```

`SessionEvent` is `turn::Update` (`crates/engine/src/turn.rs:28-56`) split into one variant per field, so the
first port is a translation, not a redesign.

| Group | Watches | Calls |
|---|---|---|
| Engine | `WatchMachines` (local engine only: directory, link path, versions, live-agent counts) | `Hello`, `EngineInfo`, `UpdateEngine`, `StopEngine` |
| Workspace | `WatchWorkspace` (session summaries with marks, projects, folders and groups, reminders, unread) | `CreateSession`, `Rename`, `Archive`, `Pin`, `Delete`, `SetWorkItem`, `SetReminder`, `SaveGroups`, `SearchSessions`, notes |
| Session | `WatchSession {session, since, window_turns}` | `Send`, `Steer`, `Stop`, `Decide {approval, answer}`, `Answer`, `KeepQuestionOpen`, `QueueEdit {id, base_rev, edit}`, `SendNow`, `SteerNow`, `Configure`, `EditLastTurn`, `Rewind`, `Fork`, `Btw`, `SwitchProvider`, `ReadTranscript {before_turn, turns}` |
| Files | `WatchDir {root, path}` (listings with a scan id) | `ListDir`, `Stat`, `Read`, `Write`, `Create`, `Move`, `DeletePath`, `SearchPaths`, `SearchContent`, `ListDrives`, `SearchHomeFolders`, `SaveAttachments`, `SaveNoteImages` |
| Git | `WatchGit {checkout}` (status and branch, from the engine's fs watch) | `ReadStatus`, `Diff`, `FileDiff`, `Branches`, `Refs`, `History`, `Switch`, `CreateBranch`, `Stage`, `Unstage`, `Discard`, `Commit`, `Push`, `Pull`, `Sync`, `HeadMessage`, `CommitContext`, `PrContext`, `PrStatus`, `CreatePr`, `Worktrees`, `CreateWorktree`, `RemoveWorktree`, `ReadCheckpoint`, `RestoreCheckpoint`, `ReadSessionChanges` |
| Terminal | `WatchTerminal {terminal, viewer, since}` (a libghostty snapshot of the host's screen through READY, its scrollback as an unnumbered `History` right after it, then output, resizes, clipboard writes, OSC 7501 `Status` and exit; `terminal.md`) | `OpenTerminal {terminal, viewer, cwd, size, colors}`, `ListTerminals` (with each terminal's status), `ResizeTerminal {viewer}`, `CloseTerminal`, `TerminalInput {viewer, Key, Text, Paste, Wheel}`, `ReadForeground`, `StopForeground`. The PTY takes the size of the viewer that typed last. |
| Agents | `WatchAccounts` (accounts with usage from the streams) | `Catalog`, `Skills`, `Commands`, `AddAccount`, `Login`, `Logout`, `SetProviderEnabled`, MCP servers, `HarnessUpdates` |
| Engine settings | `WatchEngineSettings` | `SetEngineSetting` (typed keys: keep-awake, worktree defaults, title generation, hooks) |
| Work | `WatchAutomations`, `WatchInbox` | automation upsert, delete and run now; inbox sources and refresh; `LinkWorkItem` |
| Alerts | `WatchAlerts` (`alerts.md`) | `SetViewportFocus` |

Rules:
- An approval answered on two devices is decided by whichever answer reaches the host first. The other gets
  `Rejected(AlreadyResolved)` and its echo rolls back visibly.
- Queue edits carry the `rev` they saw. An edit made against an older `rev` is `StaleRev`. A rev check replaces
  zeron's edit leases.
- `Call::AGENT` marks what an agent's `onicode mcp` may call with its scoped token, replacing
  `onicode_rpc::agent_allowed`. `onicode_rpc::methods` (zeron's 165 names) is deleted.

**Encoding.** onicode-rpc's JSON envelopes stay: `{id, method, params}`, then `ok`/`err`, or `item`* and `done`.
Over `peer` each call or watch gets its own QUIC stream (`PeerCarrier`). Locally they are ndjson over one Unix
socket (step 8), or `memory_client` in process. Steady-state traffic is small deltas, so JSON costs little and
stays readable in logs. A bench can still move snapshots to a binary codec later.

**Routing.** `Routed.engine` names the target. The app talks only to its own engine, which serves the call
itself or forwards it over `PeerLinks` (`topology.md`). Forwarded watches ride `PeerSubscription`, which
resubscribes on the kept link with the viewer's cursor, so resuming works end to end. The iPhone has no
engine of its own and dials every engine directly through `EngineList`.

**Backpressure.** A turn never waits on a viewer: today's `publish` spin-waits on the window (`turn.rs:57`),
and that goes. Each watcher has a bounded queue (256 batches or 4 MiB). Within the queue, an `Append` merges
into the pending `Append` for the same row, and a `Rows` patch absorbs older patches of the rows it covers. A
watcher that still overflows is dropped to `Resync` and comes back with its cursor. QUIC stream flow control
(16 MiB per stream) bounds the wire. Merged events cover more numbers than they are, so `Item::Events` carries
`first` and `last` and the watcher's cursor moves to `last`; a `Status` also folds into a pending `Status`
unless a finished turn sits between them.

**Step 3 as built.** `Row.id` holds the prompt's `MessageId` only while the engine holds the session live; the
store does not keep it, so rows read back carry none. The protocol does not carry `Settings`' transport-only
fields (the prepared prompt, the turn's model identity, the environment, the queued message's id) or the stop
flag, so until the composer, catalog and queue move into the engine the app stages them in process
(`SessionHub::stage`) under the call's `MessageId`. The conversation still merges `turn::Update`s: a `Viewer`
mirrors the session from its watch and folds what arrived into one `Update` per beat. The follow-up queue stays
app-owned, its steers sent as `Steer` with the queued message staged whole; `SendNow`, `SteerNow`,
`EditLastTurn`, `Rewind`, `Fork`, `Btw` and `SwitchProvider` answer "not served yet" until step 4.

**Step 4 as built.** The engine's `WorkspaceHub` holds the workspace (every session's summary, the projects'
folders, reminders, orchestration summaries, the note count and the automation runs waiting to start) and serves
it as `WatchWorkspace` with a session's seq, ring and backpressure. Only the Workspace calls change it, each
answered from the ledger on repeat; a created session takes the viewer's `SessionId`. A session created that
way which no turn has started in is removed when its last session watcher leaves, once the store shows no rows and
no saved draft in it (another viewer of the engine may have used it), and every viewer sees it go as `Removed`;
the mark lives in memory, so a session left from an earlier engine run stays. The desktop holds no session watch
while a blank chat is open, so what it shows of blank chats is still monocode's own draft behaviour. Sessions are
keyed there by the store's row id beside their `SessionId`, because folders, reminders and the saved layout name sessions by row
until `SessionKey`; `SessionSummary.key` is the id other tools know a session by. The engine runs the reminder and
automation schedulers on its own clock (a beat at start, then every 30 s in process; tests beat in virtual time),
recovers runs an earlier engine claimed, keeps and retries Inbox event claims, generates titles and worktree branch
names, stores notes under the id the viewer minted (`CreateNote` refuses a taken one, so a new note opens in the
frame it is made) and keeps orchestration summaries; a plan's dispatch is the pure `orchestration::Run`. A session's
summary marks a saved draft as `SaveQueue` stores or clears it. The shell mirrors the watch into the maps its views
already read, moving only the fields whose engine value changed (a title, pin or draft mark the app set stays until
the engine moves that field), and echoes each call in the same frame until its receipt; a refused echo returns to
the engine's value.
A conversation reads its record and transcript (`ReadSession`, `ReadTranscript`) and saves its queue (`SaveQueue`)
through the Session calls. `SendNow` and `SteerNow` take a stored queued message by its `MessageId`; the desktop
steers with `SteerNow` and still launches a queued message with `Send`. `EditLastTurn` replaces the turn that
`RecallLastTurn` holds. A side question (`Btw`, `ReadBtw`, `SaveBtw`, `StopBtw`) streams its answer as `BtwText`
events to a watch that holds no turns. Deleting a session stops its title generation, work item lookup and branch
naming, so no provider or `gh` runs for it after it goes; closing the conversation stops its side answers with
`StopBtw`, as monocode's drop did. `Rewind`, `Fork` and `SwitchProvider` stay unserved until the
composer and accounts move (step 6).
App-side for now: a run's prompt still goes through the conversation, since the composer and accounts move in step
6 (the engine claims the run, creates its session and records its end); the Inbox view still fetches from its
providers and reports what appeared (`ClaimInboxEvents`) until the providers' fetch moves with a watch of its own;
the Automations view edits definitions, and the app bridge (`sessions.create`, `sessions.read`, notes actions,
receipts) uses the store, until settings move (step 6); the layout and seen markers are preferences (step 6); note
images, attachments, worktree removal and session search are files and search (step 5). `PROTOCOL` is 4; the golden
file is re-recorded with `ONICODE_GOLDEN=record`.

**Step 5 as built.** The engine's `FilesHub` and `CheckoutHub` serve `WatchDir` and `WatchGit`, and every hub
(session, workspace, files, Git) serves its watch through one generic `Topic` (`feed.rs`): the session's numbering
and backpressure (a ring of 2048 events or 4 MiB, a queue of 256 batches or 4 MiB per watcher whose pending events
merge as each feed allows, `Resync` past that). A watcher that comes back with a backlog bigger than its queue gets
a snapshot instead, so a rejoin past the queue size converges rather than resyncing forever (a proptest per hub).
A watcher sent `Resync` stays one until its stream is dropped, and a viewer resubscribes before dropping it, so an
idle session is never unloaded and reloaded from the store between the two.
Listing and status events are whole states, so a viewer replaces what it holds. Each watched root or checkout has
one platform watch (`notify`, FSEvents) and a pump thread: reports are renamed from the resolved root
(`/private/var`) to the root the viewer named, filtered (build output, Git's object store and lock files never
move a status), and coalesced in virtual time: a burst settles 300 ms after its first change, more than 4096 paths
or a dropped report rereads everything, and a repair read runs every 30 s (a poll every 2 s where no platform watch
starts). A rescan never blocks a turn: it runs on the pump thread, listings on the hub's spawner, and only a
listing or status that changed is published.

Every listing and status read takes a scan number, counted from the engine run's start (`started << 20`) so it
rises across runs, and a read older than one already offered is dropped. A change that lands while a folder's
first watcher is still reading it rereads it (up to 3 reads), so a new watch never starts on a stale listing. Calls
that change a directory relist it before they reply and name the covering listings (`NewPath`/`Deleted` carry
`Relisted {root, path, scan}`); Git calls that change a checkout reread its status in the same lane slot and reply
`Covered {scan}`, the scan of the status the checkout's watchers then hold (`None` when nothing watches it). The
viewer keeps its echo until it holds a listing or status at that scan, so a read from before the call that arrives
after the reply never flashes the old state, and a watched checkout is never read again for a call (an unwatched
one is read once). Git runs in two lanes the caller picks: a foreground lane of 8 slots for what a person waits on
(calls and the watch's burst reads) and a background lane of 2 for pull-request discovery and repair reads, so a
background flood never holds a foreground slot. Mutating calls (`Write`, `Create`, `Move`, `DeletePath`,
`SaveAttachments`, `SaveNoteImages`, and every Git call that changes a checkout, a branch, a worktree or a
checkpoint) run once per `OpId` through a ledger over the store's `ops` table; a failure is not recorded, so its
retry runs again. Reads come back whole in one reply: `Read` carries the file's FNV-1a digest, answers `Unchanged`
when the viewer's digest still matches (the editor's 2 s poll then moves no bytes), returns images as base64 and
refuses files the editor cannot show with the reason; `Write` takes the digest it was read at and refuses to
overwrite a file that changed since. `CommitFiles` (a transcript's commit card) and `ReadDiffStats` (the project
rail's counts) are Git reads; `Refs` names the worktree the checkout is (`here`, by canonical path, as monocode
compared them).

Searches (`SearchPaths`, `SearchContent`, `SearchSessions`) run under an id the viewer mints for each one; a newer
search under the same id stops the older, and `StopSearch` stops one when its query moves on or its view closes,
as monocode's cancel flag did. A stop that arrives before its search is remembered, so the search stops as it
starts. Saved conversations are searched by the engine; the rows a live session holds in the app are searched in
place.

The app's `ProjectGit` mirrors `WatchGit` for the current project and shows stage, unstage and discard in the frame
they are made (`GitEcho`) until the covering status; a hunk staged from review or the editor shows once the
engine's status does, as in monocode. The explorer watches every loaded folder with `WatchDir` and echoes create,
move, delete, paste, duplicate and Finder copies the same way, a copy under the name Finder will give it; Finder
copies run one at a time and stop at the first failure, as monocode's did. Editor buffers stay in the app until
save. `git_cache` holds `Refs` replies for the branch and worktree pickers. `fs_watch.rs` left the app, and with it
the app's `notify`. App-side for now: revealing in Finder and screenshots (they act on the viewer's own Mac); the
commit message and pull-request text prompts, which run the viewer's provider until accounts move (step 6), and the
phone bridge's Git, which goes with `mobile/` (step 11); the Settings → Worktrees page data (locked, dirty and
unpushed marks) and a session's worktree membership, which are store and settings reads (step 6); `.git/HEAD` read
for the project menu's branch label. `PROTOCOL` is 6.

**Step 6 as built.** Four groups join the call table. Agents (`agents.rs`) serves the account book, sign-in,
usage, model catalogs, skills, native commands, MCP servers, the provider CLIs and their updates, one-off text
(`Generate`, `StopGenerate`) and the idle agents, with `WatchAccounts` (the book, each account's sign-in and each
usage read, an event replacing what it names). Engine settings (`engine_settings.rs`) serves `WatchEngineSettings`,
`SetEngineSetting` (one typed key) and `TakeViewerState`. Work serves the Inbox, automations, linked activity,
agent processes and `ReadWorkItem(Details)`; App access serves the Operator bridge's store reads; Settings pages
serves `ReadWorktreeMarks`. Every hub publishes on the generic `Topic`, every mutating call runs once per `OpId`
through the ledger, and the provider CLIs, `gh` and the trackers sit behind seams (`AgentProbe`, `Gh`,
`Trackers`) that the tests script. Who an account is comes only from the CLIs' published doors: `claude auth
status --json` (an `oauth_token` sign-in on the first-party API with no `subscriptionType`, a setup token, reads
as "Claude subscription") and Codex's `account/read`; the engine never reads a credential file. A model's limits
are optional field by field and a model is never dropped for them. Usage is polled at most once a minute unless
forced, read again after a turn ends on a provider that reports it from turns, and a reset is claimed once.
`Generate` runs the asked model, else the background model, then its `fallback` (the conversation's provider),
then Claude Code, then Codex.
The session group now serves `Rewind`, `Fork`, `SwitchProvider`, `ResetThread` and `Compact`. A rewind never asks
the provider to rewind: it cuts the rows from the turn's prompt on (in both store layouts), lets the provider
thread go and hands the next prompt the kept turns as a handoff brief; `Fork` does the same into a new session the
workspace creates, so every viewer sees it arrive. `Compact` runs the provider's own compaction behind the
`Asides` seam, records it as a `contextCompaction` tool row, holds off turns while it runs and stops on `Stop`.
`ProviderId` carries `Option<String>`, so watchers see a thread let go.
The usage limit is the engine's too: a turn that ends at one leaves it in the session's settings, every turn's
start lifts it, and a viewer's `Configure` only arms or disarms the limit the engine holds, so a viewer saving an
older copy can neither arm a limit a resume took nor drop one a turn just recorded (dismissing the banner disarms). The scheduler beat (`Local::clock`) finds armed
limits with a store query, fills a missing reset from the account's usage (`exhausted_reset`), and sends each due,
idle session "Continue from where you left off." once, under a fresh `OpId` per attempt, with no viewer open and
however many there are; the conversation only follows what the engine holds. The resume is kept in the store from
before its limit is cleared until its turn ends: a launch refused before its prompt was saved arms the limit again
for the next beat, and an engine that stopped in between finds it on its next start, drops the unsent copy of its
prompt and sends it again, but never one the provider already spoke in. That is t3code #31292's lesson: their
resume reused an id derived from the limit, so after a restart the ledger answered the retry with the old receipt
and the resume never came back.
The preferences split in two (`ui-state.md`): the engine's part (`EngineSettings`) lives in the store under
`engine-settings`, moved once from monocode's `workspace` key; the app's part lives in its own `ui.sqlite3`,
handed over once by `TakeViewerState`. The app opens no engine database: `main.rs` opens an `Engine` over
`onicode_engine::Local`, and `crates/app/clippy.toml` holds the boundary with `disallowed-types` (`Store`,
`rusqlite::Connection`, `notify`) and `disallowed-methods` (`Store::open`, the provider runs and CLI lookups,
`catalog::models_for`, `std::process::Command::new`). Each exception is an `#[expect]` naming why: `ui_state.rs`,
test git fixtures, commands that act on the viewer's own Mac (screenshots, sounds, System Settings, the hang
sampler), the terminal (step 7), the SSH remote (step 10) and the phone host and its Changes page (step 11).
The app still depends on `onicode-core` for those files and for viewer-side work core holds beside its IO: image
decoding and thumbnails, attachment checks, the visual runtime, the app bridge's socket, process teardown, the
external editors, resource pressure, and a few path helpers (`worktree_management::default_root`,
`files::document`, `git::naming`). Pure types come from `onicode_protocol`. `PROTOCOL` is 7.

**Step 8 as built.** `onicode-engine` (`crates/engine/src/bin`, `host.rs`) makes its data dir `0700`, takes
`engine.lock` with `flock` and writes its pid, executable and the executable's build time into it, opens the
store and hubs, publishes fresh tokens, removes any socket a crashed engine left and binds `engine.sock`
(`0600`). An engine that loses the lock exits quietly, so two launches racing leave one engine. `onicode-rpc`
is now only the transport: ndjson envelopes over the socket, one task per connection, every ready frame written
in one write, and a gate of `getpeereid` (same uid) then a token line (`{"token","chat"}`, answered `full` or
`agent`). The engine holds the policy: an agent's token reaches only `Call::AGENT` methods, and until a
connection says `Hello` with this `PROTOCOL` only the frozen calls are answered. The engine beats its schedulers
and sweeps deleted conversations itself; `StopEngine`, `SIGTERM` or `SIGINT` stop it, socket removed before the
lock goes, agents with it. The WebSocket server, `onicode_rpc::methods` and the in-process app engine are
deleted; `memory_client` serves tests only. The viewer side is `onicode_engine::link`: one connection shared by
every call and watch, connected on first use. Dial; when nothing answers, launch once through `onicode-launch`
(detached with `setsid`, its stderr in `engine.log`) and poll from 1 ms doubling to 50 ms for up to 5 s; greet
with `Hello`. An engine can come up and die between two looks, so one still absent 500 ms after its launch is
launched again, less often each time; a second engine racing a slow first one loses the lock and exits. An engine on another `PROTOCOL`, or not the build beside this app (the lock names another
executable or build time), is sent `SIGTERM` and replaced, so a rebuilt dev app never talks to a stale engine.
A call whose connection closes before its reply is sent again with the same params, so its `OpId` gets the
first receipt; a watch whose connection closes reads as lost and the app resubscribes with its cursor, and an
engine that restarted (epochs count from its start time) answers with a fresh snapshot. The composer's context
crosses as `Send.turn` (`TurnContext`), replacing step 3's in-process staging, and the engine ignores it from an
agent's token; `ResolveSession` names a stored row's `SessionId`. The app keeps the workspace and engine
settings it last saw in `viewer-cache.json`, so a launch draws from it while the engine starts; only a first
launch waits (up to 10 s). Closing the window stops nothing: only the conversation's stop sends `Stop`, and a
turn whose engine restarted under it ends saying so. The SSH remote and the phone host open their own
connection to the store until steps 10 and 11. Debug fixtures are written with the engine stopped, under its
lock.

**Step 9 as built.** The engine joins the tailnet through `onicode-peer` (`peers.rs`): a QUIC endpoint on the
system Tailscale daemon that admits only what whois says the person owns, a directory and one link per device.
It is off under `ONICODE_PEERS=0` (tests and the smoke test), and tries again every 5 s while the daemon is not
running. Its `EngineId` is the tailnet node's stable id, kept in `device-id` for when the tailnet is down, and
its hello's capabilities carry `protocol:N`. `router.rs` wraps the engine's service: what `Routed.engine` names
elsewhere is forwarded over the link to that engine, one QUIC stream per call or watch, for every group alike.
Only the app's own connections forward; a peer or an agent naming another engine is refused, so there is one
hop. A peer on another `PROTOCOL` gets only the frozen calls, and a forward to one fails as needing an update. A
forwarded watch is a `PeerSubscription` whose resume hook holds the viewer's cursor: when the link drops it
waits for the next one and subscribes again with that cursor, so a cursor still in the host's ring costs one
round trip and no snapshot, and a host that restarted answers with a fresh one. A terminal's snapshot still
comes ahead of its history. Each carrier stream holds at most 256 KiB before it waits on QUIC flow control,
so a slow viewer's backlog reaches the host's queue and only that viewer gets `Resync`. A mutating call for an
engine that cannot be reached goes into the store's `outbox` table (`outbox.rs`) and its caller waits:
deliveries run per engine, oldest first, across restarts of either side, and one whose reply is lost goes again
under the same op, which the host's ledger answers with its first receipt. `WatchMachines` (`machines.rs`)
lists this engine and every engine and viewer the directory knows, with link state, path (direct, peer relay,
DERP), round trip, version, protocol and live agents (`EngineLoad`, asked every 5 s). `SessionKey` and
`ProjectRef` name things elsewhere; the app's `Engine::on` aims calls, watches and turns at another engine
through its own, and a turn for one that is offline is refused with a status saying so while the conversation
keeps what it last saw. Zeron's `StreamRouter` is not ported: engines serve each other only RPC streams.
`PROTOCOL` is 10.

**Steps 10 and 11 as built.** The SSH/Node remote and the phone host are deleted: `runtime/remote.rs`,
`conversation/{remote,remote_plans,mobile}.rs`, `transcript/remote_plans.rs`,
`shell/{remote_sessions,remote_projects,mobile_host}.rs`, `mobile/`, `settings/pages/mobile.rs`,
`core/src/{remote,mobile}*`, `core/src/{files,git}/remote.rs`, `store/remote*.rs`, `protocol::remote`,
`crates/mobile-link` and the askpass hook in `main.rs`. With them go every `remote://` path and
`remote-attachment://` source, the `remote-plan` transcript row, `agent::Settings.remote_action`, the app's own
connection to the store (`Engine::legacy_store`), and the phone's Changes page, the app's last provider runs. Every
`is_remote()` gate guarded only the SSH route, so each fell with its remote branch and the local path stays;
another machine's session is reached through `Engine::on`. Settings → Connections lists what `WatchMachines`
says, in monocode's machine rows: this engine and every engine and viewer on the tailnet, with link state, path,
round trip, version, protocol and live agents. Adding a machine and "Open folder on a machine…" return with step
12, the phones half with step 13. `PROTOCOL` stays 10.

**Step 12 as built.** A project on another engine is spelled `peer://<engine>/<path>` wherever the app
keeps a path (`ProjectRef::uri`/`parse_uri`; the engine id escapes `%` and `/`, so any id reads back,
and the path is absolute, as engines name folders); plain paths are the app's own engine's. One function,
`engine_client::for_project`, turns either spelling into the engine that serves it (`Engine::on` for a
peer) and the path as that engine names it, and every view that reads or changes a project's files,
checkouts, terminals, searches or worktrees goes through it; `Engine::app_path` spells what an engine
answers back in the app's way. Settings, MCP, accounts, the inbox, harness updates and resource pressure
stay on the app's own engine. The app's engine lists only its own sessions, so the shell follows the
workspace of every engine a `peer://` project names (`shell/peer_sessions.rs`) and lays that project's
sessions over its maps like its own. The shell keys sessions by row, and two engines' rows collide, so a
peer session is named by a row of the app's own, `peer_row(SessionKey)`: FNV-1a over the engine and
session ids folded above 2^62, the same on every launch (a saved tab comes back) and apart from engine
rows and the panes' negative ids. `Engine::adopt` tells the client which row and `SessionId` that is
there, so session calls, turns and the row-bearing workspace calls (`Engine::row`) reach it; its
conversation is handed `Engine::on(peer)`. Each followed workspace is kept in `viewer-cache.json` beside
the app's own, so an offline engine's rows show as last seen across launches, and a turn for one is
refused with the status its engine gives. Folders, reminders and orchestration in a peer project stay on
that engine: its sessions show ungrouped. "Open folder on a machine" is monocode's modal again
(`shell/peer_projects.rs`), its machine chips from `WatchMachines`, its folders from the peer's `ListDir`
(which now reads `~` as the engine's home) and, for a typed name, `SearchHomeFolders`; it checks the
folder with `Stat` and adds the project's URI. Its entries are back in the sidebar project menu, the
header and compact project pickers and the rail's add menu. The focused sidebar's Machines section sits in
the slot monocode's Monos held, with the same header: each other engine (never a viewer, never this
machine) with its `internet` glyph tinted by link (direct, relayed, away), its name and its running
agents; a row opens the modal on that machine and `+` opens Connections' add card ("Install Onicode on
the machine and sign in to the same tailnet; it appears here."). The shell keeps only what the section
draws, so a round trip moving every few seconds never redraws the sidebar. A peer project shows the
`internet` tile and the pickers' globe. Connections has no "Forget": the directory's `forget` lasts only
until the next refresh, and a machine leaves the list when it leaves the tailnet. No protocol shape
changed; `PROTOCOL` stays 10.

**Versioning.** `Hello` is the first call on every connection and exchanges `{protocol, version, capabilities}`.
There is no compatibility before release. A peer with a different `PROTOCOL` is answered only `Hello`,
`EngineInfo` and `UpdateEngine`, whose shapes are frozen, so the machines list can say "Needs update" and offer
an update. `PROTOCOL` is separate from net's `WIRE` for that reason. A golden test pins the table of names and
schema hashes, so any change to a shape fails until `PROTOCOL` is bumped.

## Local feel
Local and remote take one path: the app's `EngineClient` speaks the protocol to its own engine in both cases.
- **Echo.** Every action echoes in the same frame under its client-minted id and stays pending until a
  `Receipt` or a covering event arrives, never confirmed on a timer (`local-feel.md`).
- **Streaming.** All pending events go in as one patch per beat of the app's 30 Hz pulse (`pulse.rs`, already
  awaited in `conversation.rs`), so the transcript's reveal pacing is unchanged.
- **Cache.** The viewer keeps the snapshots of sessions it opened this run and each engine's workspace snapshot
  on disk, so the first frame never waits on the engine.

| Path | Target |
|---|---|
| Echo of send, steer, stop, approve, answer, queue edit, rename, archive | same frame, 0 network events before it |
| Receipt from the local engine | ≤ 0.3 ms in process, ≤ 1 ms p99 over the socket |
| Receipt from a remote engine | 1 RTT + 1 ms |
| Provider output to pixels | core's 16 ms tick + one beat (33 ms); remote adds ½ RTT |
| Open a session seen this run / a cold local one (last 3 turns) | 1 frame / ≤ 25 ms |
| Resume after a link drop (cursor still in the ring) | 1 RTT, no snapshot |
| Engine launch to `Hello` | ≤ 150 ms; the window's first frame never waits on it |

Round-trip counts are checked in seeded virtual time over `onicode-net-sim` against a baseline that only
ratchets down; wall-clock numbers run as Mac benches.

## Process
- `onicode-engine` ships in the bundle beside the app, and `onicode-launch` starts it detached. That is zeron's
  measured choice: TCC permissions stay attributed to the app, and no Login Items approval is needed. An
  instance lock on the data dir keeps one engine per machine.
- A server Mac can turn on "Run at login", which registers the same binary as an `SMAppService` agent. Whether
  TCC then attributes the agent's file access to Onicode must be checked on a Mac before that option ships.
- The engine holds the store (one writer, plus the `search_reader` connection), the providers, PTYs, fs
  watches, schedulers and keep-awake. Its agents die with it (`claude-harness.md`); closing the window stops
  nothing.

## Plan

**Boundary.** `crates/core` becomes the engine's library whole. From `crates/app/src` into `crates/engine/src`:
- **Turns:** `runtime.rs` with `runtime/{persistence,handoff,visual_discovery}.rs` and their tests, going to
  `engine::turn`. The turn launch and update merge in `conversation.rs` (~1000-1300) become a `SessionHub` that
  turns `Update` into `SessionEvent`s.
- **Store and run parts of the conversation modules:**
  `conversation/{follow_up,edit_last_turn,btw,title,worktree_naming}.rs`, and the account-switch decision in
  `conversation/accounts.rs`.
- **Schedulers:** `shell/automation_runs.rs` (`claim_due`), the reminder tick in `shell/session_groups.rs:722`,
  and plan execution in `shell/orchestration.rs`.
- **Inbox:** the polling in `inbox.rs`. **Filesystem and git:** `fs_watch.rs`, the git command side of
  `git_status.rs` and `git_cache.rs`, and checkpoint runs in `session_review.rs`.
- **Search:** the search back ends behind `project_search.rs` and `search.rs`.
- **Terminals:** the PTY and shell in `terminal.rs` (moved: `crates/term` and `TerminalHub`). **Agent bridges:** `visual-mcp` and `app_access`
  from `main.rs`.

These are deleted: `runtime/remote.rs`, `conversation/{remote,remote_plans,mobile}.rs`,
`shell/{remote_sessions,mobile_host}.rs`, `mobile/`, `core/src/{remote,mobile}*`, `store/remote*.rs`,
`crates/mobile-link`, and the askpass hook in `main.rs`.

The app keeps pure view state: every view, layout, motion and decoration; the composer, its draft, pickers and
completion; the transcript view and its reveal; tabs, decks and splits; the explorer, editor, diff, review and
history views (fed by calls); the terminal mirror and its element; open-file buffers until save; and the UI
preferences (theme, layout, keybindings, panels, sounds), split out of `settings/preferences.rs` into the app's
own `ui.sqlite3` with `ui_state` and `window_state.rs`. It asks the engine for store reads and writes, agent and
engine settings, accounts and usage, git, worktrees and checkpoints, terminals, every file system call and
search, and the inbox, automations, notes and reminders. A `clippy.toml` `disallowed-types`/`disallowed-methods`
list in `crates/app` (`Store`, `provider::*`, `std::process::Command`, `notify`) holds the boundary.

**Migration.** Each step is one batch for one agent, with `scripts/dev/check.sh` green and the app usable
after it.
1. `crates/protocol`: ids, `Cursor`, `Item`, the `Call`/`Watch` traits, `SessionEvent`, the call table, and
   `PROTOCOL` with its golden test and serde round-trip proptests. No users yet.
2. Create `crates/engine` and move `runtime*` into it as is; `conversation.rs` calls
   `onicode_engine::turn::run_turn`.
3. `SessionHub`: seq, epoch, ring and the `Update`→`SessionEvent` mapping. The engine serves session watches and
   commands as an `RpcService`. The app's `Conversation` drives turns through `EngineClient` over
   `memory_client` on a tokio thread, with echo by `MessageId` and receipts. `native_sessions.uid` and the
   `ops` table land here, along with the session model test.
4. Workspace, sidebar and history on `WatchWorkspace`. Reminders, automations, orchestration, title and worktree
   naming, btw, notes and inbox polling move into the engine. `locked(&store)` leaves `shell/*` and
   `conversation/*` (241 sites today).
5. Files, git, search, worktrees and checkpoints: `WatchDir` (from `fs_watch.rs`), `WatchGit` and the calls,
   used by the explorer, editor, changes, diff, review, history and worktree switcher.
6. Engine settings, accounts, catalog, skills, MCP and usage. Split the preferences, then add the clippy
   boundary. The app stops opening SQLite and stops depending on `onicode-core`.
7. Terminal hosts move into the engine, on libghostty-vt (`terminal.md`). `crates/term` holds the PTY, the
   login shell, and the host terminal that answers queries and encodes snapshots. The engine's `TerminalHub`
   publishes each shell's output on a feed topic, whose ring is the output ring. The app's terminal view is a
   libghostty mirror of `WatchTerminal` and sends keys, paste and the wheel as `TerminalInput`. alacritty is gone.
8. Out of process: the `onicode-engine` binary, the instance lock, a Unix socket in the `0700` data dir
   (`getpeereid` plus the `ipc_auth` tokens, full or agent), launch through `onicode-launch`, and
   connect-or-launch with resume in the app. Turns survive the window; keep-awake and alerts follow.
9. Peers: the engine joins the tailnet (zeron's `Peers` and `StreamRouter`,
   `git show f01736b3^:crates/engine/src/peers.rs`) with `Routed` forwarding, `PeerSubscription` with cursors,
   the outbox and `WatchMachines`. `SessionKey`/`ProjectRef` replace `remote://`; `is_remote()` gates fall as
   each capability works over peers.
10. Delete the SSH/Node remote (the deletions under Boundary). The Connections rows read `WatchMachines` so
    the page never empties (`layout-gaps.md` §7.5).
11. Delete `crates/mobile-link`, `core/src/mobile*`, `shell/mobile_host.rs` and `mobile/`.
12. Machines UI (`layout-gaps.md` §2): Settings → Connections "Your machines" and its add-machine card, the
    focused sidebar's "Machines" section and its toggle, "Open folder on a machine" on `ListDir` and
    `SearchHomeFolders`, and the `peer://` glyphs.
13. Phones UI (§4): "Your phones" from the directory's phones, the tailnet status row, and the pairing overlay as
    an `EngineList` QR. Later, the iOS core is re-cut from zeron's `crates/client` onto `onicode-protocol`.

**Tests.**
- **Session model.** A pure `SessionModel` is the reference for `SessionHub`. Proptests draw seeded interleavings
  of commands, scripted provider events (an in-process fake behind the `resolve_binary` seam of
  `run_turn_using`), viewer drops and rejoins at random cursors, and overflows. Invariants: snapshot(c) plus
  events(c..d) equals snapshot(d) (metamorphic); every viewer converges to the host at each cursor; a repeated
  `OpId` gives the same receipt and no duplicate row; echoes resolve with no duplicate or reordered prompt; two
  answers to one approval apply once. Resume proptests over ring size, epoch change and `Resync`: no gap, no repeat.
- **Simulation.** Seeded runs on `onicode-net-sim` (LAN to DERP, loss, roam, sleep) with two or three engines,
  an app client and a phone client: forwarded watches across a duplicate-link race, a host restart mid-turn, an
  outbox delivering exactly once after the host returns, a slow phone pushed to `Resync` while the Mac stays
  live, and round-trip counts against the latency baseline.
- **Codec.** Round-trip proptests for every type, a `cargo fuzz` decoder target over a committed corpus, and the
  golden method table.
- **Smoke.** One opt-in test: the real binary, the socket, `Hello`, create, a send to the fake provider, finish.
  Nothing opens a window or prompts. A flaky case gets a seed repro, never a rerun.
