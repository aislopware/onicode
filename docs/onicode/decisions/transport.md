# Transport: plaintext QUIC on vendored noq

Peers speak QUIC with null crypto on the vendored noq 1.4.0 (`vendor/noq-proto`, `vendor/noq-udp`), carrying the
patches slopty and axolotty wrote and measured: BBR3 held to twice the bandwidth-delay product, stream priority
ahead of datagrams for control and terminal echo, datagram copies, an MTU of 1232 and UDP port 7433. Encrypting
again inside WireGuard bought nothing: slopty measured control round trips falling from about 1.2 to 0.55 ms and
connect-to-hello from about 30 to 11 ms with null crypto.

A peer is admitted before the handshake by Tailscale's LocalAPI `whois`. The same Tailscale user gets full access,
a tagged node needs an app capability grant, and anything else is refused. Admission fails closed when the
LocalAPI does not answer. There is one connection per device pair and one stream per RPC call, document, terminal,
file or tunnel. Video and input copies ride datagrams. Behaviour is decided in seeded virtual-time simulation
(`onicode-net-sim`); a real tailnet run is only a smoke check.

A device id is bound to its tailnet node at admission. When two peers dial each other at once, the link dialled
by the smaller device id survives, and only the end that dialled redials a lost link, so the two ends never race.
The control stream carries ping and path frames under a single wire number (`WIRE`), and a peer with a different
wire number is refused at hello: no backward compatibility before release.
