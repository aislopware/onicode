# Session doc sync

Every chat is one Loro doc, and every device that shows the chat holds a full replica of it. The engine
that runs the chat is its host; a viewer is any other engine or app. There is no relay: replicas sync
directly over the tailnet.

## Wire

Two replicas sync over one `doc/{chatId}` QUIC stream (`onicode_sync::peer_doc`). Each frame is one
tag byte followed by a body. The tag gets `0x80` added when the body is LZ4-compressed with its size
prepended; that happens from 512 B, and only when the result is smaller.

| tag | frame | body |
| --- | --- | --- |
| 1 | `Open` | `[live u8][oplog vv]`, sent by the opener |
| 2 | `State` | `[oplog vv]`, the responder's reply |
| 3 | `Updates` | `[flags u8][loro bytes]`: flag 1 means more chunks follow, flag 2 means a snapshot to adopt |
| 4 | `Ack` | `[oplog vv]`, everything the sender holds now |

Each side sends the ops the other lacks, judged by the vv it received. A reconnect therefore cannot
strand a range. A peer whose vv is behind the sender's shallow root gets the whole shallow snapshot,
adopts it, and reopens the doc. A live session keeps pushing changes both ways and acks imports
every 120 ms. A responder that holds no replica resets the stream with `NOT_HELD`.

## Outbox and mailboxes

A replica's own local updates go to its outbox in the docs store (`outbox` rows, keyed by the vv
they end at).

- Rows retire only when the host acks a vv that covers them. A viewer's ack retires nothing.
- When the host is unreachable, a viewer syncs with any other engine that holds the chat (its
  mailbox).
- An engine that imports a viewer's ops while it is not the host schedules a `FORWARD` job. It
  carries the ops to the host when the host returns, and the job completes only on the host's ack.

The host runs a queued command once: commands are rows in the doc, and running one is a doc write
that every replica converges on.

## Where

- `crates/sync/src/peer_doc.rs`: the protocol, both sides.
- `crates/engine/src/doc_server.rs`: serves `doc` streams.
- `crates/engine/src/doc_host.rs`: keeps a session open to the host for each held chat, or to
  mailboxes while the host is away.
- `crates/engine/src/peer_doc_sink.rs`: the engine's `Replica`.
- `crates/client/src/live/room.rs`: the app's replica. It follows the host and falls back to
  mailboxes; `spawn_owed` keeps a chat open until the host has acked everything.
- `crates/engine/tests/peer_sync.rs`: seeded simulation on the virtual tailnet.
