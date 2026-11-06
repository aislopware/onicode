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
