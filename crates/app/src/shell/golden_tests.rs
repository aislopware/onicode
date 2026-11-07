//! Golden images of the UI, drawn on the GPUI stack the app builds with, so a stack switch can be
//! diffed pixel by pixel (docs/onicode/golden.md).
//!
//! Opt-in twice over: every test is `#[ignore]`, and does nothing unless `ONICODE_GOLDEN` is
//! `record` or `check`; `scripts/dev/golden.sh` sets both. Rendering is headless, through
//! `HeadlessAppContext::capture_screenshot` into an offscreen Metal texture: no window opens, takes
//! focus, prompts or plays sound.
//!
//! Every scene is deterministic: a fixed window size at the test window's 2x scale, system fonts
//! only (`.SystemUIFont`, Menlo), an in-memory store with no timestamps, and frozen motion. Reduced
//! motion lands every entrance (popovers, modals, scrims, pane slides, spinners) on its resting
//! frame; the three mid-motion scenes hold their own clocks: the sidebar peek through
//! `SidebarPeek::held_opening`, the title reveal through `particle_text::hold_clock`, stepped at
//! 60 fps up to the captured instant, and the spinners through the headless executor's virtual
//! clock. The code scenes read only a scratch project the test writes into a temp dir, and a
//! scene that opens a control presses it where it is drawn (`press`) or drives it from the
//! keyboard's actions, as a person would.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyWindowHandle, HeadlessAppContext, ListOffset, Modifiers, PlatformInput, PlatformTextSystem,
    WindowBackgroundAppearance, WindowHandle, point, size,
};
use image::{Rgba, RgbaImage};
use onicode_protocol::project_appearance::{Background, Effect};
use onicode_protocol::transcript::{Row, Transcript};

use super::*;
use crate::project_background::ProjectBackground;

#[path = "golden_sources.rs"]
mod sources;

/// The shell's window, in points; the test window draws it at 2x.
const SHELL: (f32, f32) = (1000., 660.);
/// Narrow enough that every title tab fades its label.
const SHELL_NARROW: (f32, f32) = (840., 660.);
/// Tall enough for Settings' first groups below an open select.
const SHELL_TALL: (f32, f32) = (1000., 800.);
/// The code views' windows: the editor, the diff and the terminal.
const CODE: (f32, f32) = (760., 520.);
/// The virtual time the spinners run before they are captured.
const SPIN: Duration = Duration::from_millis(400);
const PROJECT: &str = "/work/onicode";
const ACTIVE_TITLE: &str = "Golden images for the stack port";
/// Long titles, so each tab label runs into its fade.
const TABS: [(&str, &str); 3] = [
    ("codex", "Port the linear edge fade into gpui-fast"),
    (
        "claude",
        "Reproduce the screenshot thumbnail freeze on the display link",
    ),
    ("codex", "Rewrite the Metal marker tests on the new APIs"),
];
const TURNS: [(&str, &str); 4] = [
    (
        "How does the transcript fade its top and bottom edges?",
        "The transcript list sits inside a group that snapshots what is drawn before and after it, \
         then mixes the two with a **linear** ramp at each edge. The ramp only appears once there \
         is content past that edge, so a list resting at the bottom keeps a crisp lower edge.\n\n\
         - The top ramp is 24 points tall.\n\
         - The bottom ramp stops at the composer's shadow.\n\
         - Both use `min(top, bottom)`, never their product.",
    ),
    (
        "Show me where the marker is painted.",
        "It is a zero-size quad whose border colour carries the marker, painted before and after \
         the list:\n\n\
         ```rust\n\
         window.paint_quad(fill(bounds, black()).border_color(edge_fade_marker(begin, top, bottom)));\n\
         ```\n\n\
         The renderer recognises the colour, ends the current pass and records the snapshot.",
    ),
    (
        "And the glass behind popovers?",
        "A popover paints a 2% content tint over a backdrop blur of 24 points, then a 10% hairline \
         border on top. Light themes paint an opaque card instead, since frosted white reads as \
         grey. The drop shadow is clipped to the outside of the card, so the glass never darkens \
         itself.",
    ),
    (
        "What should the golden images cover?",
        "Every place the vendored renderer differs from upstream:\n\n\
         1. Backdrop blur under popovers, modals, toasts and the sidebar peek.\n\
         2. Edge fades on the transcript, the sidebar and tab labels.\n\
         3. The title reveal, the inset card's corners and the project background.\n\
         4. Dashed draft bubbles, shadows and medium-weight labels.",
    ),
];
/// The edge fade allowance: a pixel sits on an antialiased edge when a neighbour in the recorded
/// image is at least this many levels away from it in some channel.
const EDGE_CONTRAST: u8 = 16;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Record,
    Check,
}

