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

monocode's widget layer moves into aislopware's gpui-kit fork (ruled 2026-11-07 by the owner, whole and not as a
pilot): gpui-kit becomes onicode's component library and wears monocode's look exactly, through a monocode theme
(colours, radii, spacing, fonts, shadows, blur and vibrancy materials, motion curves and durations) and, where a
token cannot say it, the component's own styling in the fork. Each monocode primitive (buttons, text input, popovers
and menus, dialogs, tooltips, pickers, tabs, lists, scrollbars, toggles, badges, toasts, progress and pulse, icons)
is replaced by the gpui-kit component only when onicode's golden images show no pixel moved; one gpui-kit lacks
moves into the fork as a new component. App views (composer, transcript, sidebar, terminal, diff, settings pages)
stay in the app and are rebuilt on those components. gpui-kit and onicode share one gpui: the fork depends on
aislopware/gpui-fast by the same rev, and follows the same platform ruling (macOS on Apple silicon and iOS, 27.2).
What onicode gains is one library with gpui-kit's input (IME), virtual lists and tables, keyboard navigation and
accessibility, and Longbridge's fixes upstream, under monocode's pixels.

Where it stands (2026-11-07): onicode depends on the fork's `gpui-component`, and `crates/app/src/kit.rs` holds
monocode's primitives built on it. Every button family (the dialog buttons, the settings pages' primary, secondary
and icon actions, the project settings' and Automations' buttons, the response actions and the title bar's chrome
icon buttons) is a gpui-kit `Button`, and both toggles are its `Switch`; all 50 golden images match with no pixel
moved. Three things in the fork made that possible: a custom button paints exactly the colours it is given (it
used to mix them 20% toward transparent and go transparent when pressed) and takes a hover text colour and a ring
with its own hover colour; a text size set on a button wins over its size's; and a switch takes its track, thumb
and whole-control dim from the app. gpui-kit sets itself up for every `App` through gpui-fast's
`on_every_app!` (`fast::app_init`), so the 452 GPUI tests, each with its own App, need no setup call. The kit
passes cargo-deny as onicode's own crates do, without a new skip: it is English only (rust-i18n went, with the
bitflags 1, itertools 0.11 and toml 0.8 it pinned), it speaks LSP through ls-types (lsp-types is unmaintained and
pinned bitflags 1), and gpui-fast's cbindgen 0.29 drops toml 0.8. MIT-0 (borrow-or-share, under ls-types) joins
the allowed licenses: it is MIT without the notice. What the
move gains already: the buttons take keyboard focus and press, a disabled one takes no click, and each has an
accessible name. The send button's morph stays in the app: it is a motion piece, its faces rolled through absolute
layers, not a primitive. Next, in order: tooltips, popovers and menus, the text input, pickers, tabs, lists and
scrollbars, badges, toasts, progress and pulse.

How the kit behaves is held to Base UI (mui/base-ui, watched in `scripts/upstream/sources.toml`), the web's best
unstyled library, so that a component onicode extends from the kit inherits interaction it would otherwise
rebuild. monocode decides how a component looks; Base UI decides what it does, and wins where it does better.
What moved in the kit (2026-11-07):
- The pointer travels to a hoverable popup (tooltip, hover card, submenu) inside a safe triangle. gpui-fast's
  `fast::hover_intent` judges the triangle; upstream's flat 500 ms closed a tooltip on a slow trip to its button.
- A popover or dialog that opens hands focus to its first control, and a non-modal popover closes when focus
  leaves it.
- A popup flips across its trigger when the other side has more room.
- Menus: open a submenu after a 100 ms rest, typeahead, Home/End. A checkable item stays open, a keyboard open
  highlights the first item, and press-drag-release picks the item under the pointer.
- Lists and menus keep one highlight: hover moves the keyboard cursor instead of painting a second one.
- Tabs, radio and toggle groups, toolbars and accordions share one roving focus. A toolbar is one Tab stop.
- Toasts announce at their priority.
- A focused scroller scrolls from the keyboard (gpui-fast's `fast::scroll_keys`).
- A popover can open on hover, leaving the focus where it is.
  - The pointer may travel to the popover. On the way it closes only once the pointer rests for 40 ms, as
    floating-ui's `safePolygon` does.
  - Pressing the trigger keeps a hover-opened popover open and gives it the focus.
  - HoverCard shares the same machinery (`hover_open.rs`).

- A select can put its selected item over the trigger (`align_item_with_trigger`), but not after touch input.
- A combobox takes its input in the field and shows chips when it allows several values.
- Toasts swipe away and F6 reaches them; the stack is a "Notifications" region.
- A presence that starts absent is absent at once. This fixes a gpui-base bug that drew the content of popovers that
  had never opened.

Popups move in and out, as Base UI's are mounted through their ending style (owner, 2026-11-07: Base UI wins over
monocode wherever it does better).
- Every popup (popover, dropdown menu, select, combobox, date picker, hover card, context menu) stays drawn until
  its exit ends. Reopening it on the way out turns the motion around; a context menu opened in place of another
  starts over.
- One motion, `popup_motion`: a 150 ms ease-out fade and an 8 pt slide out of the trigger's edge and back into it.
  A context menu and a select item lying over its trigger have no side, so they only fade.
- The shadow ramps by the cube of the fade, a menu's too, so it never shows through the fading panel (GPUI has no
  group opacity).
- A closing popup is inert: the pointer passes through it (gpui-fast's `fast::inert`, Base UI's
  `pointer-events: none` on a closing positioner). A press during the fade reaches what lies beneath and never picks
  an item of the menu on its way out.
- It is driven by gpui-base's `Presence`: `Popover::motion` with `PopoverState::motion_progress`,
  `HoverCard::motion`, and the kit's `dropdown_popup` for the dropdowns.

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

## Not rune, for now (2026-11-07)
The owner weighed moving the GUI to anomalyco/rune, because both gpui forks need heavy patching. A measured
evaluation on this Mac answered "not yet", and the owner kept gpui-fast and the gpui-kit plan. Rune met the bar in
three places: a 200×60 terminal grid takes 0.9 ms a frame, it idles at about 0% CPU, and its golden tests run
headless. It fails onicode's fixed requirements in five:
- It draws text with its own engine, not Core Text: 4–8% of SF text pixels differ, by up to 68 levels, against
  monocode's one-level goldens.
- Its input runs through winit, which drops the IME replacement ranges: Telex tone edits and Japanese reconversion
  fail.
- Its video surface takes no 4:4:4 and presents on the UI frame.
- It has no iOS.
- monocode, our only UI source, is gpui.

Rune also has no release, takes no PRs and has one main author. It becomes a candidate again when these hold: text
matches Core Text, it owns its macOS input, video presents off the UI frame in 4:4:4, iOS runs in CI, it has a
release policy, and a rune app ships publicly on macOS. It stays a reference in `scripts/upstream/sources.toml`.
Ideas worth taking are ported into gpui-fast:
- a motion gate that flags flicker and stale frames over time;
- warnings that name views rebuilt too often;
- skipping a present when the frame is unchanged.

All three landed in gpui-fast 3ea6b95 (`fast::same_frame`, `fast::churn`, `gpui::motion_gate`). The skip pays off:
a window notified 10 times a second that changes once a second presents 1 frame a second instead of 9.7, and its
process CPU drops from 25 to 13.5 ms a second. Notified 30 times a second for nothing, it drops from 57 to 19 ms.

The evaluation is in `.research/rune/EVALUATION.md`, kept outside the repo.
