# Development

## First checkout

Install `vp` using the [root README](../../README.md#from-source). It provides the Node version pinned
in `package.json`, so no separate Node install is needed.

On Windows, clone with long paths enabled so `.repos/` checks out cleanly:

```powershell
git clone -c core.longpaths=true https://github.com/aislopware/onicode.git
```

From the repository root:

```sh
vp i
vp run dev
```

This builds the server, starts the renderer's Vite server, and opens a dev desktop app that starts its
own server and signs in to it. Prefer a container? See [Dev container](./01_devcontainer.md).

## Running dev servers

- `vp run dev` (dev desktop app with its own server), `vp run dev:server` (server alone, for mobile and
  CLI work; pair a client with `node apps/server/src/bin.ts pair --base-dir <home>`). See
  [apps/mobile/README.md](../../apps/mobile/README.md) for mobile.
- Pass flags directly after the task name (e.g. `vp run dev --home-dir /tmp/onicode-dev`).
- **State and ports:** Linked worktrees default to `<worktree>/.onicode/userdata` (outranking `ONICODE_HOME`) and seed on first start from a read-only snapshot of `~/.onicode/userdata/statev2.sqlite` when it is at most 500 MiB (`--no-seed` or `ONICODE_DEV_SEED=0` skips). Plain clones and the main checkout use `ONICODE_HOME` or `~/.onicode/dev`; a home of `~/.onicode` still selects `~/.onicode/dev`. An explicit `--home-dir` wins. Never point a dev server at `~/.onicode/userdata`. Read actual ports and `cdpPort` from `[dev-runner]` output (`ONICODE_PORT_OFFSET` / `ONICODE_DEV_INSTANCE` override).
- **Test data:** `vp run migrate-dev-db` (dev stopped) rebuilds `statev2.sqlite` from a read-only snapshot of the live database, keeping recent projects and their stopped threads and dropping scheduled tasks, pending work, auth sessions, and environment links. Raise `--projects` and `--threads-per-project` for more data. To seed a plain clone, run `vp run migrate-dev-db --base-dir <clone>/.onicode`, then `vp run dev --home-dir <clone>/.onicode`.
- **Resource monitor:** First start builds `native/resource-monitor` via Cargo when missing; without Rust, dev starts without process telemetry.
- **External volumes on macOS:** browsers downloaded into `.onicode/tools` on an external volume paint nothing until someone allows macOS's removable-volume prompt for that binary.
- **Stop on settle:** import the **Dev** action from `onicode.json` in Settings > Projects > Actions; its `stopOnSettle` stops the stack when the thread settles. A `vp run dev` started from an agent's own shell is not attached to this.
- **Startup profiling:** `vp run profile:startup` captures the first 20 s of Node processes under `.onicode/startup-profiles/<timestamp>`.

### Driving the dev window

The dev desktop window serves the Chrome DevTools Protocol on the printed `cdpPort`. Attach to the
`onicode-dev://app` page with any CDP client; the
[test-oni-app](../../.agents/skills/test-oni-app/SKILL.md) skill covers agent-driven checks.

### Replay provider

`vp run dev` registers `Replay (dev)` ([dev-replay-agent.ts](../../apps/server/scripts/dev-replay-agent.ts)), a local ACP agent that streams recorded transcripts without calling a paid model. Add scenarios under `SCENARIOS` pointing at a generic ACP fixture (`registry_transcript.ndjson`) or a file in `apps/server/scripts/dev-replay`.

## Checks

- `vp run check:changed` — CI's fast checks for your branch (format, lint, knip, affected typechecks, nearby tests, and React Compiler coverage).
- Focused iteration: `vp test run <files>`, `vp lint <files>`, `vp run --filter <pkg> typecheck`, `vp run knip:check`, `vp run lint:mobile`.
- `apps/web` uses real Chromium (`*.browser.test.{ts,tsx}`) for DOM/React tests (no jsdom) and Node for `*.test.ts`.
- Typechecking uses `effect-tsgo tsrs --noEmit` (`@effect/tsgo`), not `typescript`. On Windows, open the workspace through WSL for VS Code's `.vscode/tsdk/tsgo`.

## Desktop artifacts

Unsigned local builds write to `release/`:

```sh
vp run dist:desktop:dmg
vp run dist:desktop:linux
vp run dist:desktop:win
# Linux ARM64: vp run dist:desktop:artifact --platform linux --target AppImage --arch arm64
```

Run `vp run dist:desktop:artifact --help` for `--arch`, `--signed`, and `--keep-stage`.

### Linux AppImage prerequisites

Build on Linux (the browser-secret helper links against `libsecret`). Install Rust stable (`rustup`), C/C++ build tools, `libsecret` dev headers, `pkg-config`, ImageMagick, and Electron runtime libs (`gtk3`, `nss`, `alsa`, `gbm`):

```sh
# Ubuntu 24.04 / Debian 13 (on 22.04/Debian 12 use libgtk-3-0 and libasound2)
sudo apt-get install build-essential libsecret-1-dev pkg-config imagemagick libgtk-3-0t64 libnss3 libasound2t64 libgbm1

# Fedora
sudo dnf install gcc gcc-c++ make libsecret-devel pkgconf-pkg-config ImageMagick gtk3 nss alsa-lib mesa-libgbm

# Arch
sudo pacman -S base-devel libsecret pkgconf imagemagick gtk3 nss alsa-lib mesa
```

### macOS DMG prerequisites

Install Xcode Command Line Tools (`xcode-select --install`) and Rust. For cross-arch/universal builds:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
```

### Windows installer prerequisites

Install Rust, Python 3, and Visual Studio Build Tools (**Desktop development with C++**, Windows SDK, MSVC build tools, Spectre-mitigated libraries), then add the Rust target:

```powershell
rustup target add x86_64-pc-windows-msvc
```

WSL support also requires passing the Linux CLI archive via `--wsl-runtime`.

### Signing

On macOS, `--signed` signs with the Developer ID identity in your keychain that `CSC_NAME` names.
Releases sign through better-update; see the [release runbook](./release.md).
