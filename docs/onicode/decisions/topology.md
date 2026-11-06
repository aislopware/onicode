# Topology: many clients, many servers, peer to peer

Every engine serves what it owns, and every UI is a client of any number of engines, the way t3code's
environments work, but with no relay of our own. Peers reach each other over the tailnet through the system
Tailscale client (slopscale control), and the app adds no pairing, NAT traversal or crypto: WireGuard already
authenticates and encrypts every packet. A relay would put a second congestion loop and an extra round trip on
every path, and a cloud service would be one more thing to run and trust. Zeron's Cloudflare Durable Objects,
WorkOS sign-in, org scopes and `edge/` go away step by step as the peer paths replace them.

The desktop UI keeps talking only to its own engine over localhost IPC, and that engine forwards to peers.
Thin clients (iOS, `onicode-client`) dial every engine directly. The host device stays the sole executor of a chat;
session documents sync replica to replica by Loro version vectors; registry rows replicate with a per-origin
version vector. Research: `.research/p2p-transport.md` (local only).

The engine joins the tailnet itself: on boot (`EngineConfig.peers`) it binds a `PeerEndpoint` as `Role::Engine`,
discovers the other engines through `PeerDirectory`, and serves its own RPC to them through `PeerLinks`. Forwarded
calls and subscriptions take that link; the edge room and its fake-device relay are gone. Links are admitted by
tailscale whois, not by account scope, so an edge sign-out no longer cuts them. Tests and fixtures never join the real
tailnet: they run engines over `onicode-net-sim`, whose seeded tailnet gives each engine its own node.