fn mode() -> Option<Mode> {
    match std::env::var("ONICODE_GOLDEN").as_deref() {
        Ok("record") => Some(Mode::Record),
        Ok("check") => Some(Mode::Check),
        Ok("") | Err(_) => None,
        Ok(other) => panic!("ONICODE_GOLDEN={other}: expected record or check"),
    }
}

/// Levels per channel any pixel may differ by; antialiased edges always get at least 1.
fn tolerance() -> u8 {
    std::env::var("ONICODE_GOLDEN_TOLERANCE").map_or(0, |value| {
        value
            .parse()
            .unwrap_or_else(|_| panic!("ONICODE_GOLDEN_TOLERANCE={value}: expected 0-255"))
    })
}

#[derive(Clone, Copy)]
enum Look {
    Dark,
    Light,
}

impl Look {
    const BOTH: [Self; 2] = [Self::Dark, Self::Light];

    fn name(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    fn preferences(self) -> crate::settings::Preferences {
        crate::settings::Preferences {
            theme: self.name().into(),
            ..Default::default()
        }
    }
}

/// Draws each look's scene in a fresh headless app, then records or checks it as
/// `<scene>-<look>.png`. Every look runs before a failure is reported, so both diffs are written.
fn run(scene: &str, draw: impl Fn(&mut HeadlessAppContext, Look) -> AnyWindowHandle) {
    let Some(mode) = mode() else {
        eprintln!("ONICODE_GOLDEN is unset: skipped (scripts/dev/golden.sh record|check)");
        return;
    };
    let mut failures = Vec::new();
    for look in Look::BOTH {
        let mut cx = headless();
        crate::theme::configure(&look.preferences(), false);
        let window = draw(&mut cx, look);
        // The recording stack's test window cleared opaque whatever the window's background, so
        // the dark window's glass is drawn over opaque black as it was recorded.
        cx.update_window(window, |_, window, _| {
            window.set_background_appearance(WindowBackgroundAppearance::Opaque);
        })
        .expect("window is open");
        settle(&mut cx, window);
        let image = cx
            .capture_screenshot(window)
            .expect("the headless Metal renderer captures the window");
        let name = format!("{scene}-{}", look.name());
        let result = match mode {
            Mode::Record => record(&name, &image),
            Mode::Check => check(&name, &image),
        };
        if let Err(failure) = result {
            failures.push(failure);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The app as `main` sets it up for drawing, on the test platform with real CoreText shaping and a
/// headless Metal renderer.
fn headless() -> HeadlessAppContext {
    // The launch's harness update check reads the machine's installed agent CLIs; claiming it
    // keeps their versions out of the scenes.
    onicode_core::harness_updates::claim_launch_check();
    let text: Arc<dyn PlatformTextSystem> = Arc::new(gpui_apple::AppleTextSystem::new());
    let mut cx = HeadlessAppContext::with_platform(
        text.clone(),
        Arc::new(crate::assets::Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        cx.set_reduce_motion(true);
        cx.set_global(crate::particle_text::GlyphRaster(text));
    });
    cx
}

fn open<V: Render + 'static>(
    cx: &mut HeadlessAppContext,
    (width, height): (f32, f32),
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V>,
) -> WindowHandle<V> {
    let window = cx
        .open_window(size(px(width), px(height)), build)
        .expect("headless window");
    settle(cx, window.into());
    window
}

/// Draws until loads, measurements and highlighting have landed.
fn settle(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    for _ in 0..6 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, _| window.refresh())
            .expect("window is open");
    }
    cx.run_until_parked();
}

/// Delivers what a platform frame would to `Window::on_next_frame` callbacks, then settles: views
/// that measure what they drew before showing something over it wait for that frame.
fn next_frame(cx: &mut HeadlessAppContext, window: AnyWindowHandle) {
    cx.update_window(window, |_, window, cx| {
        window.simulate_next_frame(cx);
        window.refresh();
    })
    .expect("window is open");
    settle(cx, window);
}

/// Clicks the element assistive technology names `label`, at the middle of where it is drawn, as
/// a pointer would. The accessibility tree is the one map of drawn bounds GPUI opens to a headless
/// test, so it is built for the frames that find the element and dropped before the click lands.
fn press(cx: &mut HeadlessAppContext, window: AnyWindowHandle, label: &str) {
    cx.update_window(window, |_, window, _| window.set_a11y_active(true))
        .expect("window is open");
    settle(cx, window);
    let position = cx
        .update_window(window, |_, window, _| {
            let tree = window
                .a11y_tree()
                .expect("an accessibility tree once a frame drew with it active");
            let bounds = tree
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(label))
                .and_then(|(_, node)| node.bounds())
                .unwrap_or_else(|| panic!("nothing named {label:?} is drawn"));
            // The tree's bounds are in device pixels.
            let scale = f64::from(window.scale_factor());
            point(
                px(((bounds.x0 + bounds.x1) / 2. / scale) as f32),
                px(((bounds.y0 + bounds.y1) / 2. / scale) as f32),
            )
        })
        .expect("window is open");
    cx.update_window(window, |_, window, cx| {
        window.set_a11y_active(false);
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
    })
    .expect("window is open");
    settle(cx, window);
}

