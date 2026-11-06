# Replica sync: version vectors peer to peer, the host's ack retires

Session docs and the registry used to sync through zeron's edge: a relay room per chat that held a
cursor and an epoch, and a registry service with a pending-op queue. Both now sync replica to replica over
tailnet QUIC streams, `doc/{chatId}` and `registry`, by trading version vectors. Each side sends exactly
what the other's vv lacks, so a reconnect cannot strand a range, and no cursor, epoch or server copy
exists to drift. A replica behind a doc's shallow root gets the whole snapshot, adopts it, and starts
over.

A viewer's outbox retires only on its host's ack. Any other replica's ack retires nothing. While the
host is away, a third engine holding the chat serves as a mailbox: it imports the viewer's ops and keeps
a `FORWARD` job until the host acks them. A send therefore survives the viewer, the mailbox or the host
going offline in any order, and it runs once, on the host.

Registry ops carry `(origin, oseq)`. Owner-only fields are checked at merge as well as on write, because
with no server in the middle every replica must refuse a forged write itself. Each pair of engines runs
one session: the side that dialed the link opens it. An app mirrors a single engine. Presence is no
longer a replicated beat: a device is online while a live link to it exists, or while the tailnet
directory finds it. This replaces the edge's liveness hooks and the link-cache cooldowns.

These behaviours are decided in seeded simulation on the virtual tailnet (`crates/engine/tests/peer_sync.rs`),
with real engines running a mock harness on a paused clock.
