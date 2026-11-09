# Dev container

> For maintainers. Using Onicode? See [docs/user](../user/).

`.devcontainer/` gives you a ready-to-code Linux environment matching CI: Ubuntu 24.04, Node 26, pnpm, Rust stable, the global `vp` CLI, and the GitHub CLI. Open the repo in VS Code and "Reopen in Container", or create a GitHub Codespace. Dependency install (`vp i`), the Electron exec-bit repair, and the Vite dep-cache warmup all run automatically before you attach.

## What works in the container

- The server: `vp run dev:server`, then pair a mobile app or a desktop app on the host through the forwarded server port with `node apps/server/src/bin.ts pair --base-dir .onicode`.
- Everything the Linux CI jobs run: focused `vp test run <files>`, `vp lint <files>`, package typechecks, `vp run build:desktop`, and the resource-monitor cargo build and tests. (`vpr` is not on PATH here; the curl installer only shims `vp`. Use `vp run <script>` or `node_modules/.bin/vpr` after install.)

## State and safety

`ONICODE_HOME` points at the workspace's gitignored `.onicode`, so all runtime state stays inside the container workspace, mirroring the worktree default. There is no live install to damage inside a container, but the test-data rule from AGENTS.md still holds: copy data in, never point at shared state.

## Caching

Two named volumes keep rebuilds fast and installs off the slow macOS/Windows bind mount: the pnpm store (shared across checkouts, mounted where `vp i` keeps it) and root `node_modules` (per-container, which covers the whole `.pnpm` virtual store since workspace packages just symlink into it). Deleting a container and recreating it reuses both, so a rebuild's `vp i` is seconds, not minutes. The host sees an empty `node_modules`; run host-side tooling inside the container.

## Out of scope

- `vp run dev` opens a desktop window, so it is host-only. Building and verifying the desktop bundle works fine in the container (CI does exactly that, headless).
- Mobile native builds are host-only (Xcode for iOS, Android SDK for Android). Typecheck, lint, and the mobile static checks run fine.

## Prebuilds

Container creation from scratch does a full `vp i` plus toolchain installs, which is worth prebuilding. Codespaces prebuilds are configured in repo settings, not files, and pick this config up as-is: the heavy steps live in `onCreateCommand` and `updateContentCommand`, which prebuilds bake in. Restrict prebuilds to one region and one retained version; storage bills per region per version. Note that prebuild snapshots exclude the caching volumes, so a prebuild-first workflow may prefer dropping the mounts. Outside Codespaces, the Dev Container CLI can push a prebuilt image:

```bash
devcontainer build --workspace-folder . --push true --image-name <registry>/onicode-devcontainer:latest
```