/// A view on the window's surface, as the shell's panes lay one out.
struct Framed<V: 'static>(Entity<V>);

impl<V: Render> Render for Framed<V> {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(surface())
            .font_family(crate::fonts::UI)
            .text_color(content(0.9))
            .child(self.0.clone())
    }
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Diffs and stats go beside the build, in `<target>/golden`.
fn out_dir() -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            // <target>/<profile>/deps/<test binary>
            let exe = std::env::current_exe().ok()?;
            exe.ancestors().nth(3).map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from("target"));
    target.join("golden")
}

fn record(name: &str, image: &RgbaImage) -> Result<(), String> {
    let dir = golden_dir();
    std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    let path = dir.join(format!("{name}.png"));
    image
        .save(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    println!("recorded {}", path.display());
    Ok(())
}

#[derive(Default)]
struct Stats {
    max_delta: u8,
    differing: usize,
    over_one: usize,
    failing: usize,
}

fn check(name: &str, actual: &RgbaImage) -> Result<(), String> {
    let out = out_dir();
    std::fs::create_dir_all(&out).map_err(|error| format!("{}: {error}", out.display()))?;
    let save = |suffix: &str, image: &RgbaImage| {
        let path = out.join(format!("{name}.{suffix}.png"));
        if let Err(error) = image.save(&path) {
            eprintln!("{}: {error}", path.display());
        }
    };
    let golden = golden_dir().join(format!("{name}.png"));
    let expected = match image::open(&golden) {
        Ok(expected) => expected.into_rgba8(),
        Err(error) => {
            save("actual", actual);
            return Err(format!(
                "{name}: no golden image at {} ({error}); record with scripts/dev/golden.sh record",
                golden.display()
            ));
        }
    };
    if expected.dimensions() != actual.dimensions() {
        save("actual", actual);
        return Err(format!(
            "{name}: drawn at {:?}, recorded at {:?}",
            actual.dimensions(),
            expected.dimensions()
        ));
    }
    let tolerance = tolerance();
    let (stats, diff) = compare(&expected, actual, tolerance);
    let report = format!(
        "{name}: max channel delta {}, {} pixels over 1 level, {} differing, {} over the tolerance \
         ({tolerance}, 1 on antialiased edges)",
        stats.max_delta, stats.over_one, stats.differing, stats.failing
    );
    if let Err(error) = std::fs::write(out.join(format!("{name}.stats.txt")), format!("{report}\n"))
    {
        eprintln!("{name}: stats: {error}");
    }
    println!("{report}");
    if stats.differing > 0 {
        save("actual", actual);
        save("diff", &diff);
    }
    if stats.failing > 0 {
        Err(report)
    } else {
        Ok(())
    }
}

/// Compares two images of one size. The diff shows matching pixels as a dim copy of the golden
/// image, differences within the allowance in amber and failing ones in red.
fn compare(expected: &RgbaImage, actual: &RgbaImage, tolerance: u8) -> (Stats, RgbaImage) {
    let (width, height) = expected.dimensions();
    let mut stats = Stats::default();
    let mut diff = RgbaImage::new(width, height);
    for (x, y, want) in expected.enumerate_pixels() {
        let delta = channel_delta(want, actual.get_pixel(x, y));
        let allowed = if on_edge(expected, x, y) {
            tolerance.max(1)
        } else {
            tolerance
        };
        stats.max_delta = stats.max_delta.max(delta);
        stats.differing += usize::from(delta > 0);
        stats.over_one += usize::from(delta > 1);
        stats.failing += usize::from(delta > allowed);
        let shade = luma(want) / 4;
        let pixel = match delta {
            0 => Rgba([shade, shade, shade, 255]),
            delta if delta <= allowed => Rgba([255, 191, 0, 255]),
            _ => Rgba([255, 0, 64, 255]),
        };
        diff.put_pixel(x, y, pixel);
    }
    (stats, diff)
}

fn channel_delta(a: &Rgba<u8>, b: &Rgba<u8>) -> u8 {
    a.0.iter()
        .zip(b.0)
        .map(|(a, b)| a.abs_diff(b))
        .max()
        .unwrap_or(0)
}

fn on_edge(image: &RgbaImage, x: u32, y: u32) -> bool {
    let here = image.get_pixel(x, y);
    let (width, height) = image.dimensions();
    let columns = x.saturating_sub(1)..=(x + 1).min(width - 1);
    (y.saturating_sub(1)..=(y + 1).min(height - 1)).any(|ny| {
        columns
            .clone()
            .any(|nx| channel_delta(here, image.get_pixel(nx, ny)) >= EDGE_CONTRAST)
    })
}

fn luma(pixel: &Rgba<u8>) -> u8 {
    let [r, g, b, _] = pixel.0;
    let luma = (u16::from(r) * 3 + u16::from(g) * 6 + u16::from(b)) / 10;
    u8::try_from(luma).unwrap_or(u8::MAX)
}

/// The window's surface where a scene has no shell behind it.
fn surface() -> gpui::Hsla {
    let mut fill = shell_fill();
    fill.a = 1.;
    fill
}

fn row(role: &str, text: &str) -> Row {
    Row {
        role: role.into(),
        text: text.into(),
        first: true,
        sent_at: None,
        id: None,
    }
}

/// Prose, a list and code, then the reference tool layout (`fixtures::chat`).
fn history() -> Vec<Row> {
    let mut transcript = Transcript::default();
    for (prompt, answer) in TURNS {
        transcript.begin("user");
        transcript.append(prompt);
        transcript.begin("assistant");
        transcript.append(answer);
    }
    let mut rows = transcript.rows;
    rows.extend(crate::fixtures::chat().rows);
    for row in &mut rows {
        row.sent_at = None;
    }
    rows
}

/// The shell, with an optional overlay drawn over the whole window as a glass test card.
struct Stage {
    shell: Entity<Shell>,
    overlay: Option<fn() -> AnyElement>,
}

impl Render for Stage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .child(self.shell.clone())
            .children(self.overlay.map(|overlay| overlay()))
    }
}

