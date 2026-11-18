# Browser use

Agents browse with [obu](https://github.com/aislopware/oni-browser-use), our
fork of a browser driver written for agents. Onicode wraps none of its tools:
each provider session launches `obu mcp` over stdio as the MCP server
`browser`, and obu's own tool list and instructions reach the model unchanged.
[`BrowserUseService`](../../apps/server/src/browserUse/BrowserUseService.ts)
builds that launch for one of three browsers.

## The panel keeps its engine

The Browser panel's pages stay Playwright pages. Frame streaming, human
takeover, recording, downloads, profiles and the desktop webview relay are all
built on them, and obu offers none of those. What obu replaces is the agent's
half: reading a page and acting on it.

So obu does not launch a browser for the panel. It attaches to one, and
[`PreviewCdpBridge`](../../apps/server/src/preview/PreviewCdpBridge.ts) is the
debug endpoint it attaches to: a loopback listener that presents one thread's
panel tabs as the targets of a browser. A second CDP session on the tab's page
carries obu's commands, beside the session Playwright already holds.

## What the bridge decides

- **Whose tabs.** An endpoint lives under a secret path issued for one MCP
  credential scope. It lists the tabs that scope's agent may act on and no
  others, so `Target.getTargets` cannot show another thread's pages. A new
  provider session gets a new path, and the old one stops answering.
- **Target ids are derived**, from the thread and tab id. The bridge keeps no
  table, and obu names files after the id.
- **Tabs open and close through `PreviewAutomationBroker`**, the same path as
  `preview_open`. A tab created over raw CDP would be one the panel never
  shows. obu's blank start tab maps to the tab the agent already owns.
- **Control is checked per command, not queued.** A driver pipelines commands
  and waits on events between them; putting them in `SessionControl`'s queue
  can deadlock. Each command is refused while a human holds the tab.
- **A pointer walk reaches the page as one move.** A driver walks the pointer
  to its target in many small moves, and Chromium answers each on the page's
  next frame. A tab nobody watches paints too rarely for that, so the bridge
  forwards only where the pointer came to rest. A drag's moves all pass.
- **A sized capture is the page's, not the panel's.** obu sizes a page for one
  `vision` capture with `Emulation.setDeviceMetricsOverride` and clears it
  after. A desktop page takes the override itself. Playwright holds a headless
  page's metrics and would not see another session's override, so that page is
  resized through its viewport and keeps the browser's own pixel ratio. The
  tab's viewport setting never changes, and
  [`ServerBrowser`](../../apps/server/src/preview/ServerBrowser.ts) restores the
  page when the driver clears the override or detaches, even while a human
  holds the tab. Sets, clears and a desktop tab's captures run one at a time in
  the order they arrived: obu pipelines them, and a clear that overtook its set
  or landed between two tiles would leave the page or the image at the wrong
  size. A size change the page does not answer within five seconds gives up
  its turn. The session detaches only after that restore, since a clear sent
  to a detached session reaches no page.
- **A desktop tab is captured in tiles.** Chromium paints only about as much of
  a webview as the app window could show, and repeats the page across any
  larger capture. The server captures a larger clip as tiles within the painted
  size and joins them ([`pngTiles`](../../apps/server/src/preview/pngTiles.ts)).
  Tiles step by CSS pixels that are whole output pixels too, so they meet
  without a seam at a fractional ratio. A clip is refused before any tile is
  taken when no such step exists, when the joined image, held whole in memory,
  would pass 40 megapixels (obu's own cap, so a page it cut to fit is never
  refused here), or when it needs more than 256 tiles. The joined
  image is as large as its tiles came back, so Chromium's rounding of a clip's
  last part pixel is kept. `fromSurface: false` would
  capture the app window around the tab, so it is dropped. A visible tab the
  panel shows scaled down captures soft.
- **A hidden window paints nothing.** With the app window minimized or fully
  covered, a desktop tab answers no capture, and no CDP command wakes it. An
  agent works with nobody at the desktop, so its new tab is checked once: when
  the desktop page answers no capture, the server runs the tab headless, as it
  does with no desktop at all. A desktop tab whose window is hidden later
  fails a capture after three seconds with a message that tells the agent to
  open the page in a new tab, instead of hanging until obu gives up on the
  browser. Chromium saves the page's emulation when a clipped capture begins
  and puts it back when the capture is answered, however late. So until such
  a capture has answered and the driver's current size has been applied again,
  the tab takes no other capture. A clear that arrives while such a capture is
  out is not sent: Chromium would drop the size to go back to and leave the
  webview at the capture's. The repair sends it once the capture has answered,
  and the driver is told the page is still sized when that takes more than
  five seconds.
- **A capture must fit one CDP message.** obu closes its connection on a
  message above 224 MiB, and a capture travels as base64. obu caps what it
  asks for, and the server refuses to send a larger reply instead of ending
  the driver's session.
- **Page-scoped domains only.** `Browser`, `Storage`, `SystemInfo` and the
  rest of `Target` reach other tabs, the profile or the host, and are refused.
  The table is [`previewCdpPolicy`](../../apps/server/src/preview/previewCdpPolicy.ts).

`ProviderSessionManager` is built below the layer that owns `ServerBrowser`
and the broker, so the bridge takes them late through `bind`
(`PreviewCdpBridge.layerBackend`). Both layer levels must provide the same
`PreviewCdpBridge.layer` reference, or each gets a bridge of its own.

## One way to work a page

While obu drives the panel, the session's credential carries the
`preview-driver` capability, and `preview_navigate`, `preview_snapshot`,
`preview_act`, `preview_evaluate` and `preview_wait_for` are left out of its
tool list. Two ways to read and act on one page make an agent mix their
element refs. The capability is part of the credential, so changing the target
issues a new one.

Onicode's instructions tell an agent that the `browser` tools are its browser
and that it starts none of its own. Which browser that is comes from obu:
`OBU_HOST_NOTE` adds one line to obu's MCP instructions and to its first
result. Both texts are model-facing; `evals/mcp-tool-use` replays them with
obu's captured tool list (`cases/browser-driver.ndjson`).

## One home, one session per thread

Every thread shares `<state>/browser-use` as `OBU_HOME`, so logins, site
knowledge and the fingerprint carry over, and `OBU_SESSION` keeps each
thread's tab record apart inside it. Archiving or deleting a thread closes its
panel tabs and runs `obu stop --close-tabs` for its session
([`BrowserUseSessionReactor`](../../apps/server/src/browserUse/BrowserUseSessionReactor.ts)).

obu nudges the pointer every few seconds while its agent is idle, for as long as
its process lives. A pointer move with no button down therefore counts as no use
of a tab: it refreshes no idle clock on the server and holds no focus on the
desktop, or a tab an agent once touched would never rest.

The release is pinned in
[`BrowserUseDriver`](../../apps/server/src/browserUse/BrowserUseDriver.ts).
Bump the version, sizes and hashes together. The panel target depends on fork
features (`OBU_SESSION`, `OBU_ENDPOINT_PATH`, `OBU_HOST_NOTE`), so an upstream
build cannot stand in for it.

## Adding a provider

An adapter gets the launch as `browserMcpServer` on its
[`McpProviderSessionConfig`](../../packages/provider-core/src/server/mcpSession.ts)
and registers it under `BROWSER_MCP_SERVER_NAME`; `stdioMcpServers` returns it
together with the computer driver.
