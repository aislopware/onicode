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

The model learns which browser it holds from obu, not from Onicode's
instructions: `OBU_HOST_NOTE` adds one line to obu's MCP instructions and to
its first result. That text is model-facing; treat a change to it like any
other instruction change.

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