/// A project with three long-titled tabs and an active chat whose history scrolls.
struct Fixture {
    window: WindowHandle<Stage>,
    shell: Entity<Shell>,
    active: i64,
}

impl Fixture {
    fn new(
        cx: &mut HeadlessAppContext,
        look: Look,
        window_size: (f32, f32),
        edit: impl FnOnce(&mut Workspace),
        overlay: Option<fn() -> AnyElement>,
    ) -> Self {
        let engine = cx.update(Engine::memory);
        let store = engine.seed();
        let mut tabs = Vec::new();
        for (provider, title) in TABS {
            let id = store.create(provider, PROJECT).expect("session").id;
            store.title(id, title).expect("title");
            store
                .write_rows(id, 0, &[row("user", title)])
                .expect("rows");
            tabs.push(id);
        }
        let active = store.create("claude", PROJECT).expect("session").id;
        store.title(active, ACTIVE_TITLE).expect("title");
        store.write_rows(active, 0, &history()).expect("rows");
        let mut summaries = store.sessions().expect("sessions");
        for summary in &mut summaries {
            summary.has_messages = true;
        }
        // The rounded frame: its inset card clips its corners.
        let mut state = Workspace {
            new_layout_boxy: false,
            ..Default::default()
        };
        state.preferences = look.preferences();
        state.add_project(PROJECT);
        for id in tabs {
            state.open(id);
        }
        state.open(active);
        edit(&mut state);
        let saved = serde_json::to_string(&state).expect("workspace state");
        let window = open(cx, window_size, move |_, cx| {
            let shell = cx.new(|cx| Shell::new(engine, summaries, PROJECT.into(), Some(saved), cx));
            cx.new(|_| Stage { shell, overlay })
        });
        let shell = window
            .update(cx, |stage, _, _| stage.shell.clone())
            .expect("window is open");
        Self {
            window,
            shell,
            active,
        }
    }

    fn with_shell<R>(
        &self,
        cx: &mut HeadlessAppContext,
        f: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) -> R,
    ) -> R {
        let shell = self.shell.clone();
        cx.update_window(self.window.into(), |_, window, cx| {
            shell.update(cx, |shell, cx| f(shell, window, cx))
        })
        .expect("window is open")
    }

    fn handle(&self) -> AnyWindowHandle {
        self.window.into()
    }
}

/// The plain shell: sidebar labels, title tabs, the inset card's rounded corners and the
/// composer's glass over the end of the chat.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn shell() {
    run("shell", |cx, look| {
        Fixture::new(cx, look, SHELL, |_| {}, None).handle()
    });
}

/// Every tab label fades instead of ending in an ellipsis.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn tabs_narrow() {
    run("tabs-narrow", |cx, look| {
        Fixture::new(cx, look, SHELL_NARROW, |_| {}, None).handle()
    });
}

/// The transcript scrolled to its middle, so both edges fade.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn transcript_mid_scroll() {
    run("transcript-mid-scroll", |cx, look| {
        let fixture = Fixture::new(cx, look, SHELL, |_| {}, None);
        let active = fixture.active;
        fixture.with_shell(cx, |shell, _, cx| {
            let transcript = shell.sessions[&active].read(cx).transcript.clone();
            transcript.update(cx, |transcript, cx| {
                let middle = transcript.list.item_count() / 2;
                transcript.list.scroll_to(ListOffset {
                    item_ix: middle,
                    offset_in_item: px(24.),
                });
                cx.notify();
            });
        });
        fixture.handle()
    });
}

