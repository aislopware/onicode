# One machine: files and clipboard move between peers as if they were local

The owner works across several Macs and an iPhone. Onicode builds only what makes them feel like one machine
in daily work. It skips polish that saves a click a week. Ranked by benefit:

1. **Files across machines.** A file explorer over every peer, with drag and drop between peers and to and
   from Finder, on a transfer engine fast enough that a copy across the tailnet feels like a local one.
2. **A clipboard that follows you.** Copy on one Mac and paste on another, with no button: instant, private,
   and loop-free.
3. **Agent alerts on every device,** with actions: approve, steer or stop from the notification
   (`alerts.md`).
4. **A localhost bridge.** A remote agent's or terminal's `open URL` opens on the device in use. Its listening
   ports are forwarded to the same local port, so OAuth loopback redirects and dev servers just work.
5. **Server Macs stay awake** while an agent runs on them (`keep-awake.md`).

Handoff, cross-machine search, a shared keyboard and mouse (Universal Control already covers nearby Macs), a
File Provider sidebar domain and a mirror-and-edit-locally sync are not built now. The sources and the
product survey are in `.research/one-machine.md` (local only).

## Transfers

Bytes move engine to engine, never through the UI's localhost IPC. The UI asks its own engine to start a
transfer, and the destination engine pulls from the source engine over `StreamKind::File` streams on the
existing peer link. Pushing a Finder drop to a remote folder is the remote engine pulling from ours.

The first byte leaves at once, with no pre-hash pass. WireGuard's AEAD already authenticates every packet,
so hashing a file before sending it buys no integrity and costs a full extra read. Both ends compute BLAKE3
while streaming, one digest per 16 MiB block, sent after the block. A file on four streams arrives out of
order, so no single running hash covers it; per-block digests are checked as each block lands, a mismatch
(which only disk or memory corruption could cause) refetches that block alone, and the blocks on disk are
what a resume trusts. A one-block file's digest is its BLAKE3. Content-defined chunking and rolling hashes are left out: Syncthing removed its
rolling hash after measuring a 0.8% saving, and rsync copies whole files locally by default.

Throughput:
- One connection per peer pair. Up to 8 files are in flight, each on its own stream.
- A file over 64 MiB that is alone is split into 16 MiB ranges across up to 4 streams.
- Files under 256 KiB are packed back to back on one stream.
- Stream and connection windows are sized to the bandwidth-delay product of a fast tailnet (16 MiB per stream,
  64 MiB per connection) instead of quinn's 100 Mbit/s × 100 ms defaults. They are measured between two Macs on the tailnet.
- zstd level 1 runs on a file only when a sample of its first 256 KiB shrinks below 90% and the link is
  slower than the compressor. Known compressed types skip the sample. The source judges while it walks:
  the first file of each extension decides for the rest, so a tree costs one sample per kind, and the
  manifest marks each item `zstd`.

Writes and resume:
- Data lands in `.onicode-partial-<transfer>` inside the destination folder, preallocated, and is renamed
  into place atomically.
- An interrupted transfer resumes from the ranges already on disk. The source's size and mtime must still
  match; otherwise it restarts that file.
- Jobs persist, so a transfer survives a dropped link or a restarted engine.

Metadata:
- Each item carries its mode, mtime and symlink target. `copyfile(COPYFILE_PACK)` serialises its xattrs,
  ACL, resource fork and Finder info, and `COPYFILE_UNPACK` restores them before the rename.
- Packages and bundles copy as directory trees.

Conflicts ask, as Finder does: replace, keep both or skip, with "apply to all".

Browsing reaches every path on a peer that belongs to the same owner, not only a chat's folder. Admission is
the tailnet whois the links already use.

Dragging a remote item out to Finder:
- It uses `NSFilePromiseProvider`, one per item, added to the gpui-fast fork. Finder names the destination
  folder, and the promise's write starts a transfer there and publishes an `NSProgress` with the file URL,
  so Finder draws the bar.
- Drops onto the explorer accept file URLs and file promises (`NSFilePromiseReceiver`, materialised in a
  staging folder first).

## Clipboard

The clipboard is eager and capped: a copy travels when it is made, so a paste never waits on the other Mac.
It was lazy at first, the way RDP's clipboard channel and X11 selections work (announce, then fetch on paste);
that was simplified on 2026-11-07 (`remote-desktop.md`), because answering a pasteboard promise means blocking
inside AppKit's callback, pumping the main run loop, for as long as the other Mac takes.

Announce and fetch:
- Each side polls its pasteboard's `changeCount` and, on a change, reads the copy and announces an offer. An
  offer carries the origin device, a sequence number, the types and sizes, and privacy flags.
- Items up to 16 KiB ride inside the offer, so most text lands with the offer itself. The rest, up to 16 MiB
  of bytes, is fetched at once on streams of its own, and the copy is written whole once it lands. A type past
  the cap stays home; the app's next-ranked type may still go.
- Regular files of 16 MiB or less in all are fetched into a cache folder before the copy is written. Bigger
  files and folders stay home.
- A copy that stays home past the caps is said where the owner is looking: the desktop window shows one line
  naming which Mac kept it and why, until a copy within the caps follows (`remote-desktop.md`).

Loops and focus:
- A received item is written with onicode's origin type and `currentHostOnly`. Its `changeCount` is
  remembered, so it is never announced back.
- The newest sequence wins.
- While a remote desktop window has focus, the clipboard follows that session both ways, as Deskflow's
  screen switch and Jump's active window do.

Privacy:
- Items marked `org.nspasteboard.ConcealedType` or `TransientType` are never sent. Password managers mark
  their copies this way.
- `AutoGeneratedType` items are sent only if the owner opts in. No history is kept.

Permission: macOS asks before a programmatic read of the general pasteboard, and that read happens on the
machine the owner is not looking at. So the grants panel walks the owner through allowing "Paste from Other
Apps" for onicode once, like Screen Recording. iOS can't read the pasteboard in the background: the phone
offers its clipboard when it comes to the foreground and pastes through `UIPasteControl`.

As built, for the remote desktop session (`remote-desktop.md`): the logic is `crates/clipboard`, its wire
`crates/desktop-proto/src/clipboard.rs`, the host side `crates/desktopd/src/clipboard.rs` and `session.rs`, and the
viewer side `crates/desktop-client/src/session.rs` and `crates/app/src/desktop/clipboard.rs`. Engines do not follow
the clipboard between Macs outside a desktop window.
- Offers order by `(at, origin, seq)`. `seq` breaks a tie inside one millisecond, and `at` is never behind
  the newest offer the Mac has seen, so a Mac whose clock runs behind still wins with a later copy.
- A fetch names the item as well as the type, `clip/<seq>/<item>/<type>`: a copy of several pictures has the
  same type in each item. `clip/<seq>/<type>` means item 0.
- Until "Paste from Other Apps" is allowed, the origin reads nothing, so nobody is prompted, and its copies
  stay there; the other side's copies still land. The guide to that pane raises the one prompt that lists
  desktopd there when it was never asked.
- A privacy marker on any item keeps the whole copy home, since some apps write the marker as an item of
  its own.
- A copy has 30 s to arrive in full; a newer copy on either Mac meanwhile wins, and a copy that never
  arrives leaves the board as it was.
- Tests keep the clipboard in memory. Only the running app and desktopd use the general pasteboard, and the
  `NSPasteboard` tests are opt-in (`ONICODE_PASTEBOARD_LIVE`) on uniquely named pasteboards.
