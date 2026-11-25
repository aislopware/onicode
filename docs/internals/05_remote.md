# Remote architecture

Each connection joins a client to one environment over HTTP and WebSocket. Direct access (LAN),
Tailscale, and SSH change how the client reaches the server; they do not introduce a different
execution model. See [remote access](../user/remote-access.md) for setup.

## Identity is independent of the route

- An environment keeps its ID across restarts and endpoint changes. Repository identity can
  correlate clones across environments (preferring `upstream` for PR features, grouping forks by
  `origin`), but work never silently crosses environments.
- [Environment ID initialization](../../apps/server/src/environment/ServerEnvironment.ts) publishes
  atomically and retains repair state so concurrent initializers choose the same winner; do not
  sweep that recovery state as temp cleanup.
- **Routes are untrusted hints.** Only the connecting client can prove reachability. Never fall back
  to loopback when a shareable endpoint is unavailable (loopback names the client's own machine).
  Verify a direct or learned route answers as the expected environment before sending credentials,
  and never let a token paired over HTTPS downgrade to plain HTTP.
- **Learned routes.** A connected server reports its LAN and tailnet addresses; the client saves them
  as learned routes that reuse the credential, drops the ones the server stops reporting, and never
  touches routes the user saved.
- **A wildcard listener with a private-address filter.** A background service with no configured
  host listens on every IPv4 interface and closes connections unless both peer addresses are
  loopback, private, or tailnet, so a VPS's public address stays closed. It is not a firewall:
  Tailscale Serve arrives over loopback and a source-rewriting proxy looks private, so nothing may
  proxy the public internet to the main listener; internet-facing routes use the dedicated public
  access listener below. Tailnet-only mode applies a stricter filter and never
  reports a LAN address.
- Pull request routing trust covers the saved route set: adding or changing a route revokes trust;
  reordering does not.

## Public access (Tailscale Funnel)

[`PublicAccess`](../../apps/server/src/publicAccess/PublicAccess.ts) publishes webhook and outside-agent
routes to the internet through Tailscale Funnel without exposing the main server:

- **Dedicated loopback listener, never the main server.** Funnel forwards exclusively to a separate
  `127.0.0.1` listener (reusing a port persisted in secrets across restarts so a stale Funnel route
  cannot land on another process). Pointing Funnel at the main listener would leak loopback trust
  to the internet.
- **Live path gating.** Each request checks current `publicWebhooksEnabled` (`/api/hooks/*`) and
  `publicAgentsEnabled` (`/mcp`, `/oauth/mcp/*`, `/.well-known/oauth-*`) settings before forwarding
  to the main router, so turning a switch off stops its routes immediately and every other path
  returns 404. The gate checks the raw request target, which the router dispatches unchanged, and
  refuses any target a URL parser would rewrite (dot segments, encoded slashes, backslashes).
- **Public request sanitisation.**
  [`asPublicRequest`](../../apps/server/src/publicAccess/publicRequest.ts) strips all `tailscale-*`
  headers (so internet callers cannot spoof tailnet identity), rewrites `Host` and
  `X-Forwarded-Proto: https` to the Funnel origin for OAuth metadata and issuers, and sets
  `IsPublicRequest` in the Effect context so `/mcp` skips internal provider thread tokens.
- **Teardown order and policy gate.** Shutdown removes the Funnel route before closing the
  listener. A managed policy omitting `remote` from `clients` keeps Funnel off.

## Remote desktop is peer to peer

The server only [relays one offer and one answer](../../apps/server/src/remoteDesktop/RemoteDesktop.ts);
picture and input travel over WebRTC between the viewer and the desktop app on the host.

- **WebRTC, not the socket.** A late frame is worthless and TCP would hold later frames behind a lost
  packet; WebRTC brings UDP, congestion control, and hardware codecs in every client without native
  modules. A custom QUIC transport was rejected: no client runtime speaks it without native code, and
  inside a tailnet `ws://` already rides WireGuard.
- **The host is the desktop renderer**, the only process with Screen Recording/Accessibility grants
  and `getDisplayMedia`. A headless `onicode up` host cannot share a screen.
- **No STUN or TURN.** Host candidates only, so the connection needs a direct LAN or tailnet route
  and never leaves through a third party.
- **The host opt-in is a client setting on purpose**, so sharing can only be turned on at the host
  device, not by any client with settings access.
- **One display-media handler per session.** Electron allows a single
  `setDisplayMediaRequestHandler`; features register through
  [`claimDisplayMediaRequests`](../../apps/desktop/src/electron/displayMediaClaims.ts).

## Peer links

A [peer link](../../apps/server/src/peer/PeerLinks.ts) lets one environment's agents work in another
by signing in to the target's `/mcp` as an ordinary OAuth client (`onicode-peer-link`). The receiving
side lists and revokes it in Sharing & access like any other session, and owns its expiry/renewal
(answering each call with `oni-link-expires-at`; the linking side never extends a link itself, and a
receiving side that predates renewal sends no header, so the link keeps its original 30 days).
Unlinking sends the token to `/oauth/mcp/revoke` only at an address that still answers as that
environment.

- **Mode ceilings always narrow.** Forwarded calls carry the calling agent's modes in
  `oni-mode-limit`, which the receiving `/mcp` can only intersect downward with the link's approved
  access.
- **Non-full-access links are fenced by `linkOrigin`.** Threads launched by a peer link (and their
  derived forks, subagents, and spawned threads) carry an immutable `linkOrigin` stamp enforced in
  shared access rules ([`mcp/linkOrigin.ts`](../../apps/server/src/mcp/linkOrigin.ts)). Below full
  access, linked work can mutate only threads with the same origin, cannot touch projects, settings,
  scheduled tasks, or further links, and skips project setup scripts and worktree layout commands
  (in a project that declares a layout, a worktree launch or handoff for such a thread fails;
  restoring a missing folder brings back plain checkouts). Full-access links are not fenced because
  their agents already run as the receiving OS user. Being unfenced only answers who asked: a
  layout command still runs only with a person's approval of its text, and for an agent only from
  a thread in Full access, both checked in
  [`TaskFolders`](../../apps/server/src/workspace/TaskFolders.ts) at every run.
- **A descriptor's environment id filters addresses; it does not prove identity.** Only addresses
  from the user's client count as a machine's, never a loopback one, and an address alone never
  moves an unexpired link.
- **Namespace idempotency keys.** All callers on the linking side share one session on the receiving
  side, so idempotency keys must be hashed with the caller's namespace before forwarding.
- **The Linked environments beta (`linkedEnvironmentsEnabled`) gates both sides.** Off on the linking
  side, new links and forwarded calls refuse, but `readThread`, `waitForThread`, and `interruptThread`
  still work so running remote tasks complete and stay cancellable. Off on the receiving side, link
  sessions get a 403, which the linking side reports as retryable.
- **A delegated task can run in a linked environment.** The parent records `remoteChild` instead of
  `childThreadId`, and a [`RemoteDelegation`](../../apps/server/src/peer/RemoteDelegation.ts)
  follower completes it; open remote tasks are followed again at startup, which also keeps restart
  recovery from treating them as abandoned provider work.
- **Clients read linked threads as standard thread streams.**
  [`PeerThreadRelay`](../../apps/server/src/peer/PeerThreadRelay.ts) proxies the same stream format
  read-only so there is no second transcript schema to maintain.

## Desktop previews tunnel loopback

A desktop webview for a remote environment keeps `localhost` URLs and tunnels loopback connections
through `/api/preview/tunnel` on the environment's own origin:

- **Fixed HTTP proxy, not PAC.** Chromium ignores PAC scripts for loopback hosts. The partition uses a
  fixed proxy that routes only loopback through the tunnel (handing link-local addresses back to the
  direct route) and has no direct fallback—a failed or disabled tunnel must error rather than
  silently load the reviewer's own localhost.
- **HTTP so it can authenticate.** Chromium cannot authenticate to SOCKS, but answers an HTTP proxy's
  407 through Electron's `login` event, so the
  [listener](../../apps/desktop/src/preview/LoopbackTunnel.ts) refuses clients without this run's
  random credentials.
- **A partition of its own.** `localhost` cookies and storage ignore the port, so tunnelled tabs use a
  separate partition.
- **Media tickets.** Desktop main holds no environment credentials, so the renderer supplies a media
  ticket per connection (also tracking route switches).
- **Forwarded ports and mobile preview are unauthenticated on purpose.** Forwards listen on plain
  loopback like `ssh -L`, on Linux and macOS restricted to connections the system attributes to this
  OS user (`/proc/net/tcp`, `lsof`); one whose owner cannot be read is refused, and Windows has no
  owner check. Mobile device preview opens its own SOCKS listener; on iOS
  [`OniHttpRewriter`](../../apps/mobile/modules/oni-preview-tunnel/ios/OniHttpRewriter.swift) swaps
  loopback names for a stand-in host in plain HTTP heads and text bodies, since iOS never proxies
  loopback hosts.
- **Off stays routed.** With `allowsPreviewTunnel` off, the partition keeps its tunnel proxy, so a
  desktop tab left open fails instead of loading the reviewer's own localhost.

## Access and process ownership are separate

- Tailscale supplies an endpoint for normal pairing; auth remains the environment's responsibility.
- [SSH cleanup](../../packages/ssh/src/tunnel.ts) stops a remote server only if the launcher started
  it; a server discovered already running must survive client disconnect.
- Remote servers outlive client releases. Clients negotiate features via advertised capabilities
  rather than assuming version parity. Process replacement uses the
  [update protocol](./07_server-updates.md).
- Desktop can run without a local environment (`localEnvironmentEnabled`): the renderer is served
  from disk by the `onicode://` scheme, so only saved environments connect.