/// A session's menu: popover glass over the transcript.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn menu() {
    run("menu", |cx, look| {
        let fixture = Fixture::new(cx, look, SHELL, |_| {}, None);
        let active = fixture.active;
        fixture.with_shell(cx, |shell, window, cx| {
            let anchor = Anchor::Point(point(px(420.), px(150.)));
            shell.open_menu(MenuTarget::Session(active), anchor, window, cx);
        });
        fixture.handle()
    });
}

/// A standard modal: scrim, frosted card, its shadow and buttons.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn modal() {
    fn overlay() -> AnyElement {
        let card = crate::modal::card().child(
            crate::modal::card_body()
                .child(crate::modal::title("Archive this chat?"))
                .child(crate::modal::body(
                    "It leaves the sidebar but stays searchable, and you can restore it later.",
                ))
                .child(
                    crate::modal::actions()
                        .child(crate::modal::ghost_button("golden-cancel", "Cancel"))
                        .child(crate::modal::primary_button("golden-archive", "Archive")),
                ),
        );
        crate::modal::overlay("golden-modal", card, |_, _| {}).into_any_element()
    }
    run("modal", |cx, look| {
        Fixture::new(cx, look, SHELL, |_| {}, Some(overlay)).handle()
    });
}

/// A toast in the top-right stack: dashed glass frame over the chat.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn toast() {
    fn overlay() -> AnyElement {
        div()
            .absolute()
            .top(px(52.))
            .right(px(16.))
            .w(px(340.))
            .child(
                crate::notifications::surface::toast_frame().child(
                    div()
                        .relative()
                        .p(px(12.))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .child("Golden images recorded"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(content(0.6))
                                .child("34 scenes on the vendored GPUI stack"),
                        ),
                ),
            )
            .into_any_element()
    }
    run("toast", |cx, look| {
        Fixture::new(cx, look, SHELL, |_| {}, Some(overlay)).handle()
    });
}

/// A saved draft: the dashed bubble at the end of the chat.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn draft() {
    run("draft", |cx, look| {
        let fixture = Fixture::new(cx, look, SHELL, |_| {}, None);
        let active = fixture.active;
        fixture.with_shell(cx, |shell, _, cx| {
            let transcript = shell.sessions[&active].read(cx).transcript.clone();
            transcript.update(cx, |transcript, cx| {
                let draft = onicode_protocol::follow_up::Message {
                    id: "golden-draft".into(),
                    text: "Record the golden images before the stack switch.".into(),
                    at: usize::MAX,
                    ..Default::default()
                };
                transcript.set_saved_draft(Some(draft), cx);
                transcript.list.scroll_to_end();
            });
        });
        fixture.handle()
    });
}

/// The collapsed sidebar peeking out, held halfway through its opening curve.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn peek_mid_slide() {
    run("sidebar-peek", |cx, look| {
        let fixture = Fixture::new(cx, look, SHELL, |state| state.sidebar_visible = false, None);
        fixture.with_shell(cx, |shell, _, cx| {
            shell.sidebar_peek = Some(sidebar_peek::SidebarPeek::held_opening(0.5));
            shell.peek_animating = true;
            cx.notify();
        });
        fixture.handle()
    });
}

/// Settings → Interface scale at 125%, through the window's content zoom.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn ui_scale() {
    run("ui-scale-125", |cx, look| {
        Fixture::new(
            cx,
            look,
            SHELL,
            |state| state.preferences.ui_scale = 125,
            None,
        )
        .handle()
    });
}

struct Title(&'static str);

impl Render for Title {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .px(px(24.))
            .bg(surface())
            .font_family(crate::fonts::UI)
            .text_size(px(crate::theme::TITLE_TAB_TEXT_SIZE))
            .text_color(content(0.9))
            .child(
                div()
                    .w(px(280.))
                    .h(px(20.))
                    .child(crate::particle_text::particle_text(self.0)),
            )
    }
}

/// A title changing to a new one, `elapsed` into the reveal. The clock steps at 60 fps from the
/// change, so particles are born frame by frame as they are on screen.
fn reveal(cx: &mut HeadlessAppContext, elapsed: Duration) -> AnyWindowHandle {
    cx.update(|cx| cx.set_reduce_motion(false));
    let start = Instant::now();
    crate::particle_text::hold_clock(Some(start));
    let window = open(cx, (340., 56.), |_, cx| cx.new(|_| Title("New session")));
    window
        .update(cx, |title, _, cx| {
            title.0 = ACTIVE_TITLE;
            cx.notify();
        })
        .expect("window is open");
    cx.run_until_parked();
    let frames = (elapsed.as_secs_f32() * 60.).round() as u32;
    for frame in 1..=frames {
        crate::particle_text::hold_clock(Some(start + elapsed * frame / frames));
        cx.update_window(window.into(), |_, window, _| window.refresh())
            .expect("window is open");
        cx.run_until_parked();
    }
    window.into()
}

