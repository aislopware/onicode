# Computer use

Agents drive desktop apps through [cua-driver](https://github.com/trycua/cua).
Onicode adds no tools of its own: each provider session launches cua-driver's
stdio MCP proxy, and every proxy talks to one daemon the server owns. Passing
the driver's own tool list and instructions through unchanged is the point. A
wrapper would have to track a driver that ships several releases a week.

## Why the server starts the daemon

macOS grants Accessibility and Screen Recording to the app at the top of a
process's launch chain. The desktop app starts the server as a direct child,
so a daemon the server spawns in embedded mode answers with Onicode's grants
and never raises a prompt of its own. A daemon started any other way, through
LaunchServices or by a standalone cua-driver, is its own identity with its own
grants. macOS also answers a process from the grants it had at launch, so
[`ComputerService`](../../apps/server/src/computer/ComputerService.ts) replaces
a daemon that started without a grant instead of asking it again.

A server that is not a child of the app (`vp run dev:server`, a service on a
headless machine) gets the grants of whatever launched it, usually none.

## One binary, one endpoint per daemon

The proxy refuses a daemon of another version, so the daemon and every proxy
run the one release pinned in
[`ComputerDriver`](../../apps/server/src/computer/ComputerDriver.ts). Bump the
version, sizes and hashes together.

The approval mode is fixed when the daemon starts, so changing it restarts the
daemon. A proxy never reconnects, and its session would keep a dead one. Each
daemon therefore listens on a new endpoint, and the session's
`computerMcpServer` changes with it: an adapter that builds its MCP servers per
turn picks the new proxy up at once, and the others at their next session.
That swap leaves the Onicode MCP credential alone, so it does not disturb a
live session's Onicode tools.

## Adding a provider

An adapter gets the proxy as `computerMcpServer` on its
[`McpProviderSessionConfig`](../../packages/provider-core/src/server/mcpSession.ts)
and registers it under `COMPUTER_MCP_SERVER_NAME` next to Onicode's server. It
is absent when the user, the project or the per-provider switch withholds
computer use, and when the driver cannot run on the machine.
