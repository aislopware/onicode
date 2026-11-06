# Remote desktop: Mac host, Mac and iOS clients, whole displays

Remote desktop streams whole displays from a Mac host to Mac and iOS clients. Per-window remoting is never built,
and neither are other host or client platforms: one host platform keeps capture, encode, input injection and
virtual displays on one set of APIs, and that is where the owner uses it.

The bar is Parsec measured on this Mac: about 33 ms p50 composite latency, 4:2:0 only, no FEC or NACK. Onicode
targets glass-to-glass p50 at or under 25 ms and p99 at or under 45 ms at 1080p to 1440p60 on the tailnet,
4:4:4 for text and 120 Hz. The host is `onicode-desktopd`, a Developer ID-signed launch agent that owns the
Screen Recording and Accessibility grants and its own QUIC endpoint. Video goes as low-latency HEVC with long-term
references, FEC and NACK over QUIC datagrams. The client decodes with VideoToolbox into a gpui-fast `VideoLayer`
and shows the system cursor. It ports slopty's media code once the peer transport and the gpui-fast migration have
landed.

P0 has landed as `onicode-desktop-media` (`crates/desktop-media`): slopty's pure media core ported whole with its
tests and fuzz corpora, namely packetizing with Reed–Solomon parity, reassembly with NACK and refresh, adaptive
redundancy, rate control with the chroma gate and encoder watch, clock sync, still refinement, audio copies,
temporal layers, heartbeats, cursor moves and the client's present-on-arrival pacer. Window streams, regions and
crops are gone: one stream is one display, or one stripe of it. Its wire lives in `wire`: a 16-byte media header
on datagram channel 2 (`MEDIA`, host to client) and client feedback on channel 3 (`FEEDBACK`), each after the
channel byte `onicode-net` adds, sized so a datagram fits a 1232-byte tailnet packet whatever the connection id.
The wire's canonical encoding is pinned byte for byte in a test, so no layout changes by accident: a deliberate
change updates the pinned bytes and bumps `wire::VERSION` in the same commit.
