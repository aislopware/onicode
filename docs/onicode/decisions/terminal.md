# Terminal: libghostty-vt in the engine

Terminals run on libghostty-vt, ghostty's VT parser and state without its renderer or PTY, through
aislopware/libghostty-rs. The dependency is pinned by rev because the engine's host and every viewer's mirror must
read the same snapshot format.

The engine hosts every terminal (`crates/term`, `TerminalHub`). The host's terminal is the authority: it alone
answers a program's queries (cursor and size reports, OSC 10/11/12 colours, OSC 52 copies) and encodes input. Keys,
paste and the wheel reach it as `TerminalInput` calls. It encodes them by its own modes: monocode's xterm keys with
the macOS editing shortcuts, or libghostty's encoder once a program turns on the Kitty keyboard. Attaching is
`WatchTerminal`, which sends a snapshot of the host's screen and then the bytes after it. A viewer too far behind
the topic's ring gets a fresh snapshot.

The snapshot comes in two items. First the part through libghostty's READY record, which is enough to paint. Then
the older scrollback, as a `History` event in an `Events` item that covers no sequence numbers. A long history
therefore never delays a remote viewer's first frame, and the cursor, the ring and resuming still see one snapshot.
The mirror paints the first part at once. When the history arrives it decodes the whole snapshot, replays the
output and resizes it took in the meantime, and keeps its scroll position.

Each viewer sends a `ViewerId` with its watch, input and resizes, minted once per app run. The PTY takes the size
of the viewer that typed last. Until someone types, or after every viewer that typed has stopped watching, the
viewer that sized it last wins. A sole viewer therefore behaves as if it were alone.

A viewer's mirror is display-only, with its PTY writes and effects off. It takes a new size only when the host's
stream says so, so a window resize shows one frame late, but the mirror never diverges from the host. The element
paints the mirror's dirty rows from libghostty's render state and keeps shaped rows by row id.

The viewer paints only what the host echoed; there is no local echo prediction. A mosh-style predictor was built
and deleted (2026-11-07, owner ruling: features that are error-prone and extremely complex go). Guessing where an
echo lands needs heuristics the host cannot confirm: without an input acknowledgement from the host, an earlier
key's echo can look like the confirmation of a later one, and a 50,000-case property run kept finding a key typed
at a `Password:` prompt painted, and a fast link measured as slow from queued bursts. Over a tailnet, echo is one
round trip, usually tens of milliseconds, so the gain did not pay for the risk.

Selection keeps alacritty 0.26's rules (`select.rs`): half-cell sides, separators for double-click, brackets, and
logical lines. libghostty only paints and copies the selection. A synchronized update a program leaves open shows
after 150 ms.

Shells get `TERM=xterm-ghostty` from the bundled terminfo, or `xterm-256color` when it cannot be installed. zsh, bash and
fish also get onicode's shell integration, which marks prompts and commands (OSC 133) and reports the working
directory (OSC 7).

Programs report their status with OSC 7501 (program status, rev 0.4). Claude Code and Codex both use it. Only the
host registers for these reports, so only the host answers the support query, and it keeps the records whether or
not anyone is watching (`program.rs`). The records follow the spec:

- There is one record per id, and each report replaces its record whole.
- A clear removes that id and every record beneath it. An empty id or a full reset removes them all.
- A new prompt (OSC 133 A) or the shell's exit drops working, blocked and idle records. Done and error records stay.
- At most 256 records are kept, and the least recently updated is dropped first.
- Text loses control characters and bidi overrides.

Viewers see a single record, the most urgent one (blocked, then working, error, done, idle). It travels in
`TerminalState`, in `TerminalSummary` and as `Status` events. `TerminalHub::statuses` broadcasts every change for
alerts. The terminal tab shows it with monocode's session marks: a spinner while working, the amber mark while
blocked, and the check for done until the tab has been seen. Otherwise the tab keeps its tile. When the foreground
command gives no agent, a report's `app` names it. These reports are display evidence only and never resolve an
approval or finish a turn.

Zig never enters the dev loop. CI builds `libghostty-vt.a` per target from the pinned fork commit, and
libghostty-rs downloads it by sha256 from `prebuilt.toml`. The ghostty fork is a slim patch queue on upstream
`main`.
