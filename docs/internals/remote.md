# Remote architecture

Each connection joins a client to one environment over HTTP and WebSocket. The
environment owns providers, execution, files, and durable state. Direct access,
Tailscale, and SSH change how the client reaches that server; they do not
introduce another execution model. See
[remote access](../user/remote-access.md) for setup.

## Identity is independent of the route

An environment keeps its ID across server restarts and endpoint changes. Saved
connections are local to a client profile; the server's identity and state are
not. A repository identity can correlate clones across environments, but never
routes work between them. A project and its threads belong to one environment;
an agent reaches another environment's threads only through a peer link.
The canonical key follows the `upstream` remote when one exists, so pull request
features target the repository a fork tracks. A fork also reports its own
`origin`, and clients group and label by that, so a fork never collapses into a
checkout of its upstream.

[Environment ID initialization](../../apps/server/src/environment/ServerEnvironment.ts)
must publish a complete ID atomically. Repair of an empty ID file retains a
recovery file so concurrent or delayed initializers choose the same winner.
Removing that recovery state as ordinary temporary-file cleanup can change the
identity underneath an already-running server.

Advertised endpoints are reachability hints. Only the connecting device can
prove that a route works. In particular, a host's loopback address refers to a
different machine when another device opens it. Endpoint selection must not
silently fall back to loopback when a shareable endpoint is unavailable.

A saved environment holds an ordered list of routes, and the
[driver](../../packages/client-runtime/src/connection/driver.ts) connects over
the first that works. Each direct route is first checked with the public
descriptor, so a saved LAN address that a different machine answers on another
network receives no credential. That check is not proof of a working route:
when every route stays silent, each is still tried. A route that fails to
connect moves on to the next; only an incompatible server stops the walk, because it is the same
server on every route. While connected over a later route the
[supervisor](../../packages/client-runtime/src/connection/supervisor.ts)
preflights the earlier ones and replaces the session when one would connect.
Preflight includes authorization so a route that answers but rejects this
client never costs a working session; a route that still fails afterwards is
held back for a cooldown so a flaky network cannot bounce the connection.

A connected server reports the LAN and tailnet addresses it is bound to, and the
client saves them as learned routes. A learned route reuses the credential of
the route it was learned over, the paired bearer token. A paired token only learns a plain HTTP address off the tailnet
when it was itself paired over plain HTTP, so a token paired over HTTPS never
starts travelling unencrypted. Learned routes the server stops reporting are
dropped, which is how a changed LAN address replaces the old one; routes the
user saved are never touched. The reported addresses are hints like any advertised endpoint, so a
learned route still has to answer as this environment before it is used.

A background service with no configured host is what makes this work for
headless machines. It listens on every IPv4 interface and closes each
connection unless both addresses of the direct peer are loopback, private, or
tailnet IPv4. That keeps a VPS's public address closed, including a cloud VM
whose public address is NATed to a private interface. It is not a firewall:
Tailscale Serve reaches the server over loopback by design, and a proxy that
rewrites the source address looks private, so nothing may proxy the public
internet to it. One listener per address would
miss a DHCP change, and the wildcard keeps the reported addresses current. Tailnet-only mode uses the
same wildcard with a stricter filter: both addresses loopback, or both tailnet
IPv4. A LAN address is never reported in that mode.

Pull request routing trust covers the whole route list. Adding or changing a route
revokes it; reordering does not, because the same addresses remain trusted.

## Remote desktop is peer to peer

Every other feature reaches a machine through its environment server. Remote
desktop does not: the server only
[relays one offer and one answer](../../apps/server/src/remoteDesktop/RemoteDesktop.ts),
and the picture and input travel over a WebRTC connection between the viewer and
the desktop app on the host. The reasons, and what follows from them:

- **Why WebRTC and not the socket.** A screen is real-time media: a late frame
  is worthless, and TCP would hold every later frame behind a lost packet.
  WebRTC gives UDP transport, congestion control, and the platform's hardware
  encoders and decoders in every client without a native module. A custom
  QUIC transport for RPC was evaluated at the same time and rejected: neither
  the Electron renderer nor React Native can speak it without native code in
  three runtimes, and `ws://` inside a tailnet is already plaintext TCP inside
  WireGuard, so removing TLS saves nothing there.