/// The title reveal's soft edge and particles, 300 ms and 700 ms into its 1.1 s sweep.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn particle_reveal() {
    for millis in [300, 700] {
        run(&format!("particle-{millis}ms"), |cx, _| {
            reveal(cx, Duration::from_millis(millis))
        });
    }
    crate::particle_text::hold_clock(None);
}

struct Labels;

impl Render for Labels {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let weights = [
            (FontWeight::NORMAL, "Regular"),
            (FontWeight::MEDIUM, "Medium"),
            (FontWeight::SEMIBOLD, "Semibold"),
        ];
        div()
            .size_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .bg(surface())
            .font_family(crate::fonts::UI)
            .text_color(content(0.9))
            .children([11., 12., 13., 15.].map(|text_size| {
                div()
                    .flex()
                    .gap(px(16.))
                    .text_size(px(text_size))
                    .children(weights.map(|(weight, label)| {
                        div()
                            .font_weight(weight)
                            .child(format!("{label} {text_size} Inbox Notes"))
                    }))
            }))
    }
}

/// SF labels at the app's sizes in regular, medium and semibold.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn labels() {
    run("labels", |cx, _| {
        open(cx, (720., 170.), |_, cx| cx.new(|_| Labels)).into()
    });
}

struct Backdrop(Entity<ProjectBackground>);

impl Render for Backdrop {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .bg(surface())
            .child(self.0.clone())
            .child(
                div()
                    .absolute()
                    .left(px(24.))
                    .bottom(px(24.))
                    .font_family(crate::fonts::UI)
                    .text_size(px(13.))
                    .text_color(content(0.9))
                    .child("Ask anything"),
            )
    }
}

/// The project background's mask, over an empty chat or behind a filled one, plain or with Haze.
fn backdrop(cx: &mut HeadlessAppContext, empty: bool, effect: Effect) -> AnyWindowHandle {
    let settings = Background {
        path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets/monocode.png").into(),
        effect,
        ..Default::default()
    };
    open(cx, (480., 300.), move |_, cx| {
        let background = cx.new(|cx| {
            let mut background = ProjectBackground::default();
            background.configure(Some(settings), empty, None, cx);
            background
        });
        cx.new(|_| Backdrop(background))
    })
    .into()
}

#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn project_background() {
    for (name, empty, effect) in [
        ("background-empty", true, Effect::None),
        ("background-filled", false, Effect::None),
        ("background-empty-haze", true, Effect::GradientBlur),
        ("background-filled-haze", false, Effect::GradientBlur),
    ] {
        run(name, |cx, _| backdrop(cx, empty, effect));
    }
}

/// Settings on `section` in place of the workspace, after clicking the control named `label`.
fn settings_page(
    cx: &mut HeadlessAppContext,
    look: Look,
    section: crate::settings::Section,
    label: Option<&str>,
) -> AnyWindowHandle {
    let fixture = Fixture::new(cx, look, SHELL_TALL, |_| {}, None);
    fixture.with_shell(cx, |shell, window, cx| {
        shell.open_settings(section, window, cx);
    });
    settle(cx, fixture.handle());
    if let Some(label) = label {
        press(cx, fixture.handle(), label);
    }
    fixture.handle()
}

/// Settings' controls in their rail and pages: the rail's search field and sections, page and
/// group headers, segmented controls, sliders (light dims the dark-only ones), accent swatches,
/// select triggers and toggles. `settings-appearance` opens the workspace theme select: its
/// listbox popover with the filter field, swatch icons and the check on the chosen row.
/// `settings-accent` opens the custom accent popover, its field's text selected.
/// `settings-chat` shows toggles on and off beside segmented rows.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn settings() {
    use crate::settings::Section;
    for (name, section, label) in [
        (
            "settings-appearance",
            Section::Appearance,
            Some("Workspace theme: Default"),
        ),
        ("settings-accent", Section::Appearance, Some("Custom color")),
        ("settings-chat", Section::Chat, None),
    ] {
        run(name, |cx, look| settings_page(cx, look, section, label));
    }
}

/// Quick Open as the command palette over the chat: its scrim and modal surface, the query field
/// holding `>`, and every command row with its icon and shortcut, the first one highlighted.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn quick_open() {
    run("quick-open", |cx, look| {
        let fixture = Fixture::new(cx, look, SHELL, |_| {}, None);
        fixture.with_shell(cx, |shell, window, cx| {
            shell.open_quick_open(true, window, cx);
        });
        fixture.handle()
    });
}

