# Terminal: libghostty-vt

Terminals move from alacritty_terminal to libghostty-vt, ghostty's VT parser and state, without its renderer or
PTY, through aislopware/libghostty-rs. The engine keeps one authoritative terminal per PTY that answers queries and
produces snapshots. Each viewer keeps a mirror fed the same bytes with PTY writes and effects off. The
`Emulator` facade keeps its API, so the panel, view, mobile core and predictor change little. The GPUI element
reads dirty rows from the render state and caches shaped rows instead of reshaping every row every frame.

Zig never enters the dev loop: CI builds `libghostty-vt.a` per target from the pinned fork commit, and
libghostty-rs downloads it by sha256 from `prebuilt.toml`. The ghostty fork is kept as a slim patch queue on
upstream `main`.
