# Remote desktop: Mac host, Mac and iOS clients, whole displays

Remote desktop streams whole displays from a Mac host to Mac and iOS clients. Per-window remoting is never built,
and neither are other host or client platforms: one host platform keeps capture, encode, input injection and
virtual displays on one set of APIs, and that is where the owner uses it.

## Scope: what people use, finished to a premium bar

Ruled 2026-11-07 by the owner: build the features people need and use often, delete the rarely used ones, and
make each one that stays feel like sitting at the other Mac. No vendor publishes usage telemetry, so the ruling
reads public trackers by votes (Moonlight, RustDesk), vendor defaults and priorities (Jump Desktop, Apple Screen
Sharing's High Performance mode, Parsec, Screens 5) and reviews of Mac to Mac use (study of 2026-11-07, sources
in `.research/remote-desktop-usage.md`).

Kept and finished:
- Whole displays, one picked at a time among the Mac's displays; HEVC with long-term references, two stripes,
  parity and NACK, rate and chroma control, 4:4:4 for text, the present-on-arrival pacer.
- **A display made at the viewer's size**, Retina, and resized with the window: the most asked-for picture
  feature (Moonlight's second issue, Jump's second performance tip, Apple's Dynamic Resolution). The made display
  is kept ten minutes after the last visit, windows in place. Opening a physical display stays one click away.
- **The Mac's shortcuts while the window has focus and holds control**: ⌘Tab, ⌘Space, Mission Control and the
  rest reach the Mac, as Jump does by default for a Mac host. ⌘⌥⎋ and ⌃⌘Q stay local.
- **The clipboard follows the window**: while a desktop window has focus, text, rich text, images and files
  copied on either side paste on the other, sent when copied (up to 16 MiB), never sending items password
  managers mark concealed or transient (onicode's own clipboard design, `one-machine.md`, restored for the
  desktop session).
- **Sessions that stay up**: redial after a drop without a second capture, the host kept awake while a viewer
  is joined, and the locked or login-window state, a refused open and refused input said in the window.
- Sound from the Mac (a toggle; never on for this Mac's own desktop), the Mac's cursor as the system cursor,
  trackpad scroll with momentum and gestures.
- Many viewers, one controller: watching an agent's Mac without taking it (observe-only) is a core use.
- **The curtain**: the Mac's physical displays go dark while a controller holds it, by shield windows that the
  capture excludes, as Apple's High Performance mode blanks the host.
- The stats overlay, with the round trip, behind its toggle: what diagnoses a slow path.

Deleted, rarely used: keyboard input-source sync and the host's layout table (⌘ shortcuts fall back to the key
position, which is right for the same layout on both Macs), typed-text injection, the paste chord (the clipboard
replaces it), focused-field detection, still screenshots, the H.264 fallback (every Mac host encodes HEVC in
hardware). Never built: microphone, recording, window, region and
crop streams, zoom, wake from sleep, chat and share links, pen pressure, HDR, gamepads, manual frame-rate and
bitrate controls (the rate control adapts on its own). Dragging files into the window waits: files move through
the clipboard.

Simplified 2026-11-07 (error-prone and complex for what they gave):
- The display holder process: made displays live in desktopd and linger ten minutes; a desktopd restart may move
  their windows. A second process with its own socket, wire and restore paths outweighed a rare restart.
- The engines' give-way between streams: each stream's own encoder watch steps its rate down when the engines
  are full. Favouring a focused stream needed a focus message, holds and settle timers across streams.
- Temporal layers: parity, NACK and LTR refreshes repair loss. Layers changed the wire, the reassembler and the
  encoder's rate split for a gain only under heavy loss.
- Sound's pitch-period concealment, adaptive jitter target, loss-driven copies and video metering lane: a 5 ms
  fade to silence on loss and one fixed 40 ms jitter target (raised only past a device render's own buffer).
  Each was a controller with its own tuning; on a tailnet the fixed depth costs a few milliseconds.
- The clipboard's lazy pasteboard promises: copies travel when they are made, up to 16 MiB. A promise blocked
  inside AppKit's callback, pumping the main run loop for up to 3 s or 120 s while it waited on the other Mac.
- The stripes' timing gate: two stripes from a fixed size (`stripes::STRIPED_FROM`, about 4.76 million pixels).
  A benchmark at stream open with spend holds rebuilt sessions and raced the first keyframe.

The bar is Parsec measured on this Mac: about 33 ms p50 composite latency, 4:2:0 only, no FEC or NACK. Onicode
targets glass-to-glass p50 at or under 25 ms and p99 at or under 45 ms at 1080p to 1440p60 on the tailnet,
4:4:4 for text and 120 Hz. The host is `onicode-desktopd`, a Developer ID-signed launch agent that owns the
Screen Recording and Accessibility grants and its own QUIC endpoint. Video goes as low-latency HEVC with long-term
references, FEC and NACK over QUIC datagrams. The client decodes with VideoToolbox into a gpui-fast `VideoLayer`
and shows the system cursor. It ports slopty's media code once the peer transport and the gpui-fast migration have
landed.

P0 has landed as `onicode-desktop-media` (`crates/desktop-media`): slopty's pure media core ported whole with its
tests and fuzz corpora, namely packetizing with Reed–Solomon parity, reassembly with NACK and refresh, adaptive
redundancy, rate control with the chroma gate and encoder watch, clock sync, still refinement, one Opus packet a
datagram for sound, heartbeats, cursor moves and the client's present-on-arrival pacer. Window streams, regions and
crops are gone: one stream is one display, or one stripe of it. Its wire lives in `wire`: a 16-byte media header
on datagram channel 2 (`MEDIA`, host to client) and client feedback on channel 3 (`FEEDBACK`), each after the
channel byte `onicode-net` adds, sized so a datagram fits a 1232-byte tailnet packet whatever the connection id.
The wire's canonical encoding is pinned byte for byte in a test, so no layout changes by accident: a deliberate
change updates the pinned bytes and bumps `wire::VERSION` in the same commit.

The libraries are slopty's, ported whole rather than phase by phase, since slopty measured each choice
(its `docs/MEASUREMENTS.md` and `docs/decisions/{video,input,transport}.md`, which the ported comments cite):
`onicode-desktop-proto` (control types: displays, stream setup, screen input by key position, events, keys, the
clipboard), `onicode-desktop-codec` (VideoToolbox low-latency HEVC with LTR and padded sizes, the decoder, two-stripe
coding, the SPS conformance window, Opus), `onicode-desktop-capture` (ScreenCaptureKit display streams, the
synthetic canvas, the window server's cursor, the curtain's shield windows that the capture filter leaves out, and
the keep-awake power assertion), `onicode-desktop-input` (injection by key position, the pointer and gestures) and
`onicode-desktop-vdisplay` (virtual displays at the client's size). Their names and API shapes stay slopty's, so the
host and client orchestration ports line up against them. Whole displays only: window capture, AX window picking,
regions, crops and drags are not ported, and the rarely used features the Scope deletes went with their tests,
protocol variants and dependencies (the session wire, `onicode_desktop_link::WIRE`, is 4). Tests that touch a device
are opt-in, one variable per kind (`ONICODE_VT_TESTS`, `ONICODE_AUDIO_LIVE`, `ONICODE_DESKTOP_LIVE`,
`ONICODE_INPUT_LIVE`, `ONICODE_VDISPLAY_LIVE`, in `onicode-desktop-testkit`), because a hosted runner's paravirtual
encoder hangs a process and the rest prompt, play sound, move the pointer or rearrange the owner's screens.

The orchestration is ported in the same pass, around one whole-display stream. `onicode-desktopd`
(`crates/desktopd`) is slopty-worker's screen pipeline rewritten smaller: capture into a newest-wins mailbox, an
encode thread, packetizing with parity, the repair loop with LTR refreshes, rate,
chroma and cadence control, two stripes from a fixed size (`stripes::STRIPED_FROM`), sized virtual displays made in
desktopd's own process and kept ten minutes after their last stream, sound (10 ms Opus packets, one a datagram, which
the viewer plays at a fixed 40 ms depth and fades to silence over a lost one), the curtain (drawn only for the controller, lifted when it lets go of
the controls, leaves or closes, and told to every viewer), the Mac kept awake by one IOKit power assertion while any
viewer is joined, and the drawn platform its tests run on. It is the helper app's launchd
agent with its own QUIC endpoint on the tailnet at port 7434, admitted by whois like any peer. It reads its own grants
(`Grants::fresh`, never a prompt and no ScreenCaptureKit call before Screen Recording reads true), refuses sessions
naming the panes that are off, and restarts itself for launchd once a guide it was told about sees them on. The engine
captures nothing: it brokers. `OpenDesktop`, `DesktopGrants` and `DesktopOpenGuide` are forwardable RPCs that the Mac's
engine answers by asking desktopd on a Unix socket in its data directory (the protocol's Desktop group, served by
`crates/engine/src/desktop.rs`), starting it with `launchctl kickstart` on the label made from the app's bundle id.
The first desktop asked of a Mac registers the agent with `SMAppService` from the engine, whose main bundle is the
app's (no onboarding step does it any more); an agent the person turned off in Login Items stays off and the engine
opens that pane for them. A client then dials desktopd itself, so no frame takes a second hop and a desktopd restart never touches agent sessions.
desktopd keeps its data where the app and its engine do (`~/Library/Application Support/Onicode`, `Onicode
Development` for a development build, or `$ONICODE_DATA_DIR`), so the engine finds its socket and desktopd's hello
carries the engine's `device-id`. The session around slopty's screen
protocol is `onicode-desktop-link`: a join and a welcome, the grants, numbered input that also goes as a datagram
copy on channel 4 and is applied once and in order whichever copy arrives first, and the seat. The seat is what
onicode adds to slopty's multi-client ruling, which keeps one capture and encode per viewer so each client adapts to its
own path: any number of viewers, one controller, whose input alone reaches the Mac; the last to ask takes the controls,
and taking them needs the owner's tailnet grant. `onicode-desktop-client` (`crates/desktop-client`) is
slopty-client's screen path for both apps: routing with a backlog for datagrams that beat `Opened`, a reassembler and
decode thread per stripe lane, the stitch, reports and clock probes, sound, the letterbox fit, and redial. The macOS
view (`crates/app/src/desktop`) presents each decoded picture on a gpui-fast `VideoLayer` from the decode thread, so a
picture waits on no GPUI frame, and shows the Mac's pointer as the system cursor. It shows a display the Mac makes at
the window's content size in pixels at its backing scale, first in the display picker and opened by default, and
resizes it with the window once the window holds still for 250 ms; the Mac's physical displays follow it in the picker,
and a Mac that cannot make one streams its main display and the window says so. A redial opens nothing again: the
session reopens what it had, so each display is asked once. While the window is key and holds the controls it turns
the system's shortcuts off for itself (HIToolbox's symbolic hot-key mode, which needs Accessibility; the window says
when it is off) and a local event monitor sends them to the Mac. It opens in a window of its own from
a machine: "Open desktop" in a Machines row's context menu in the sidebar, or the button on an engine's row in Settings
→ Connections (this Mac's own included, to look at and to turn its grants on). Its chrome is the old viewer's (the
seat badge, the display and sound, curtain and stats toggles, the notices), drawn with the app's own buttons and
floating surface over a dark stage: a locked Mac or one at the login window and a refused open are notices, and the
display made instead of the one asked for, refused input and a refused curtain are lines under the controls. The
stats overlay carries the round trip of the clock probes. "Turn on" for this Mac's own grants opens the guide here and
tells desktopd through the engine too, so it restarts once the grant is on. The iPhone viewer comes back with the iOS
client, last in the plan.

As built, the clipboard follows the window. `onicode-clipboard` (`crates/clipboard`) is onicode's clipboard design
from `one-machine.md` restored for the desktop session: a `Hub` per Mac watches one pasteboard's `changeCount`, and
a `Session` per desktop link follows the Mac at the other end while the viewer says its window is key and holds
focus (`Clip::Focus`). Clips ride the control stream as `Up::Clip` and `Down::Clip` (`desktop-proto`'s
`clipboard` module): an offer carries the origin, `seq`, `at` and each item's types with sizes, and items up to
16 KiB inline. The origin reads the whole copy when it is made and holds its bytes; the other end fetches what
did not ride inline as soon as the offer lands, on streams of its own (`clip/<seq>/<item>/<type>` for bytes,
`clipfiles/<seq>/<item>` for files), and writes the copy whole once all of it is there, within 30 s
(`FETCH_DEADLINE`) and unless a newer copy came meanwhile, so a paste never waits on the link. The caps:
`SEND_MAX`, 16 MiB of bytes a copy (a type past what is left stays home and the app's next-ranked type may still
go, so a screenshot's TIFF stays while its PNG travels), and `FILES_MAX`, 16 MiB of regular files in all (bigger
files and folders keep their items home). A fetch for a copy the origin no longer holds is reset with the
`REFUSED` code. Writes carry the origin type and
`currentHostOnly` and remember their `changeCount`, so nothing echoes; the newest `(at, origin, seq)` wins on both
Macs; concealed and transient copies never leave (a marker on any item keeps the whole copy home) and
auto-generated ones stay unless opted in. No history is kept: a cache folder holds the newest files only.
desktopd (`crates/desktopd/src/clipboard.rs`, `session.rs`) gives a clipboard session to owner links only and
watches the general pasteboard only while a focused viewer follows it. It reports its Paste from Other Apps
setting every 2 s (`Clip::Access`): until it is Allow, desktopd reads nothing, so nothing prompts, and copies on
the host stay there while the viewer's still land. The window shows a notice with the guide to that pane
(`Pane::PasteFromOtherApps`) until it is Allow; opening the guide raises the one prompt that lists desktopd in that
pane when it was never asked. The viewer side is `crates/app/src/desktop/clipboard.rs` (one hub
on the app's general pasteboard, attached to each session that shows another Mac) and `onicode-desktop-client`,
which holds the session and its fetch streams across redials. Tests run both ends on in-memory boards over a fake
link on tokio's paused clock (`crates/clipboard/tests`); the `NSPasteboard` tests are opt-in
(`ONICODE_PASTEBOARD_LIVE`) on uniquely named pasteboards.
