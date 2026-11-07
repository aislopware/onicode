# Rendering: aislopware/gpui-fast, macOS GUI

The app draws on aislopware/gpui-fast, pinned by rev on the fork's main (`Cargo.toml`). It replaces monocode's patched
GPUI snapshot (`vendor/gpui-pre*`, gpui 0.3.9 with monocode's renderer patches), which is deleted. gpui-fast is a
retained-mode GPUI: list and scroll layers composite cached tiles, and a view is drawn from the last frame unless
something it read changed. It is Onicode's own fork, so a missing effect is added at its root there, not worked around
in the app. It draws with Metal only, so the GUI is macOS (plus iOS through its own app); the engine and CLI build
without gpui and stay portable.

## Parity with monocode
monocode's look is kept exactly, so every renderer patch monocode carried was rebuilt in the fork (FORK.md "Onicode"):
backdrop blur, linear edge fades and the text reveal, rounded clips, the project background's layer mask with Haze,
drop shadows outside the box, 3:2 dashes, the role cursor, the wheel glide, content zoom and the atlas clear. The
golden images (`docs/onicode/golden.md`) prove it scene by scene against the snapshot: the plain scenes match exactly,
and what still differs is at most one level, listed there with its cause. The choices that took:

- **Group fades.** monocode fades a group by mixing a snapshot from before it with the result after it. The fork does
  the same (`with_group_fade`, `.group()`), and `edge_fade_by` fades over the bounds laid out in the same frame, so
  the app's own mask code is gone.
- **Backdrop blur.** Every blur is drawn whole (`paint_backdrop_blur_whole`), ordered above what its region overlaps
  and after the shadows painted before it, and mixed with what is under it in one pass through framebuffer fetch,
  rounded once as monocode's composite is. The app's `opacity(1/progress)` workaround in the sidebar peek is gone.
- **Blur over a scroll layer.** No ordering of a layer's cached tiles lets a blur read exactly what direct drawing
  shows, because what it must not read is known only from the frame's bounds tree. So a scroll layer that the last
  frame blurred over draws its standing content into the frame instead of compositing tiles, as list layers already
  do; nothing is repainted. The cost on a full view of rows at 1800×1200 is 244.5 µs of GPU time against 177.8 µs
  through tiles. The first frame a blur appears over a compositing layer still reads the tiles.
- **Dither.** Gradient dither is seeded by frame position, as monocode's was; a cached tile passes its frame origin to
  the shader, so tiles composited in place equal direct drawing. A tile reused after a scroll keeps the dither of
  where it was rasterised, up to two levels in gradient pixels.
- **View caching.** A cached view rebuilds when it is notified, not whenever a model it read is notified, as
  monocode's GPUI does; so typing and streaming no longer rebuild the sidebar, tabs or project rail. One extension
  is kept: a model changed under a notified parent shows on the next frame (a file dropped on the composer needs it).
  A cached view lays out from its snapped bounds, as monocode's did, so a view at a half pixel can move by one device
  pixel between a cached and an uncached frame, again as monocode's could.
- **Text.** CoreText shapes a line's first run at `next_up(size)`, as zed and monocode do.

gpui-kit is deferred. monocode's own widgets are kept as they are; a gpui-kit widget replaces one only where it draws
the same pixels.

## Retained mode
gpui-fast redraws a view only when something it read changed: an entity, a global, a list or scroll state, or the
window's pointer, modifiers or focus. So:

- Render reads entities, globals and window state. Anything else it reads (an `Rc<RefCell>`, a `Cell`, a
  thread-local, an `Arc<Mutex>`) is written together with a notify of the view that reads it. For a custom element,
  that view is `window.current_view()`, captured while drawing.
- The clock is not a dependency. Animations read `cx.frame_time()` and keep their own view drawing; they never drive
  a parent's or the root's frames.
- A notify raised while drawing never wakes the window. Send it through `window.on_next_frame` or `cx.defer`, and
  only when the value changed. Nothing writes an entity or a global on every frame.
- `window.refresh()` stays only where a change is truly window-wide: theme, settings and keyboard layout. It is
  dropped while the window draws, so a change found then refreshes nothing. The theme's colors live in a
  thread-local every view reads, so the shell paints them before the frame that shows a change: as it is notified
  (an app-level observer that only reads it) and as the system turns light or dark, refreshing the windows when
  they changed. A size a layout probe measures while drawing notifies the view drawing it through `cx.defer`.
- Work done in a render because monocode redrew everything on any notify has to be told: the shell reconciles
  notices as it renders, so its observer of each chat notifies it when the chat's turn or pending inputs move.
- `GPUI_VIEW_RETENTION=0` and `GPUI_CHECK_RETAINED=panic` (each retained frame compared with one drawn from
  scratch) settle whether retention is to blame. monocode's code was written for a GPUI that redrew on any notify,
  so the retained audit (`.research/monocode/stack-port.md` step 13) fixes each stale read at its source, and
  `scripts/ci/nextest.toml` sets `GPUI_CHECK_RETAINED=panic` for every test of the app (a nextest setup script),
  so each view test also proves its view is told of every change it shows. Turned on, it first failed 40 view
  tests; nearly all read the wall clock while drawing, so the scratch frame drew a later moment. Every render now
  reads `frame_time()` (the pulse loops through `pulse::now`), and view tests move animations with the executor's
  clock, never by sleeping. A test that forces `refresh()` before it looks draws from scratch and proves nothing
  about retention.
- A transcript row's measured height is kept across a resize that keeps the column's width, so a pane sliding beside
  it lays out no row again. The pane-slide bench (`pane_motion_frame_cost_with_long_history`, 10,000 messages, 80
  frames, release) went from CPU p50 11.9 ms to 7.5 ms (p95 8.1 ms), its samples including the second render the
  layout probes ask for while the panel moves.