/// The session menu driven from the keyboard: the highlight stepped down to "Copy session ID"
/// and its submenu open beside it, a disabled row first, over the menu's separators, shortcut
/// hint, submenu chevrons and danger row.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn menu_submenu() {
    run("menu-submenu", |cx, look| {
        let fixture = Fixture::new(cx, look, SHELL, |_| {}, None);
        let active = fixture.active;
        fixture.with_shell(cx, |shell, window, cx| {
            let anchor = Anchor::Point(point(px(420.), px(150.)));
            shell.open_menu(MenuTarget::Session(active), anchor, window, cx);
        });
        settle(cx, fixture.handle());
        let keys: [Box<dyn gpui::Action>; 3] = [
            Box::new(crate::popover::SelectNext),
            Box::new(crate::popover::SelectNext),
            Box::new(crate::popover::OpenSubmenu),
        ];
        for action in keys {
            cx.update_window(fixture.handle(), |_, window, cx| {
                window.dispatch_action(action, cx);
            })
            .expect("window is open");
            settle(cx, fixture.handle());
        }
        fixture.handle()
    });
}

/// A project folder the test owns in the temp dir, holding the editor's file. The folder's own
/// name and the file's place in it are fixed, so only `src` shows in the breadcrumbs. Removed when
/// dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("onicode-golden-{}", std::process::id()));
        let src = dir.join("onicode/src");
        std::fs::create_dir_all(&src).expect("a scratch project");
        std::fs::write(src.join("edge_fade.rs"), sources::EDGE_FADE).expect("the scratch file");
        Self(dir)
    }

    fn project(&self) -> PathBuf {
        self.0.join("onicode")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("{}: {error}", self.0.display());
        }
    }
}

/// The file editor on a Rust file: breadcrumbs with the file's icon, line numbers, syntax colours
/// and a soft-wrapped doc comment; the find bar with the query Find took from the selected word,
/// its toggles and count, every match marked in the text and on the scrollbar rail beside its
/// thumb; and a selection across a whole function with the "Add to chat" popover above it.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn editor() {
    let scratch = Scratch::new();
    run("editor", |cx, look| {
        cx.update(|cx| cx.set_global(look.preferences()));
        let project = scratch.project();
        let path = project
            .join("src/edge_fade.rs")
            .to_string_lossy()
            .into_owned();
        let project = project.to_string_lossy().into_owned();
        let window = open(cx, CODE, move |_, cx| {
            let git = cx.new(|cx| crate::git_status::ProjectGit::new(project, cx));
            let editor = cx.new(|cx| crate::file_editor::FileEditor::new(path, git, cx));
            cx.new(|_| Framed(editor))
        });
        let editor = window
            .update(cx, |framed, _, _| framed.0.clone())
            .expect("window is open");
        let handle: AnyWindowHandle = window.into();
        let word = sources::EDGE_FADE.find("fade").expect("the file says fade");
        cx.update_window(handle, |_, window, cx| {
            editor.update(cx, |editor, cx| {
                editor.set_visible(true, cx);
                let input = editor.input.clone();
                window.focus(&input.focus_handle(cx), cx);
                input.update(cx, |input, cx| input.select_range(word..word + 4, cx));
            });
        })
        .expect("window is open");
        settle(cx, handle);
        cx.update_window(handle, |_, window, cx| {
            window.dispatch_action(Box::new(crate::file_editor::Find), cx);
        })
        .expect("window is open");
        settle(cx, handle);
        let function = sources::lines(sources::EDGE_FADE, 18, 25);
        cx.update_window(handle, |_, _, cx| {
            let input = editor.read(cx).input.clone();
            input.update(cx, |input, cx| input.select_range(function, cx));
        })
        .expect("window is open");
        settle(cx, handle);
        // The popover anchors on the selection as the frame after it laid out placed it.
        next_frame(cx, handle);
        handle
    });
}

fn diff_file(path: &str, before: &str, after: &str) -> crate::diff_view::DiffFile {
    let unified = onicode_protocol::diff::build_unified_file(before, after, 3);
    crate::diff_view::DiffFile {
        id: path.to_owned().into(),
        path: format!("{PROJECT}/{path}"),
        label: path.into(),
        binary: false,
        too_large: false,
        empty_message: None,
        additions: unified.additions,
        deletions: unified.deletions,
        blocks: Arc::new(unified.blocks),
        can_stage: true,
        can_discard: true,
        can_stage_hunk: true,
    }
}

