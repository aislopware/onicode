# Local feel: the network may delay confirmation, never echo

Onicode must feel like working on the local machine. A local editor or terminal answers in single-digit
milliseconds, and a tailnet round trip alone is 1 to 80 ms (more over DERP), so every action the user starts
echoes locally within one frame and the network only ever confirms it. Sends, steers, stops, queue edits and
approval answers are written to the local replica under ids the client mints. The host dedupes on those ids
and answers a receipt for one it has seen. The UI marks an action pending until the authority's state covers
it, never on a timer, and rolls it back visibly only on an explicit rejection. This is how Replicache, Figma,
Linear and t3code's mobile outbox work, and onicode's doc-as-outbox and processed-command ledger already
carry most of it.

Reads come from replicas: chat docs from SQLite, synced by version vectors both ways on a `doc/{chatId}`
stream the host keeps open for every recent chat; file trees as host-pushed listings with a scan id, as in
Zed; per-device catalogs and recently opened files cached on disk and revalidated in the background. Links to
every known engine are dialled at start and on foreground, kept alive at 1 s pings while something live is on
screen, and reset on wake or a path change. The terminal keeps a mosh-style predictor that the host always
overrides. It trusts the first key at a shell-marked prompt and holds predictions back on a fast link.

Each target is stated in round trips plus a constant, or as zero network events before the echo. Seeded
virtual-time scenarios in `onicode-latency` over `onicode-net-sim` profiles (LAN to DERP, loss, roam, sleep)
check them, against a committed baseline that only ratchets down. Wall-clock budgets (frame time, SQLite open,
cold start) run as benches on a Mac. The research and the ranked plan with file:line maps stay in
`.research/local-feel.md` (not committed).
