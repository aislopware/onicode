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

A viewer's mirror is display-only, with its PTY writes and effects off. It takes a new size only when the host's
stream says so, so a window resize shows one frame late, but the mirror never diverges from the host. The element
paints the mirror's dirty rows from libghostty's render state and keeps shaped rows by row id.

Selection keeps alacritty 0.26's rules (`select.rs`): half-cell sides, separators for double-click, brackets, and
logical lines. libghostty only paints and copies the selection. A synchronized update a program leaves open shows
after 150 ms.

Shells get `TERM=xterm-ghostty` from the bundled terminfo, or `xterm-256color` when it cannot be installed. zsh, bash and
fish also get onicode's shell integration, which marks prompts and commands (OSC 133) and reports the working
directory (OSC 7). Nothing reads those marks yet.

Zig never enters the dev loop. CI builds `libghostty-vt.a` per target from the pinned fork commit, and
libghostty-rs downloads it by sha256 from `prebuilt.toml`. The ghostty fork is a slim patch queue on upstream
`main`.