/// The unified diff of two files, both open: the toolbar's file count, line counts and hunk
/// stepper; file headers with their icons, counts and actions; hunk headers; the gutter's line
/// numbers; added, deleted and context lines over syntax colours; the overview rail's thumb; and
/// the sideways scrollbar under a line too long for the lane.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn diff() {
    run("diff", |cx, look| {
        cx.update(|cx| cx.set_global(look.preferences()));
        let before = sources::edge_fade_before();
        let (doc_before, doc_after) = sources::GOLDEN_DOC;
        let files = vec![
            diff_file("src/edge_fade.rs", &before, sources::EDGE_FADE),
            diff_file("docs/onicode/golden.md", doc_before, doc_after),
        ];
        open(cx, CODE, move |_, cx| {
            let view = cx.new(crate::diff_view::DiffView::new);
            view.update(cx, |view, cx| view.set_files(files, cx));
            cx.new(|_| Framed(view))
        })
        .into()
    });
}

/// The terminal's grid fed a test run's output, with no shell behind it: bold, dim, italic,
/// underlined, struck and inverse text, the 16 colours as ink and fill, a 256-colour grey ramp,
/// truecolor ink, and the focused block cursor at the prompt. The terminal font is pinned to
/// Menlo, whatever Nerd Font the machine has installed.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn terminal() {
    crate::fonts::pin_terminal_font();
    run("terminal", |cx, _| {
        let window = open(cx, CODE, |_, cx| {
            let terminal = cx.new(|cx| Terminal::new(PROJECT.into(), cx));
            cx.new(|_| Framed(terminal))
        });
        let terminal = window
            .update(cx, |framed, _, _| framed.0.clone())
            .expect("window is open");
        let output = sources::terminal_output();
        cx.update_window(window.into(), |_, window, cx| {
            window.focus(&terminal.focus_handle(cx), cx);
            terminal.update(cx, |terminal, cx| terminal.feed(output.as_bytes(), cx));
        })
        .expect("window is open");
        window.into()
    });
}

/// Every kind of spinner, each in its own row.
struct Spinners {
    marks: Vec<Entity<SessionMark>>,
}

impl Render for Spinners {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let row = || div().flex().items_center().gap(px(16.));
        div()
            .size_full()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .bg(surface())
            .text_color(content(0.9))
            .child(
                row()
                    .child(crate::session_mark::spinner(
                        "golden-wave",
                        crate::pulse::Rate::Smooth,
                    ))
                    .children([0., 0.25, 0.5, 0.75].map(crate::session_mark::spinner_at)),
            )
            .child(
                row()
                    .child(crate::right_panel::spinner("golden-loader", "loader", 16.))
                    .child(crate::right_panel::spinner(
                        "golden-loader-small",
                        "loader",
                        12.,
                    ))
                    .child(crate::right_panel::spinner(
                        "golden-sync",
                        "refresh-cw",
                        14.,
                    )),
            )
            .child(row().children(self.marks.iter().cloned()))
    }
}

/// The spinners with motion on, `SPIN` of virtual time after they first drew, so each loop on the
/// shared clock stands at that clock's last tick: the session wave live and at four fixed
/// fractions of its period, the rotating loaders, and a session mark idle, working, done and
/// waiting on an approval.
#[test]
#[ignore = "golden images: scripts/dev/golden.sh"]
fn spinners() {
    use crate::session_mark::State;
    run("spinners", |cx, _| {
        cx.update(|cx| cx.set_reduce_motion(false));
        let window = open(cx, (360., 150.), |_, cx| {
            let marks = [State::Idle, State::Working, State::Done, State::Attention].map(|state| {
                cx.new(|cx| {
                    let mut mark = SessionMark::new("claude".into());
                    mark.set("claude", state, cx);
                    mark
                })
            });
            cx.new(|_| Spinners {
                marks: Vec::from(marks),
            })
        });
        cx.advance_clock(SPIN);
        window.into()
    });
}

#[test]
fn compare_allows_one_level_only_on_antialiased_edges() {
    let mut expected = RgbaImage::from_pixel(4, 3, Rgba([20, 20, 20, 255]));
    // A hard edge between columns 1 and 2.
    for y in 0..3 {
        expected.put_pixel(2, y, Rgba([200, 200, 200, 255]));
        expected.put_pixel(3, y, Rgba([200, 200, 200, 255]));
    }
    assert!(on_edge(&expected, 1, 1));
    assert!(!on_edge(&expected, 0, 1));
    let mut actual = expected.clone();
    actual.put_pixel(1, 1, Rgba([21, 20, 20, 255]));
    let (stats, _) = compare(&expected, &actual, 0);
    assert_eq!((stats.max_delta, stats.differing, stats.failing), (1, 1, 0));
    actual.put_pixel(0, 1, Rgba([21, 20, 20, 255]));
    let (stats, _) = compare(&expected, &actual, 0);
    assert_eq!((stats.differing, stats.over_one, stats.failing), (2, 0, 1));
    actual.put_pixel(2, 0, Rgba([203, 200, 200, 255]));
    let (stats, _) = compare(&expected, &actual, 2);
    assert_eq!((stats.max_delta, stats.over_one, stats.failing), (3, 1, 1));
}
