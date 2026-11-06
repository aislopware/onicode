# Golden images

`scripts/dev/golden.sh record|check [TEST]` draws the UI headlessly on the GPUI stack the app builds with, through
`HeadlessAppContext::capture_screenshot` into an offscreen Metal texture, so the M2 move from the vendored snapshot to
gpui-fast (`.research/monocode/stack-port.md` §6) can be diffed pixel by pixel. The scenes live in
`crates/app/src/shell/golden_tests.rs` as `#[ignore]` tests that also do nothing without `ONICODE_GOLDEN`, so
`cargo t` never runs them; each draws a dark and a light image into `crates/app/tests/golden/<scene>-<theme>.png`:
the shell (sidebar labels, tabs, the inset card's corners, composer glass), narrow faded tab labels, the transcript
mid-scroll with both edges faded, a session menu, a modal, a toast, a bare popover card, a dashed draft bubble, the
sidebar peek halfway open, interface scale 125%, the title reveal at 300 and 700 ms, SF labels in three weights, and the
project background empty or filled, plain or with Haze. They are deterministic: a fixed window at the test window's 2x
scale, system fonts, an in-memory store without timestamps, reduced motion so every entrance rests on its last frame,
and held clocks for the two mid-motion scenes. Two more things keep them fixed: the window clears opaque whatever its
background, as monocode's test window always did, and the harness claims the agent-update check before any scene draws,
so no toast shows the machine's installed CLI versions. `check` fails on any channel that moved, except by 1 level on an
antialiased edge (a pixel with a neighbour at least 16 levels away in the recorded image), the ring §1.1 expects to
differ; `ONICODE_GOLDEN_TOLERANCE=N` widens that to N everywhere while a port is in progress. Diffs (amber within the
allowance, red over it), the drawn images and per-scene stats (max channel delta, pixels over 1 level) go to
`$CARGO_TARGET_DIR/golden`. Re-record only for an intended UI change, in the same commit.

The ten dark images were recorded again on the monocode stack (7152d2bd) with those two fixes, since the first
recording held the update toast; the light ones came out byte-identical. On gpui-fast (FORK.md "Onicode",
`onicode-parity-3`) the plain scenes match exactly: labels, the title reveal and all eight project backgrounds. Every
other scene differs by at most 1 level and still fails `check`. A dark scene keeps 4 to 9 pixels (24 in
`ui-scale-125-dark`): two at each corner of the clipped card, along its outer antialiased step, whose low contrast the
16-level edge test does not count; the frosted composer's corner ring is gone since a backdrop blur mixes with what is
under it rounded once, as monocode's composite does. A light scene keeps about 31 pixels over the tolerance (115 in
`ui-scale-125-light`), glyph interiors a level off in the workspace line under the composer: gpui-fast draws the
composer's 20 px shadow (draw order 4) after that text (order 3), and the drawn values fit the shadow blended over the
text, monocode's the text blended over the shadow, so monocode orders that shadow below the text; why is not found yet.
`ui-scale-125`'s activity tile icons sit where monocode drew them since a cached view lays out from its snapped
bounds again, as monocode's did (it had placed them a device pixel lower, at the half pixel the tile lies at in
place). The project background spans the view's width at the image's aspect from the top, which is what monocode's
taffy 0.13 made of `img().size_full()` (taffy 0.15 resolves the height to the view's and centres the cover), so the
app now lays that out explicitly.