- **The host is the desktop app's renderer**, not the server. Only a GUI
  process can hold the Screen Recording and Accessibility grants, and only a
  renderer has `getDisplayMedia` and a hardware encoder. A headless
  `onicode up` host therefore cannot share a screen, and the server reports a
  host only while a desktop app holds the
  [`remoteDesktop.host`](../../packages/contracts/src/rpc.ts) stream open.
- **No STUN or TURN.** Both peers offer host candidates only. Electron offers
  its real addresses, including the tailnet one, which is what lets a WebView
  viewer that hides its own addresses behind mDNS names connect: the viewer
  dials the host, and the host learns the viewer's address from that packet.
  The connection therefore needs a direct route (LAN or tailnet) and never
  leaves through a third party.
- **Signaling is one request.** Each side waits for ICE gathering to finish and
  sends a complete description, so there is no trickle channel to keep ordered
  across the relay.
- **The host opt-in is a client setting on purpose.** A server setting could be
  flipped by any client with settings access, from anywhere. Client settings
  live on the host device, so sharing can only be turned on by someone at it.
- **Frames are shown on arrival.** The desktop app starts with
  `WebRTC-ForceSendPlayoutDelay` set to zero, which every libwebrtc receiver
  honors. Without it the viewer's jitter buffer held frames for 10 to 60 ms
  after each idle-to-motion change; `jitterBufferTarget = 0` alone does not
  lower Chromium's floor.
