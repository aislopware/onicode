# Replica sync: gone; the host is the authority and the outbox carries commands

Zeron synced session docs and the registry replica to replica by Loro version vectors, with a third engine
acting as a mailbox while a host was away. Onicode keeps no replicas (`engine.md`, "Authority"). A session's
host engine is its only writer. Viewers watch it: a snapshot at a cursor, then every event, and a viewer that
comes back with a cursor still in the host's ring gets only what it missed. A command for a host that cannot be
reached waits in the forwarding engine's durable `outbox` table and is delivered once the host is back, in the
order it was made; the host's op ledger answers any repeat with its first receipt, so it runs once. A host that
is offline shows the viewer's last snapshot, read-only.

Presence is not replicated either: a device is online while a live link to it exists, or while the tailnet
directory finds it (`WatchMachines`).

These behaviours are decided in seeded simulation on the virtual tailnet (`crates/engine/tests/peers.rs`) and
the outbox's model test (`crates/engine/src/outbox/tests.rs`).
