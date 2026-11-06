# Project previews

Run an HTTP development server in a project and open a Browser tab in that
session. The empty tab lists its running services. **Open** navigates to
`http://<device>.<project>.localhost:7331`; additional services receive persistent
names such as `<device>.<project>-api.localhost:7331`.

The daemon owns this listener, discovery and the tunnels to other devices.
Closing a browser tab does not stop discovery or change an address. Account
teardown cancels the proxy, discovery, catalog feeds and open tunnels before the
next profile starts. A busy proxy port produces an inline error instead of
changing the URL.

## Discovery and identities

On Linux, the scanner joins the current user's `/proc` process cwd, ancestry,
creation time and socket descriptors to listening TCP sockets. On macOS it uses
`lsof` and `ps` for equivalent metadata. Only loopback-reachable listeners whose
cwd belongs to a known local project are probed. The deepest matching project
wins. Commands with explicit authentication arguments (`login`, `auth`,
`authenticate`, `signin`, `sign-in`, `sso`, `oauth`, or `oauth2`, before `--`)
are excluded before any connection, even when started by a Zeron terminal or
agent. Their listeners may be one-shot browser callbacks: for example,
`infisical login` fails with EOF if it receives a discovery HEAD without the
browser's JSON body. This command check does not identify custom callback
servers without an authentication argument.

Unrelated listeners and non-HTTP services are excluded. Discovery runs every
two seconds; bounded HEAD probes accept valid HTTP status responses (including
authentication and application errors). Confirmed HTTP listeners are not
probed again during that socket's lifetime; non-HTTP results use a capped
exponential backoff.

Zeron terminal/task/agent descendants are marked as Zeron-owned. Framework
commands identify Vite, Next.js, Astro, Miniflare and Node servers; otherwise the
list uses a generic HTTP label. Before a local backend connection, the daemon
rechecks the listener's process identity and cwd to reject stale port reuse.

The profile's `previews.json` stores project and service IDs and hostname labels
separately from observed PIDs and ports. Command identities omit common port
options. Device-name collisions get persisted suffixes. The first service keeps
the project hostname; additional services get descriptive suffixes. Two
indistinguishable instances of the same command in the same cwd get separate
slots; their individual identities cannot be inferred across simultaneous
restarts without additional application-provided identity.

`WatchPreviews` resolves the session's current cwd on each catalog or workspace
change. On the viewing device it selects either the local services or the
advertised services for the session's owning device. Changing the active
checkout therefore changes the list without copying paths or entering ports.

## HTTP and stream transport

On macOS, WebKit receives a per-domain HTTP CONNECT configuration for preview
hostnames, so `*.localhost` names reach the proxy without system DNS changes.
The CONNECT endpoint admits only known preview names at the proxy port, and its
tunnels share bounded limits and account cancellation. Other website traffic
retains normal routing.

The loopback proxy validates the Host against its catalog, then opens the
service by its opaque ID. A local service is a TCP connection to its listener. A
service on another device is a tunnel: one QUIC stream (`StreamKind::Tunnel`,
named by the service ID) on the peer link to that device, carrying the
connection's raw bytes both ways (`onicode_preview::tunnel`). Tunnels are sent
behind terminals and RPC on the same link, and a writer waits once 256 KiB is
queued. A clean close half-closes the stream; a reset at either end (the browser
giving up, the dev server failing) reaches the other end as a reset. Remote
metadata never authorizes an arbitrary TCP address: the hosting device resolves
only its own live service IDs.

Hyper streams request and response bodies. The proxy removes hop-by-hop headers,
preserves Host and Origin together (including Next.js Server Actions), sets
X-Forwarded-Host, and rewrites absolute localhost redirects. Set-Cookie and other
end-to-end headers remain intact. WebSocket upgrades retain their byte stream,
subprotocol and close handshake, allowing Vite HMR through the same URL.

## Catalogs across devices

Devices meet over the tailnet; there is no coordinator. Each engine opens a
stream named `catalog` to every engine it has a live peer link to, and the host
sends its local services there as JSON whenever they change. The services are
listed under that device until the stream or the link ends. Peers admit each
other by tailscale whois, so a user's previews reach only that user's devices.
Local previews work without any link.

## Validation

`cargo nextest run -E 'package(onicode-preview)'` covers real process/cwd
isolation, non-HTTP exclusion, live disappearance, persistent aliases, port
changes, streaming HTTP headers and redirects, WebSocket traffic and
cancellation through the proxy. `tests/sim.rs` runs two devices on a simulated
tailnet in virtual time: an HTTP exchange larger than the tunnel window both
ways, and resets and clean finishes reaching the other end.

Build `cargo build -p zeron-ui --example preview-fixture --features browser-fixture`.
Run the fixture with an output directory, an available display and `VITE_BINARY`
pointing to an installed `vite/bin/vite.js`. It starts real Vite/API processes in
an isolated project, discovers them through daemon RPC and waits for a native
click on Vite's Open button. It then verifies HMR, disappearance and a port-change
restart while capturing the native UI. Screenshots/videos belong in PR user
attachments, not the repository.
