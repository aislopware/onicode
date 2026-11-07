# Topology: many clients, many servers, peer to peer

Every engine serves what it owns, and every UI is a client of any number of engines, the way t3code's
environments work, but with no relay of our own. Peers reach each other over the tailnet through the system
Tailscale client (slopscale control), and the app adds no pairing, NAT traversal or crypto: WireGuard already
authenticates and encrypts every packet. A relay would put a second congestion loop and an extra round trip on
every path, and a cloud service would be one more thing to run and trust. Nothing of zeron's edge is left
(`no-edge.md`): a device's identity is its tailnet node, and its profile is its data directory.

The desktop UI keeps talking only to its own engine over localhost IPC, and that engine forwards to peers.
Thin clients (iOS, `onicode-client`) dial every engine directly. On macOS a thin client finds the engines through
the Tailscale LocalAPI, as an engine does. iOS has no LocalAPI to read, so the phone keeps a list of engines the
person typed (`EngineList`: MagicDNS names or tailnet addresses), dials each over QUIC, and each engine admits the
phone by its own whois of the phone's address. `NWPathMonitor` redials the links at once on a network handover
(`PeerLinks::follow_path`). A session's host engine is its only writer; every other device watches it and sends
commands (`engine.md`, "Authority"), and nothing is replicated. Research: `.research/p2p-transport.md` (local
only).

The engine joins the tailnet itself: on start (unless `ONICODE_PEERS=0`) it binds a `PeerEndpoint` as `Role::Engine`,
discovers the other engines through `PeerDirectory`, and serves its own RPC to them through `PeerLinks`. Forwarded
calls and subscriptions take that link. Links are admitted by tailscale whois alone. Tests and fixtures never join the real
tailnet: they run engines over `onicode-net-sim`, whose seeded tailnet gives each engine its own node.

Terminals ride the same links as every other call: a viewer watches another engine's terminal with
`WatchTerminal` and types with `TerminalInput`, each on its own RPC stream (`engine.md`, `terminal.md`). A link
carries control, RPC and file streams only, and its datagrams are remote desktop's. WebRTC, its signaling room,
STUN and the relayed terminal path are gone.
