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
and held clocks for the two mid-motion scenes. `check` fails on any channel that moved, except by 1 level on an
antialiased edge (a pixel with a neighbour at least 16 levels away in the recorded image), the ring §1.1 expects to
differ; `ONICODE_GOLDEN_TOLERANCE=N` widens that to N everywhere while a port is in progress. Diffs (amber within the
allowance, red over it), the drawn images and per-scene stats (max channel delta, pixels over 1 level) go to
`$CARGO_TARGET_DIR/golden`. Re-record only for an intended UI change, in the same commit.
