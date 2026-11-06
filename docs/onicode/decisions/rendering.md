# Rendering: aislopware/gpui-fast, macOS GUI

The GUI moves from zeron's zui to aislopware/gpui-fast (retained mode) and from `gpui-base` to aislopware/gpui-kit.
The fork already carries the zui features zeron relies on: asset eviction, list tail reservation, rounded clips,
backdrop blur, transformed glyphs, masked images and present callbacks. What remains is the edge fade API, the
browser's native composition, blur under native views and call-site drift from zed.

gpui-fast draws those effects with Metal only, so the GUI becomes macOS only (plus iOS through its own app). The
engine and CLI stay portable and build without gpui, so Linux machines can still serve as headless peers.
Retained mode's risk is a view that silently stops updating; the switch is proved by A/B runs of the same build
with view retention off and pixel-identity checks.

## Browser on native composition

The browser's WKWebView now lives in a gpui-fast native host under GPUI's single Metal layer: the page creates its
host once (`create_native_host`, `attach_view`) and the tab renders `native_view(&host)` tracking a focus handle of its
own. GPUI cuts the hole, clips and orders the page with the frame, routes the pointer by its own hit test and keeps the
keyboard in step both ways, so everything zui's scheme needed went: the clip view and its mask layer, the scene overlay
plane, the per-present `on_present`/`Host::sync` geometry, the resize inset and right occlusion the shell pushed in,
`release_keyboard`/`focus_chrome` calls, and the local key monitor with its shortcut list (keys reach GPUI's keymap
first, unmatched equivalents reach WebKit). A drag keeps the pointer off the page by an occluding child while the shell
says `Passthrough`. The mouse-wheel glide stays: AppKit still hands the wheel straight to the WKWebView where it is the
topmost hit, so GPUI's smooth scroll never sees it and WebKit would apply a line tick at once. The fixture now reads
placement, hit testing and native blur from `Window::presented_natives()` and `native_hit_map()`. The Linux browser
(WebKitGTK and WPE helpers) is deleted with the Linux GUI; elsewhere an address opens in the default browser.

## Retained mode

gpui-fast draws a view from the last frame unless something it read changed: an entity, a global, a list or scroll
state, or the window's pointer, modifiers or focus. Before the switch, any notify redrew the whole window. That hid
every input a render read without telling anyone, so onicode now keeps all such inputs visible to the reading view or
notifies that view itself:

- Render reads entities, globals and window state. Anything else it reads (an `Rc<RefCell>`, a `Cell`, a
  thread-local, an `Arc<Mutex>`) is written together with a notify of the view that reads it. For a custom element,
  that view is `window.current_view()`, captured while drawing.
- The clock is not a dependency. A time label wakes its own view when its text next changes
  (`label_clock::LabelTick`). Animations read `cx.frame_time()` and keep their own view drawing: through the pulse
  clock lease, `request_animation_frame`, or the fade pump. They never drive a parent's or the root's frames, because
  the root no longer renders every frame.
- A notify raised while drawing (prepaint or paint) never wakes the window. Send it through `window.on_next_frame`
  or `cx.defer`, and only when the value changed.
- Nothing writes an entity or a global on every frame. Compare before writing. Bookkeeping that no view displays sits
  behind cells in a global set once (`motion::PulseClock`).
- `window.refresh()` and `cx.refresh_windows()` stay only where the change is truly window-wide or app-wide:
  - theme, settings and keyboard-layout changes;
  - markdown text selection, which is one thread-local spanning every view and whose hit registry only a full paint
    refills;
  - hover colour fades (`motion::pump_fades`), because their store is keyed by element, not by view, so the window is
    redrawn whole until the fade settles. Moving those fades to gpui-fast's `StateTransition` would remove the
    refresh.
- Tests run with retention on. Draw through notifies, not `window.refresh()`, and turn on
  `window.set_check_retained(Some(RetainedCheck::Panic))` so that each retained frame is compared with one drawn
  from scratch. In the app, `GPUI_VIEW_RETENTION=0` and `GPUI_CHECK_RETAINED=panic` settle whether retention is to
  blame.

Changed for this:

- **Toasts.** Toasts are their own `Toaster` view, so their per-frame store writes and animation frames no longer
  rebuild the shell. Pushes, dismissals and expiry wake every window's toaster by notify, and a height measured in
  prepaint is told on the next frame.
- **Hover fades and glyph morphs.** These were driven from the shell's render tail, which assumed the root renders
  every frame. Hover fades now run their own pump, which prunes unread entries only on whole-window frames. Glyph
  morphs (`motion::state_t`) lease the pulse clock for the view that reads them.
- **Pulse clock.** It is written once and runs on the executor's clock and the frame time, so every spinner render
  no longer marks every spinner changed, and tests can advance it.
- **Link menus, popups and tooltips** (`markdown/link_interaction.rs`). These notify their own view instead of
  refreshing the window. A scroll redraws only when there was something to hide; before, every scroll event refreshed
  the whole window once per painted link run.
- **Git history column preferences.** History views observe the shared global instead of refreshing every window on
  each mouse move of a column drag.
- **Time labels.** The sidebar, lane, pull-request, device and archive "ago" labels, and the transcript's working
  timer, wake their own view when their text next changes.
- **Measurements told from paint or prepaint.** Transcript bubble heights, picker trigger geometry and image
  preview bounds are now sent on the next frame, so the window wakes and shows them.
- **Writes the shell made on every render.** `settings::update`, the wallpaper preload queue, and Shell's writes into
  Composer, Transcript, the session hover card and the picker bounds now write only when the value changed.
- **Still open: views drawn through `parent.update(..)` in their own render.** `SidebarPane` and the session card
  are the main cases. Each such draw counts as a write to the parent, so it rebuilds the parent too. That costs time
  but does not go stale, and fixing it needs render helpers that take `&Shell`.
