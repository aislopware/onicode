# Local feel: the network may delay confirmation, never echo

Onicode must feel like working on the local machine. A local editor or terminal answers in single-digit
milliseconds, and a tailnet round trip alone is 1 to 80 ms (more over DERP), so every action the user starts
echoes locally within one frame and the network only ever confirms it. Sends, steers, stops, queue edits and
approval answers echo at once under ids the client mints and go to the host, which dedupes on those ids and
answers a repeat with its first receipt. The UI marks an action pending until the authority's state covers
it, never on a timer, and rolls it back visibly only on an explicit rejection. This is how Replicache, Figma,
Linear and t3code's mobile outbox work; in onicode the host's op ledger and the forwarding engine's outbox
carry it (`engine.md`).

Reads come from the host's watches: a snapshot at a cursor, then every event, resumed with the cursor after a
drop, the snapshots last seen kept on disk so a first frame never waits; file trees as host-pushed listings,
as in Zed; per-device catalogs and recently opened files cached on disk and revalidated in the background. Links to
every known engine are dialled at start and on foreground, kept alive at 1 s pings while something live is on
screen, and reset on wake or a path change. The terminal paints only what the host echoed (`terminal.md`). What
is the viewer's own stays local: a window resize relays the terminal's mirror out in the same frame, and the
host's resize reply and stream settle it without guessing the host's state. Find in the scrollback runs on the
mirror with no round trip.

Each target is stated in round trips plus a constant, or as zero network events before the echo. Seeded
virtual-time scenarios in `onicode-latency` over `onicode-net-sim` profiles (LAN to DERP, loss, roam, sleep)
check them, against a committed baseline that only ratchets down. Wall-clock budgets (frame time, SQLite open,
cold start) run as benches on a Mac. The research and the ranked plan with file:line maps stay in
`.research/local-feel.md` (not committed).
