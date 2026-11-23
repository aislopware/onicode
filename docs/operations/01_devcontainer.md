# Dev container

`.devcontainer/` provides a Linux development environment matching CI, with `vp`, Rust, and the GitHub CLI installed and workspace dependencies prepared before you attach. Open the repo in VS Code ("Reopen in Container") or create a GitHub Codespace.

## What works

- The server: `vp run dev:server`, then pair a mobile app or a desktop app on the host through the forwarded server port with `node apps/server/src/bin.ts pair --base-dir .onicode`.
- Linux CI checks: focused `vp test run <files>`, `vp lint <files>`, package typechecks, `vp run build:desktop`, and the resource-monitor Cargo build and tests.

## State and caching

`ONICODE_HOME` points at the workspace's gitignored `.onicode`. Named Docker volumes hold the pnpm store and root `node_modules` so container rebuilds stay fast and off slow host bind mounts. The host sees an empty `node_modules`; run tooling inside the container.

## Out of scope

- `vp run dev` opens a desktop window, so it is host-only. Mobile native builds (Xcode / Android SDK) are host-only too; headless desktop bundles and static checks run in the container.

## Prebuilds

Codespaces prebuilds bake in `onCreateCommand` and `updateContentCommand` automatically. Note that prebuild snapshots exclude named caching volumes, so a prebuild-first workflow may prefer dropping the volume mounts. Outside Codespaces, push a prebuilt image with:

```bash
devcontainer build --workspace-folder . --push true --image-name <registry>/onicode-devcontainer:latest
```
