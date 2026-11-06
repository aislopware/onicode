# Onicode

A native control plane for coding agents on macOS and iOS: many clients drive many servers peer to peer over a
tailnet, and working on a remote Mac feels local.

The app grew from [monocode](https://github.com/hardbeat920/monocode)'s native GPUI preview, whose interface it
keeps exactly, on Onicode's own engine, tailnet transport, remote desktop and terminal
(`docs/onicode/decisions/monocode-base.md`). macOS and iOS 27.2 or later.

Developing: `scripts/dev/setup.sh`, then `scripts/dev/check.sh`. See `CLAUDE.md` and `docs/onicode/`.
