# Onicode

Onicode is a minimal GUI for coding agents. A server on your machine drives the agent CLIs you already use, and the desktop and mobile apps connect to it, on the same machine or from anywhere on your network.

Works with your subscriptions on Claude Code, Codex, Cursor, Grok Build, OpenCode, Google Antigravity, and fx. If they're set up on your computer, Onicode can control them.

Onicode is open source and maintained by [aislopware](https://github.com/aislopware).

## Before you start

Install and authenticate at least one provider on the machine that runs the Onicode server:

- Codex: install [Codex CLI](https://developers.openai.com/codex/cli) and run `codex login`
- Claude: install [Claude Code](https://claude.com/product/claude-code) and run `claude auth login`
- Cursor: install [Cursor CLI](https://cursor.com/cli) and run `agent login`
- Grok Build: install [Grok Build CLI](https://x.ai/cli) and run `grok login`
- OpenCode: install [OpenCode](https://opencode.ai) and run `opencode auth login`
- Antigravity: enable it in Settings, then use **Install Antigravity** and **Sign in with Google**. No CLI is required.
- fx: install [fx](https://fx.sh) and run `fx login` ([setup guide](./docs/user/providers-fx.md))

## Install

### Command line

```bash
npm install -g onicode
```

Then run `onicode` to start the server and print pairing details for the desktop and mobile apps. `onicode service install` keeps it running in the background, and `onicode --help` has the full reference.

### Desktop and mobile

The desktop app bundles the server. On a Mac with Apple silicon, install it with [Homebrew](https://brew.sh):

```bash
brew install --cask aislopware/tap/onicode
```

or [download it](https://updates.jmango360.dev/feeds/59625ece-aea5-49b4-8978-1f045f907d92/macos/latest/download) and move **Onicode** to Applications. Desktop and mobile releases are published through Onicode's own update feeds, and installed apps update themselves from there. See [Install and first run](./docs/user/install.md) and [Remote access](./docs/user/remote-access.md) to connect a phone or another machine.

### From source

```bash
git clone https://github.com/aislopware/onicode.git
cd onicode
vp i
vp run dev
```

Onicode uses [Vite+](https://viteplus.dev/guide/), so install the global `vp` command first:

```bash
# macOS / Linux
curl -fsSL https://vite.plus | bash
# Windows (PowerShell)
irm https://vite.plus/ps1 | iex
```

`vp run dev` starts the server and web app with state in the checkout's own `.onicode` directory. [Development](./docs/operations/development.md) covers ports, test data and the desktop and mobile apps. [docs/internals/overview.md](./docs/internals/overview.md) explains the architecture.

## Documentation

Docs live in [docs/](./docs).

- [FAQ](./docs/user/faq.md)
- [Install and first run](./docs/user/install.md)
- [Permission modes](./docs/user/permission-modes.md)
- [Keyboard shortcuts](./docs/user/keybindings.md)
- [Project settings](./docs/user/project-settings.md)
- [Appearance preferences](./docs/user/appearance.md)
- [Remote access from a phone or another machine](./docs/user/remote-access.md)
- [Keeping app and server in sync](./docs/user/updating.md)
- [Source control integrations](./docs/user/source-control.md)
- Multiple accounts: [Codex](./docs/user/providers-codex.md) · [Claude](./docs/user/providers-claude.md)
- [Run Onicode as a background service](./docs/user/background-service.md)

## No hosted services

Onicode has no cloud relay, web app or telemetry. Clients reach a server on the same machine, over your local network or through Tailscale, and releases ship through Onicode's own update feeds.

## Contributing

Read [CONTRIBUTING.md](./CONTRIBUTING.md) before reporting a bug or opening a PR. Report security issues privately as described in [SECURITY.md](./.github/SECURITY.md).

## License

[MIT](./LICENSE).
