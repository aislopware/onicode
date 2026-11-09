# Install Onicode

Onicode runs coding agents on your computer and lets you control them from its
desktop or mobile app. Set up the machine where the agents will work first.

## Requirements

You need an installed, authenticated provider before starting a thread. You can
launch Onicode and configure providers afterwards.

## Command line

```bash
curl -fsSL https://raw.githubusercontent.com/aislopware/onicode/main/scripts/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/aislopware/onicode/main/scripts/install.ps1 | iex
```

This installs the `onicode` binary to `~/.local/bin`. If that folder is not on your
`PATH` yet, the installer says so and, for zsh, bash on Linux, and fish, prints
a command to add it. On Windows, `onicode` works in the same PowerShell window right
away; add the folder to your `PATH` for new windows. If a `onicode` that the
installer or the desktop app did not create is already there, it leaves that
one alone and stops. Set `ONICODE_VERSION` to pin an exact version.

| Task                                             | Command                                                                      |
| ------------------------------------------------ | ---------------------------------------------------------------------------- |
| Start the server and print pairing details       | `onicode`                                                                    |
| Same, without adding this folder as a project    | `onicode serve`                                                              |
| Use it from your other devices                   | `onicode up` ([details](./remote-access.md))                                 |
| Keep it running in the background (macOS, Linux) | `onicode service install` ([details](./background-service.md))               |
| Move to the newest release                       | `onicode update`                                                             |
| Reclaim database space                           | `onicode doctor` ([details](./background-service.md#reclaim-database-space)) |
| Remove it again                                  | `onicode uninstall`                                                          |

Run `onicode help` or `onicode --help` for the full reference. To start in a new working
directory, use an explicit path such as `onicode ./my-project`. A bare directory name
is accepted only if it already exists.

If `onicode` or `onicode start` reports an already running server, connect to that server
instead. Stop it before starting a replacement, or use a different `--base-dir`
for an independent server.

To try Onicode once without installing it, run `npx onicode@latest` instead (needs
Node.js for `npx`). It offers to install the `onicode` command first. If you say no,
it asks again at the next minor release, and `npx onicode install` installs it later.
`npx onicode` always runs the version npm picked, even when a `onicode` is installed.

### Intel Macs

There is no `onicode` executable or desktop app for Intel Macs. To run a server there, build it from source with `vp`, which provides the Node version it needs
([Install vp](https://github.com/aislopware/onicode#install-vp)):

```bash
git clone https://github.com/aislopware/onicode
cd onicode && vp i && vp run build:desktop
node apps/server/dist/bin.mjs
```

`onicode update` and the background service do not apply to a server run this way;
update it with `git pull` and a rebuild.

## Desktop app

The desktop app is available for macOS on Apple silicon and for Linux on x86_64.
Download the newest release:

| Platform       | Download                                                                                                          |
| -------------- | ----------------------------------------------------------------------------------------------------------------- |
| macOS          | [Onicode for Mac](https://updates.jmango360.dev/feeds/59625ece-aea5-49b4-8978-1f045f907d92/macos/latest/download) |
| Linux AppImage | [AppImage](https://updates.jmango360.dev/feeds/59625ece-aea5-49b4-8978-1f045f907d92/linux/latest/download)        |
| Debian, Ubuntu | [.deb](https://updates.jmango360.dev/feeds/59625ece-aea5-49b4-8978-1f045f907d92/linux/latest/download?format=deb) |

On macOS, unzip the download and move **Onicode** to Applications, or install it
with [Homebrew](https://brew.sh):

```bash
brew install --cask aislopware/tap/onicode
```

The app updates itself from then on, so `brew upgrade` skips it unless you pass
`--greedy`. If you used the earlier native Onicode, it updates to this app on its own.

### Linux

To launch the AppImage, open a terminal in its download folder. Replace
`Onicode.AppImage` below with the exact filename you downloaded:

```bash
chmod +x ./Onicode.AppImage
./Onicode.AppImage
```

Install the `.deb` with `sudo apt install ./Onicode-*.deb`. It updates itself like
the AppImage and asks for your password to install each update. If your desktop
has no password prompt, the update fails; download the new `.deb` and install it
the same way.

### The `onicode` command

The desktop app includes the `onicode` command-line tool. A new install sets it up
on first launch; after an update, or any time, choose **Install** next to
**onicode command** in **Settings → About**. On macOS and Linux it adds a `onicode` link
in `~/.local/bin` (or `~/bin`). Open a new terminal afterwards. If the folder is not on your
`PATH`, **Add to PATH** on the Welcome page or in **Settings → About** adds one
line to your shell's startup file. **Remove** takes it off, and the app does not
add it again. If you already have another `onicode`, it stays as it is.

### Open a project from a terminal

With the desktop app already running on the same machine:

```bash
onicode app
```

This opens a new thread for the current directory, adding the project if needed.
Pass a path, such as `onicode app ../my-project`, to open another directory. It requires
the desktop app, so a standalone server or an SSH session is not enough. If the
command cannot reach the app, start or update the desktop app and try again.

## Mobile app

The iOS and Android apps come through Onicode's own update feed and receive updates from it.
The phone connects to a server on another machine. Follow
[remote access](./remote-access.md) to link it with a pairing URL.

If the app crashes during launch, open Settings → Diagnostics on the next launch
that succeeds. It lists startup crashes from the last 7 days with the error and
component stack that platform crash reports leave out. Copy the report and paste it
into a GitHub issue. Error messages can quote values from the app, so read it over
before sharing.

## Providers

Open **Settings → Providers** in the web or desktop app, select the environment,
and enable the provider you want. Installation, login, and configuration belong
to that environment's machine, even when you connect from a phone or another
computer.

| Provider    | Install and authenticate                                                                                                                                  |
| ----------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Codex       | [Connect with ChatGPT](./providers-codex.md#connect-with-chatgpt), or install [Codex CLI](https://developers.openai.com/codex/cli) and run `codex login`. |
| Claude      | Install [Claude Code](https://claude.com/product/claude-code), then run `claude auth login`.                                                              |
| Cursor      | Install [Cursor CLI](https://cursor.com/cli), then run `agent login`.                                                                                     |
| Grok Build  | Install [Grok Build CLI](https://x.ai/cli), then run `grok login`.                                                                                        |
| Kiro        | Install [Kiro CLI](https://kiro.dev/docs/cli/), then run `kiro-cli login`.                                                                                |
| OpenCode    | Install [OpenCode](https://opencode.ai), then run `opencode auth login`.                                                                                  |
| Antigravity | Install and sign in with Google from Onicode's provider settings.                                                                                         |
| Pi          | Install [Pi](./providers-pi.md#set-up-pi) with Node.js 22.19 or newer, then run `pi` and `/login`.                                                        |
| Muse Code   | Install [Muse Code](https://dev.meta.ai/docs/muse-code) on the server, run `muse login`, then enable it in Settings → Providers.                          |
| fx          | Install [fx](https://fx.sh) with `curl -fsSL https://fx.sh/setup.sh \| bash`, then run `fx login`.                                                        |

Provider CLIs must be on the server's `PATH`. If Onicode cannot find one, set its
**Binary path** in provider settings, especially when using a version manager.
Cursor's executable is `cursor-agent`, although its login command is
`agent login`. Codex connected through ChatGPT and Antigravity can use their
managed runtimes without a `PATH` entry.

Onicode warns when a provider version has known compatibility problems with your
release. Check **Settings → Providers** on that environment for the recommended
version or range. When its package manager supports installing a specific version,
you can install the recommendation there. Otherwise use the provider's installer
on the environment's machine. An unlisted version is unverified.

When a provider CLI is behind its latest release, its provider card shows the
available version. **Update now** runs the installer that owns the CLI
(Homebrew, mise, or a global npm, pnpm, Yarn, Bun, Volta, or Vite+ install), or
the CLI's own update command when Onicode cannot tell. Onicode runs `mise upgrade`
only for a tool your mise config requests as `latest` when the provider's binary
path is a mise shim or under the tool's `installs/<tool>/latest` link; update a
pinned version, or a path into one version's directory, through mise yourself.
Cursor and Antigravity update with Onicode. Homebrew and Chocolatey installs
compare against the version their package manager offers, which can trail the
npm release; update Claude installed with Chocolatey through Chocolatey.

Add another provider instance for a separate account or configuration. Each
instance can have its own environment variables, such as API keys or a custom
base URL. Mark secret values as sensitive; after saving, Onicode does not display
their original values.

For provider-specific setup and accounts, see [Codex](./providers-codex.md),
[Claude](./providers-claude.md), [OpenCode](./providers-opencode.md),
[Antigravity](./providers-antigravity.md), [Kiro](./providers-kiro.md),
[Pi](./providers-pi.md), [Muse Code](./providers-muse.md), and [fx](./providers-fx.md).

## Next steps

- [Working with threads](./thread-sidebar.md): start tasks and organize parallel work.
- [Permission modes](./permission-modes.md): choose when agents ask before acting.
- [Remote access](./remote-access.md): connect from another device.
- [Running in the background](./background-service.md): keep a Linux or macOS host available.
- [Updating Onicode](./updating.md): update the app and connected servers.
- [Managed policy](./managed-policy.md): configure Onicode for everyone on a machine.
