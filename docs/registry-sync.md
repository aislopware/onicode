# Registry sync

The registry (devices, spaces, chats, sessions, sidebar preferences) is one `RegistryDoc` per user. Each
engine holds a full replica. An app mirrors one engine: it takes that engine's rows and pushes its own
writes to it. There is no server copy.

## Ops and version vectors

Every field write is stamped `(origin, oseq)` with a hybrid clock. A row merges field by field, latest
clock wins (`onicode_doc::merge_row`). A replica's vv maps each origin to the highest oseq it holds.

Owner-only fields (`onicode_doc::owner_only`) are a device's own row, a space's git stamps, a chat's
host-written fields such as `lastMessagePreview`, and session status. A write to one of them is kept
only when the device in its clock owns the row. This is checked on local writes and again at merge,
so a peer cannot forge one by sending rows. Fields anyone may write, such as a chat's `title`, merge
normally. Deleting a row is not a field write, and any device may do it.

## Wire

Replicas sync over one `registry` QUIC stream (`onicode_sync::registry`). Each frame is one JSON
object, prefixed by `0`, or by `1` when it is LZ4-compressed with its size prepended (from 512 B,
when smaller).

1. Both sides send `{"t":"hello","vv":{..}}`.
2. Both sides send `{"t":"rows","rows":[..]}`, ending with a frame that carries `"vv"`. These are
   the rows holding writes the peer's hello lacked.
3. While the session stays open, each change goes out as a `rows` frame with the sender's `vv`.

A receiver raises its vv to a frame's `vv`. The sender attaches a `vv` only once it has sent
everything up to that vv.

## Who talks to whom

- Each engine dials every engine the tailnet directory finds. The side that dialed a link opens
  the registry stream on it, so each pair of engines runs one session.
- An app opens one stream to an engine it holds a live link to, or else to one the directory
  found.
- An engine's registry counts as synced once one peer's catch-up has merged, or when no other device
  has ever written to it.
- Presence is not replicated: a device is online while a live link to it exists. The engine also
  counts the engines the directory found, and refreshes `lastSeenAt` from both every 15 s.

## Where

- `crates/doc/src/registry.rs`: the doc, merge and owner-only rules.
- `crates/sync/src/registry.rs`: the protocol.
- `crates/engine/src/registry_server.rs`: serves streams and keeps the engine sessions open.
- `crates/engine/src/workspace_host.rs`: the engine replica and presence.
- `crates/client/src/live/mod.rs`: the app's mirror.
