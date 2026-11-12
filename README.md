# Onicode

Onicode is a fork of [T3 Code](https://github.com/pingdotgg/t3code): a desktop and mobile GUI for the coding agents you already pay for. A server on your machine drives Claude Code, Codex, Cursor, Grok Build, OpenCode, Antigravity, fx and other agent CLIs, and the apps connect to it from the same machine, your local network or your tailnet.

We keep porting T3 Code's fixes and features, one reviewed commit at a time, and go our own way on the points below.

## What's different from T3 Code

- **No account, no relay, no telemetry.** T3 Connect, the hosted web app and analytics are gone. The apps reach your machine directly, over your local network or Tailscale.
- **Tailscale sign-in.** Pick a machine from your tailnet and connect without a pairing code. A host can also accept tailnet connections only.
- **Remote desktop.** See and control a machine's whole screen from another desktop or your phone, peer to peer, for the moments an agent's work needs a GUI.
- **More of Claude Code.** Claude threads open `/context`, `/mcp`, `/status`, `/agents`, `/skills`, `/memory`, `/hooks`, `/permissions` and `/plugin`, on desktop and mobile.
- **Faster project picking.** Add project, the command palette and `@` mentions find folders through zoxide and fzf.
- **A snappier desktop app.** Quicker cold starts and no window flashes on high-refresh Macs.
- **One release channel.** No nightly track. The apps update themselves from Onicode's own feeds.

## From source

Install [Vite+](https://viteplus.dev/guide/) for the `vp` command, then:

```bash
git clone https://github.com/aislopware/onicode.git
cd onicode
vp i
vp run dev
```

See [Development](./docs/operations/development.md) for the rest.

---

[Docs](./docs) · [Contributing](./CONTRIBUTING.md) · [Security](./.github/SECURITY.md) · [MIT license](./LICENSE)
