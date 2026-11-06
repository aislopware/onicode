# No edge: the tailnet is the only service

Zeron ran a Cloudflare Worker, `edge/`, for sign-in, sync, relay, cloud machines and updates. Onicode
has no service of its own (`topology.md`), so the edge and everything that leaned on it are gone.

## Removed

- WorkOS sign-in, organizations, org scopes and the synced profile.
- Cloud devices: the cloud runner, cloud delegates, billing and the vault.
- Edge rooms, the device-room relay with its fake-device sockets and `LinkCache`.
- Tool output and diff sidecar blobs, and the diff upload.
- APNs push sent by the edge.
- The update manifest served by the edge.
- The `edge/` worker, its scripts, its CI jobs and its env vars (`ONICODE_EDGE_URL`, `ONICODE_EDGE_TOKEN`,
  `ONICODE_ORG_ID`, `ONICODE_WORKOS_*`).

## What replaces each

- Profile: one per device, its data directory. Identity: the tailnet device. There is no account.
- Transport: peer links carry RPC, subscriptions, terminals, previews, session docs and the registry
  (`replica-sync.md`, `terminal.md`).
- Admission: tailscale whois of the caller's address.
- iOS discovery: `EngineList`, the engines the person typed, dialled over QUIC.
- Reconnect after a network change: `NWPathMonitor` through `PeerLinks::follow_path`.
- Updates: the feed named by `ONICODE_UPDATE_FEED` until better-update's feed (`releases.md`). With no
  feed set, the updater stays off.

## Deliberately gone

- Push notifications, until there is a peer-to-peer design. One would need the phone to register its
  APNs token with each engine over its link, and the engine to post to APNs directly with the owner's
  key. The other option is a background wake on the phone through NetworkExtension. Until then the
  phone learns of a finished turn when it next opens a link.
- Cloud machines: a VPS on the tailnet running `onicode-engine` takes their place.
- Account deletion and multi-user organizations. There is no account to delete. Sharing with another
  person is a tailnet grant of onicode's capability (`onicode_tailnet::CAP`), not an org.