- **Codec choice can be refused.** The host prefers HEVC, then H.264 from the
  highest profile down, but a viewer may negotiate a variant the host cannot
  encode (WebKit's constrained-high H.264). `setParameters` rejects the whole
  call for that, so [the host](../../apps/web/src/remoteDesktop/host.ts) sets
  bitrate and scaling first and tries codecs one at a time afterwards.
- **A lost path reconnects; a hang-up does not.** The host closes the input
  channel when it ends a session, and a network change leaves it open and
  silent. [The viewer](../../packages/client-runtime/src/remoteDesktop/viewer.ts)
  retries only the second, so **Disconnect** on the host is not undone by the
  viewer's own recovery.
- **The clipboard is read around macOS's paste prompt.** A read that waited
  behind that prompt in the main process would lock a remote user out, because
  the same process posts their input and they could not dismiss it. The host
  watches the pasteboard's change count, which never prompts, and
  [reads the text](../../apps/desktop/src/remoteDesktop/MacHost.ts) in-process
  only once macOS always allows it. Before the first prompt it reads through
  `pbpaste`, so the prompt holds that child instead. Once macOS asks every
  time, copies are left unread and Settings says why. What was copied before
  a viewer connected is never sent.
- **One display-media handler per session.** Electron allows a single
  `setDisplayMediaRequestHandler`, and the browser preview needs it too.
  Features register a claim through
  [`claimDisplayMediaRequests`](../../apps/desktop/src/electron/displayMediaClaims.ts)
  instead of installing their own.

## Peer links

A [peer link](../../apps/server/src/peer/PeerLinks.ts) lets one environment's
agents work in another. The linking environment signs in to the other's `/mcp`
through its peer-link OAuth flow, with a pairing code from it, and keeps that
session's token in its own secret store. The other environment lists the link
in Sharing & access with its other clients and revokes it there.
Unlinking on the linking side only forgets the token.

- **The Linked environments beta (`linkedEnvironmentsEnabled`) gates both
  sides.** Off on the linking side, `PeerLinks.link`, link requests and
  `PeerForwarding.call`/`links` refuse; `readThread`, `waitForThread` and
  `interruptThread` do not, so a remote task already running still completes
  and can still be cancelled. Off on the receiving side, link registrations are
  refused and link sessions get a 403, which the linking side reports as
  retryable so the task's follower waits rather than failing it.
- **A tool runs in a linked environment when its parameters take
  `OrchestratorMcpEnvironmentTarget`** as `environmentId`.
  [`McpToolAccess.toLayer`](../../apps/server/src/mcp/McpToolAccess.ts) sends
  such a call there once the tool's access declaration has checked the caller,
  so a read-only or ended caller is refused before anything is sent. The
  thread or project the call names is checked where it lives. A tool that acts
  as the calling thread itself cannot take a linked environment.
- **Every forwarded call carries the calling agent's modes** in
  `Onicode-Mode-Limit`, which the receiving `/mcp` only ever narrows the client's
  approved access with. A plan-mode agent therefore cannot start full-access
  work through a full-access link.
- **A descriptor's environment id filters addresses; it does not prove
  identity.** The token goes only to an address that reports the linked id, so
  a LAN address that now reports another machine's never receives it. Anything can
  report any id, though, so only addresses from the user's client count as a
  machine's, and never a loopback one, which names whichever machine opens
  it. An address an agent gives is recorded alone, and
  [`linkRequestTarget`](../../packages/client-runtime/src/linkRequest.ts) names
  a machine for it only when the client already reaches exactly one machine
  there. An address alone never moves an unexpired link to another address.
- **All callers on one side share the link's single session on the other**,
  so idempotency keys are hashed with the caller's namespace before they
  leave. Without that, two agents reusing a key would collide on the far side.
- **A forwarded wait is split into calls of at most 50 s**, because the
  receiving `/mcp` sends nothing while a wait is open, and a proxy on the
  route may drop a request that stays idle too long.

**Work a link paired below full access starts stays within that link.** The
receiving environment knows a session is a peer link because the linking side
registers its OAuth client with the `onicode-peer-link` software id, the only
one registration accepts, and the marker is signed into the session. Threads that session launches carry it, with
the access it was paired with, as an immutable `linkOrigin`, and so do the
subagents, forks and `create_threads` they derive. The rules live in
[`mcp/linkOrigin.ts`](../../apps/server/src/mcp/linkOrigin.ts) and run in the
shared access declarations, not tool by tool:

- linked work may change only threads with the same origin, not the user's own
  threads and not another link's;
- it cannot change projects, settings or scheduled tasks, nor use the receiving
  environment's own links;
- its project setup scripts are skipped
  ([`ProjectSetupScriptRunner`](../../apps/server/src/project/ProjectSetupScriptRunner.ts)
  checks every path that runs them).

Reads are not fenced, and a client's own `thread.create` can never carry
`linkOrigin`.

**A delegated task can run in a linked environment.** Its child is then an
ordinary thread there, so the parent here records the task without a child
thread of its own (`remoteChild` instead of `childThreadId`). A follower in
[`RemoteDelegation`](../../apps/server/src/peer/RemoteDelegation.ts) waits on
that thread and completes the task with `delegated_task.remote.complete`. That
internal command reuses the parent half of a local finalize, so the parent
wakes the same way. Open remote tasks are followed again at startup, which is
also what keeps restart recovery from treating them as abandoned provider work.

The link is routing, not isolation: an agent the link starts runs as the
receiving environment's user. **A full-access link is therefore not fenced**:
its agents can already do anything that user can, so the fence would only stop
Onicode's own tools. Its work still carries `linkOrigin`, so clients show where it
came from. A stamp without the access, from before it was recorded, is fenced.

## Access and process ownership are different

Tailscale supplies an endpoint for ordinary pairing, so it needs no separate
environment type. Authentication remains the environment's responsibility for
every route. See [environment authentication](./environment-auth.md).

SSH can launch a server as well as forward a port. Desktop main owns that
lifecycle because it can spawn SSH and handle authentication prompts. The
renderer uses the forwarded endpoint through the shared connection runtime.
[SSH cleanup](../../packages/ssh/src/tunnel.ts) stops a remote server only if the
launcher owns it; a server it discovered already running must survive a client
disconnect. Reconnection restores the forward before opening the application
transport.

Remote servers can outlive several client releases. Clients must use advertised
capabilities and handle their absence, rather than assume their own version
describes the server. Process replacement belongs to the launcher's
[update protocol](./server-updates.md); the connection runtime handles the
resulting disconnect.

### Desktop without a local environment

Desktop normally launches its own primary server, but the desktop setting `localEnvironmentEnabled`
(`apps/desktop/src/settings/DesktopAppSettings.ts`) turns that off. Changing it relaunches the app;
no local state is deleted. On the next start the main process skips port selection, server exposure,
and the primary and WSL backends, and opens the window right away. The renderer sees this through
`desktopBridge.getLocalEnvironmentEnabled()`: `readPrimaryEnvironmentTarget` returns null, so primary
auth and platform-managed discovery are skipped and only saved environments (pairing, SSH)
connect. This is possible because the desktop renderer is not served by the backend: the `onicode://`
scheme serves the bundled client from disk (Vite in development) and API traffic always goes to the
environment's own URL.
