# Remote desktop: Mac host, Mac and iOS clients, whole displays

Remote desktop streams whole displays from a Mac host to Mac and iOS clients. Per-window remoting is never built,
and neither are other host or client platforms: one host platform keeps capture, encode, input injection and
virtual displays on one set of APIs, and that is where the owner uses it.

The bar is Parsec measured on this Mac: about 33 ms p50 composite latency, 4:2:0 only, no FEC or NACK. Onicode
targets glass-to-glass p50 at or under 25 ms and p99 at or under 45 ms at 1080p to 1440p60 on the tailnet,
4:4:4 for text and 120 Hz. The host is `onicode-desktopd`, a Developer ID-signed launch agent that owns the
Screen Recording and Accessibility grants and its own QUIC endpoint. Video goes as low-latency HEVC with long-term
references, FEC and NACK over QUIC datagrams. The client decodes with VideoToolbox into a gpui-fast `VideoLayer`
and shows the system cursor. It ports slopty's media code once the peer transport and the gpui-fast migration have
landed.

P0 has landed as `onicode-desktop-media` (`crates/desktop-media`): slopty's pure media core ported whole with its
tests and fuzz corpora, namely packetizing with Reed–Solomon parity, reassembly with NACK and refresh, adaptive
redundancy, rate control with the chroma gate and encoder watch, clock sync, still refinement, audio copies,
temporal layers, heartbeats, cursor moves and the client's present-on-arrival pacer. Window streams, regions and
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
synthetic canvas, the window server's cursor), `onicode-desktop-input` (injection by key position, gestures, the
pasteboard, input sources) and `onicode-desktop-vdisplay` (virtual displays at the client's size). Their names and
API shapes stay slopty's, so the host and client orchestration ports line up against them. Whole displays only:
window capture, AX window picking, regions, crops, drags and the curtain are not ported. Tests that touch a device
are opt-in, one variable per kind (`ONICODE_VT_TESTS`, `ONICODE_AUDIO_LIVE`, `ONICODE_DESKTOP_LIVE`,
`ONICODE_INPUT_LIVE`, `ONICODE_VDISPLAY_LIVE`, in `onicode-desktop-testkit`), because a hosted runner's paravirtual
encoder hangs a process and the rest prompt, play sound, move the pointer or rearrange the owner's screens.

The orchestration is ported in the same pass, around one whole-display stream. `onicode-desktopd`
(`crates/desktopd`) is slopty-worker's screen pipeline rewritten smaller: capture into a newest-wins mailbox, an
encode thread, packetizing with parity, the audio lane ahead of video, the repair loop with LTR refreshes, rate,
chroma and cadence control, stripes, the engines gate, sized virtual displays and their holder (now
`onicode-desktopd --display-holder`), sound, and the drawn platform its tests run on. It is the helper app's launchd
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
picture waits on no GPUI frame, and shows the Mac's pointer as the system cursor. It opens in a window of its own from
a machine: "Open desktop" in a Machines row's context menu in the sidebar, or the button on an engine's row in Settings
→ Connections (this Mac's own included, to look at and to turn its grants on). Its chrome is the old viewer's (the
seat badge, the display and sound, curtain and stats toggles, the notices), drawn with the app's own buttons and
floating surface over a dark stage. The iPhone viewer comes back with the iOS client, last in the plan. Window, region and crop streams, zoom, drag in and out, the paste chord's clipboard push, the
clipboard following a focused desktop window (`one-machine.md`) and input-source sync are not ported; the curtain is
refused until its platform half exists.
