# Terminal runtime

The environment server owns PTYs, session lifetime, and retained output. Every
client, including the desktop renderer, attaches through the environment connection.
This lets clients reconnect or share a running session. Renderer choices stay local
to each client and do not change terminal contracts.

## Output and retention

[Terminal history](../../apps/server/src/terminal/Manager.ts) is incremental.
PTY callbacks append new chunks; live events carry only those chunks. Materializing
or copying full scrollback on every callback makes output cost grow with retained
history, so snapshots and coalesced persistence are the materialization boundaries.
Persistence queues the mutable history buffer and reads its latest value when the
write runs. Clear, restart, and close must drain writes before completing their
lifecycle boundary.

Server history is capped at 5,000 lines and 8 MiB of UTF-8 text per terminal, so a
long unterminated line cannot bypass retention. Eviction removes the oldest output
without splitting Unicode code points; live output is not truncated. Release
discarded chunk references immediately, even if array compaction happens later.
Client buffers have a separate 512 KiB cap. Measure throughput with full scrollback
when changing this path.

Restoration must read only the bounded tail of current or legacy history files,
skip any incomplete UTF-8 prefix, and apply the line limit. Close the read handle
before rewriting the capped file. Reading whole old logs would defeat the memory
bound during startup.

## Renderer ownership

iOS, Android, and web use the same `libghostty-vt` C ABI for terminal behavior. The two
mobile platforms share one native core,
[`oni_terminal_core.cpp`](../../apps/mobile/modules/oni-terminal/cpp/oni_terminal_core.cpp),
which CMake (Android) and the pod (iOS) both compile. Platform adapters own drawing and input
integration, and React stays out of terminal frames.
The web adapter shares one WebAssembly instance per browser tab while each terminal
owns and frees its own handles. The canonical upstream pin is
[`native/libghostty-vt/VERSION`](../../native/libghostty-vt/VERSION); the iOS, Android,
and web artifacts must all be rebuilt when it changes. Web embeds the revision in its build
info so the ABI check can detect drift without a second pin.

Shell configs and TUIs often hardcode truecolor for one background, so every client
enforces a WCAG 4.5 minimum text contrast against the cell background. Web
([`contrast.ts`](../../apps/web/src/terminal/ghostty/contrast.ts)) and the mobile core
carry the same rule and exemptions and must change together. Theme changes go through
the terminal's default colors rather than repainting, because OSC 10/11/4 replies,
`CSI ? 996 n`, and the mode 2031 report that Neovim, Helix, and lazygit follow are all
derived from them.

Restoring scrollback must not send terminal replies to the current shell. Historical
device queries can otherwise provoke fresh replies that appear as junk at the
prompt. The server strips query/response traffic from retained history, and the
[web renderer](../../apps/web/src/terminal/ghostty/core.ts) detaches its PTY writer
during replay. Preserve both protections when changing retention or renderer code.
