/// A keystroke as macOS spells it or as the other desktops do, as the `&'static str`
/// `KeyBinding::new` takes: `os_keys!("cmd-n", "ctrl-n")`.
macro_rules! os_keys {
    ($mac:literal, $other:literal) => {
        if cfg!(target_os = "macos") {
            $mac
        } else {
            $other
        }
    };
}

mod accounts;
mod activity;
mod app_log;
mod app_menu;
mod arcade;
mod assets;
mod automations;
mod changes;
mod composer_completion;
mod composer_motion;
mod composer_runner;
mod context_meter;
mod conversation;
// A Mac's whole display in a window of its own (remote-desktop.md): macOS only.
#[cfg(target_os = "macos")]
mod desktop;
#[cfg(debug_assertions)]
mod dev_reload;
mod diff_view;
mod editor_analysis;
mod editor_scrollbar;
mod editor_syntax;
mod engine_client;
mod explorer;
mod file_editor;
mod file_icons;
mod fixtures;
mod fonts;
mod frame_log;
mod git_cache;
mod git_graph;
mod git_picker;
mod git_status;
mod gpu_trim;
#[cfg(target_os = "macos")]
mod hang_watchdog;
mod history;
mod image_lightbox;
mod image_memory;
mod inbox;
mod ink_field;
mod input;
mod markdown;
mod markdown_model;
mod mask;
#[cfg(target_os = "macos")]
mod memory_log;
#[cfg(all(test, target_os = "macos"))]
mod metal_tests;
mod modal;
mod notes;
mod notifications;
mod pane_layout;
mod pane_motion;
mod particle_text;
mod phase_tile;
mod pickers;
mod popover;
mod project_background;
mod project_search;
mod project_settings;
mod project_tabs;
mod project_theme;
mod pulse;
mod questions;
mod quick_composer;
mod quick_open;
mod reflow_motion;
mod resource_pressure;
mod response_actions;
mod reveal;
mod review;
mod right_panel;
mod search;
mod send_morph;
mod session_mark;
mod session_review;
mod settings;
mod shell;
mod sound;
#[cfg(all(target_os = "macos", not(test)))]
mod status_item;
mod syntax;
mod tab_label;
mod tab_motion;
mod tab_tile;
mod terminal;
mod theme;
#[cfg(target_os = "macos")]
mod traffic_lights;
mod transcript;
mod ui_state;
mod usage_footer;
mod visuals;
#[cfg(test)]
mod wheel_glide_tests;
mod window_state;
mod workspace;
mod worktree_switcher;

use std::path::PathBuf;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{App, KeyBinding, TitlebarOptions, WindowOptions, actions, point, px, size};
use onicode_protocol::sessions::Session;
use onicode_protocol::transcript::Transcript;
use onicode_protocol::workspace::{CreateSession, NewSession};
use onicode_protocol::{OpId, SessionId};

use crate::engine_client::Engine;
/// libmalloc keeps freed large spans dirty, so startup decodes and image churn
/// stayed in the footprint. mimalloc 2 returns them; 3 retains churn too.
#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

actions!(application, [Quit]);
fn quit(cx: &mut App) {
    if notifications::updater::defer_quit(cx) {
        return;
    }
    let shutdown = cx
        .background_executor()
        .spawn(async { onicode_core::process::shutdown_all() });
    cx.spawn(async move |cx| {
        shutdown.await;
        cx.update(|cx| cx.quit());
    })
    .detach();
}

/// Quit, then relaunch the same app once this process has exited.
fn restart(cx: &mut App) {
    let shutdown = cx
        .background_executor()
        .spawn(async { onicode_core::process::shutdown_all() });
    cx.spawn(async move |cx| {
        shutdown.await;
        cx.update(|cx| cx.restart());
    })
    .detach();
}

/// Onicode's own data: `Onicode` for release builds, `Onicode Development` for debug ones, or
/// `ONICODE_DATA_DIR`.
fn data_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("ONICODE_DATA_DIR") {
        return path.into();
    }
    let base = PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
        .join("Library/Application Support");
    base.join(if cfg!(debug_assertions) {
        "Onicode Development"
    } else {
        "Onicode"
    })
}
fn main() {
    let cli_args: Vec<_> = std::env::args().skip(1).collect();
    if cli_args
        .first()
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        // `onicode-update` runs a staged release's `--version` and expects exactly this.
        println!("onicode {}", onicode_update::app_version());
        return;
    }
    if cli_args.first().is_some_and(|a| a == "visual-mcp") {
        if let Err(error) = onicode_core::visual_bridge::mcp_stdio() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    if cli_args.first().is_some_and(|a| a == "visual") {
        match onicode_core::visual_bridge::client(&cli_args[1..]) {
            Ok(value) => {
                println!("{value}");
                if value.get("ok") == Some(&serde_json::Value::Bool(false)) {
                    std::process::exit(1);
                }
            }
            Err(error) => {
                println!("{}", serde_json::json!({"ok":false,"error":error}));
                std::process::exit(1);
            }
        }
        return;
    }
    if cli_args.first().is_some_and(|a| a == "app") {
        match onicode_core::app_access::bridge::client(&cli_args[1..]) {
            Ok(value) => {
                println!("{value}");
                if value.get("ok") == Some(&serde_json::Value::Bool(false)) {
                    std::process::exit(1);
                }
            }
            Err(error) => {
                println!("{}", serde_json::json!({"ok":false,"error":error}));
                std::process::exit(1);
            }
        }
        return;
    }
    let mut args = std::env::args().skip(1);
    let mut fixture = false;
    let mut chat_fixture = false;
    let mut visual_fixture = false;
    let mut check_data = false;
    let mut explicit_cwd = false;
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fixture" => fixture = true,
            "--chat-fixture" => chat_fixture = true,
            "--visual-fixture" => visual_fixture = true,
            "--check-data" => check_data = true,
            "--cwd" => {
                explicit_cwd = true;
                cwd = PathBuf::from(args.next().expect("--cwd needs a path"));
            }
            "--help" | "-h" => {
                println!(
                    "onicode [--cwd PATH] [--check-data]\nData lives in ~/Library/Application Support/Onicode (Onicode Development for debug builds, or ONICODE_DATA_DIR).\n--fixture, --chat-fixture and --visual-fixture add local samples to debug builds' data only.\nvisual docs prints the native visual SDK reference.\nvisual check/show --input FILE validates or publishes native session visuals.\nvisual-mcp runs the session visual tool server over standard input/output.\n--check-data validates and lists the store read-only, then exits."
                );
                return;
            }
            _ => {
                eprintln!("Unknown option: {arg}");
                std::process::exit(2);
            }
        }
    }
    if !cfg!(debug_assertions) && (fixture || chat_fixture || visual_fixture) {
        eprintln!("Fixtures are for debug builds; they never go into your Onicode data.");
        std::process::exit(2);
    }
    let cwd = cwd
        .canonicalize()
        .unwrap_or(cwd)
        .to_string_lossy()
        .to_string();
    if check_data {
        match onicode_engine::Local::check_data(&data_dir()) {
            Ok((database, sessions)) => eprintln!(
                "Database: {}\n{sessions} saved sessions; no import or migration.",
                database.display(),
            ),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    // Where the update channel is kept, and the update record this app reads.
    let data = data_dir();
    onicode_update::channel::set_data_dir(&data);
    let version = onicode_update::app_version();
    if onicode_update::Channel::of_version(version) == onicode_update::Channel::Beta
        && let Err(error) = onicode_update::channel::claim_for_beta_build(&data, version)
    {
        eprintln!("Cannot record the beta update channel: {error}");
    }
    let samples = Fixtures {
        scroll: fixture,
        chat: chat_fixture,
        visual: visual_fixture,
    };
    app_log::init();
    let platform = gpui_platform::current_platform(false);
    let title_raster = particle_text::GlyphRaster(platform.text_system());
    gpui::Application::with_platform(platform)
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            #[cfg(debug_assertions)]
            dev_reload::install(cx);
            cx.set_global(title_raster);
            image_memory::init(cx);
            #[cfg(target_os = "macos")]
            memory_log::start(cx);
            frame_log::start(cx);
            gpu_trim::start(cx);
            #[cfg(target_os = "macos")]
            hang_watchdog::start(data.join("gpui-hangs"), cx);
            input::bind(cx);
            image_lightbox::bind(cx);
            composer_completion::bind(cx);
            file_editor::bind(cx);
            popover::bind(cx);
            cx.set_prompt_builder(modal::prompt_builder);
            explorer::bind(cx);
            history::bind(cx);
            diff_view::bind(cx);
            terminal::bind(cx);
            cx.bind_keys([KeyBinding::new(os_keys!("cmd-q", "ctrl-q"), Quit, None)]);
            shell::bind_keys(cx);
            app_menu::bind(cx);
            usage_footer::bind(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    quit(cx)
                }
            })
            .detach();
            let Launch {
                engine,
                sessions,
                cwd,
                saved,
                workspace,
            } = launch(&data, cwd, explicit_cwd, samples, cx);
            let bounds = workspace.window.bounds(cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(bounds),
                    window_min_size: Some(size(px(820.), px(520.))),
                    window_background: theme::window_backdrop(true),
                    // GPUI owns the custom chrome. AppKit must not reinterpret
                    // its controls as native titlebar double-click/drag gestures.
                    app_owns_titlebar_drag: cfg!(target_os = "macos"),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Onicode".into()),
                        appears_transparent: cfg!(target_os = "macos"),
                        traffic_light_position: if cfg!(target_os = "macos") {
                            // Original macOS chrome: 12 pt left inset, with
                            // 14 pt native buttons centered in the 40 pt bar.
                            Some(point(px(12.), px((theme::TITLE_BAR_HEIGHT - 14.) / 2.)))
                        } else {
                            None
                        },
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    // Install the menu one frame later, after the first frame presents.
                    window.on_next_frame(|window, _| {
                        window.on_next_frame(|_, cx| app_menu::ready(cx));
                    });
                    let shell = cx.new(|cx| shell::Shell::new(engine, sessions, cwd, saved, cx));
                    shell.update(cx, |shell, cx| shell.focus_active(window, cx));
                    #[cfg(target_os = "macos")]
                    {
                        traffic_lights::pin(window, theme::TITLE_BAR_HEIGHT);
                        // AppKit rebuilds the buttons on some titlebar changes.
                        shell.update(cx, |_, cx| {
                            cx.observe_window_activation(window, |_, window, _| {
                                traffic_lights::pin(window, theme::TITLE_BAR_HEIGHT)
                            })
                            .detach();
                        });
                    }
                    window.activate_window();
                    shell
                },
            )
            .expect("open GPUI window");
            cx.activate(true);
        });
}

/// What the first window opens on.
struct Launch {
    engine: Engine,
    sessions: Vec<Session>,
    cwd: String,
    saved: Option<String>,
    workspace: workspace::Workspace,
}

/// The local samples debug builds can add to their data.
#[derive(Clone, Copy)]
struct Fixtures {
    scroll: bool,
    chat: bool,
    visual: bool,
}

impl Fixtures {
    fn any(self) -> bool {
        self.scroll || self.chat || self.visual
    }
}

/// The project a saved workspace names.
fn saved_project(saved: Option<&str>) -> Option<String> {
    saved
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|state| {
            state["project"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
}

/// Reaches the engine (launching it in the background when none runs) and opens the app's own
/// state, then finds the project and sessions the window opens on: from what the last launch
/// kept, so the first frame never waits on the engine.
fn launch(
    data: &std::path::Path,
    cwd: String,
    explicit_cwd: bool,
    samples: Fixtures,
    cx: &mut App,
) -> Launch {
    let ui_state = ui_state::UiState::open(data).unwrap_or_else(|error| {
        eprintln!("{error}; the window's layout will not be kept.");
        ui_state::UiState::memory()
    });
    if samples.any() {
        let project = saved_project(ui_state.get(ui_state::WORKSPACE).as_deref())
            .filter(|_| !explicit_cwd)
            .unwrap_or_else(|| cwd.clone());
        if let Err(error) = seed_fixtures(data, &project, samples) {
            eprintln!("Cannot add the fixtures: {error}");
        }
    }
    let engine = Engine::connect(data, cx).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });
    if let Err(error) = ui_state.adopt(&engine) {
        eprintln!("Cannot take the saved workspace from the engine: {error}");
    }
    cx.set_global(ui_state::Shared(ui_state.clone()));
    let saved = ui_state.get(ui_state::WORKSPACE);
    let stored = engine
        .workspace_now()
        .map(|state| {
            state
                .sessions
                .into_iter()
                .map(|summary| summary.session)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|error| {
            eprintln!("Cannot read the engine's sessions: {error}");
            Vec::new()
        });
    let existing_project = saved_project(saved.as_deref())
        .or_else(|| stored.iter().find(|s| !s.archived).map(|s| s.cwd.clone()));
    // Opened from the Dock or Finder, macOS starts the app in `/`. Resume the
    // saved project instead of deriving a project from an internal chat path.
    let cwd = if let Some(project) = existing_project.filter(|_| !explicit_cwd) {
        project
    } else if explicit_cwd || std::path::Path::new(&cwd).parent().is_some() {
        cwd
    } else {
        stored
            .iter()
            .map(|s| s.cwd.clone())
            .find(|c| c != &cwd)
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or(cwd))
    };
    let mut sessions = stored;
    if sessions.is_empty() {
        let created = futures::executor::block_on(engine.call::<CreateSession>(
            Some(OpId::new()),
            CreateSession {
                session: SessionId::new(),
                provider: "codex".into(),
                cwd: cwd.clone(),
                title: None,
                start: NewSession::Bare,
                started_by: None,
                scope: Vec::new(),
            },
        ));
        match created.map(|created| created.session) {
            Ok(Some(summary)) => sessions.push(summary.session),
            Ok(None) => eprintln!("The engine created no first session"),
            Err(error) => eprintln!("Cannot create the first session: {error}"),
        }
    }
    let metadata = sessions
        .iter()
        .map(|s| (s.id, s.cwd.clone()))
        .collect::<Vec<_>>();
    let workspace = workspace::Workspace::restore(saved.as_deref(), &metadata, &cwd);
    Launch {
        engine,
        sessions,
        cwd,
        saved,
        workspace,
    }
}

/// Writes the debug builds' local samples into the store in `data`, before the engine is
/// reached: a running engine is stopped first, and the one launched next reads them.
fn seed_fixtures(data: &std::path::Path, cwd: &str, samples: Fixtures) -> Result<(), String> {
    onicode_engine::testing::Seed::offline(data, |seed| -> Result<(), String> {
        if samples.scroll {
            let s = seed.create("codex", cwd)?;
            seed.title(s.id, "Scroll lab · 10,000 messages")?;
            let mut t = Transcript::default();
            let started = Instant::now();
            for i in 0..10_000 {
                t.begin(if i % 2 == 0 { "user" } else { "assistant" });
                t.append(&format!("Message {i:05} · Native transcript stress fixture.\nUnicode stays intact: 🙂 漢字 café. Scroll up while another session streams.\nThis message is local fixture data; sending resumes a new real provider conversation."));
            }
            seed.write_rows(s.id, 0, &t.rows)?;
            eprintln!("Seeded {} rows in {:?}", t.rows.len(), started.elapsed());
        }
        if samples.chat {
            let s = seed.create("codex", cwd)?;
            seed.title(s.id, "Native chat · Markdown and tools")?;
            seed.write_rows(s.id, 0, &fixtures::chat().rows)?;
        }
        if samples.visual {
            let s = seed.create("codex", cwd)?;
            seed.title(s.id, "Native visuals · Controls, charts and 3D")?;
            seed.write_rows(s.id, 0, &fixtures::visuals().rows)?;
            let s = seed.create("claude", cwd)?;
            seed.title(s.id, "Native visuals · Charts inside an answer")?;
            seed.write_rows(s.id, 0, &fixtures::visual_story().rows)?;
        }
        Ok(())
    })?
}

#[cfg(test)]
mod native_tests {
    use gpui::{
        Bounds, Context, Entity, EntityInputHandler, ListOffset, Render, StyleRefinement,
        TestAppContext, Window, WindowBounds, div, point,
    };
    use onicode_protocol::transcript::Row;

    use super::*;
    use crate::input;
    use crate::transcript::TranscriptView;
    struct Surface {
        transcript: Entity<TranscriptView>,
        input: Entity<input::Input>,
    }
    impl Render for Surface {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(
                    div().flex_1().min_h_0().child(
                        self.transcript
                            .clone()
                            .cached(StyleRefinement::default().size_full()),
                    ),
                )
                .child(
                    self.input
                        .clone()
                        .cached(StyleRefinement::default().w_full().h(px(76.))),
                )
        }
    }
    #[gpui::test]
    fn long_history_virtualizes_and_typing_does_not_rebuild_it(cx: &mut TestAppContext) {
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.), px(0.)),
                        size(px(900.), px(700.)),
                    ))),
                    ..Default::default()
                },
                |window, cx| {
                    // It counts renders; the retained check's frame from scratch renders again.
                    window.set_check_retained(None);
                    cx.new(|cx| {
                        let transcript = cx.new(|_| TranscriptView::new());
                        let mut history = Transcript::default();
                        for _ in 0..10_000 {
                            history.begin("assistant");
                            history.append("A bounded block of text.\nA second line.");
                        }
                        transcript.update(cx, |v, cx| v.patch(0, history.rows, cx));
                        Surface {
                            transcript,
                            input: cx.new(input::Input::new),
                        }
                    })
                },
            )
            .unwrap()
        });
        cx.refresh().unwrap();
        cx.run_until_parked();
        let root = window.root(cx).unwrap();
        let transcript = root.read_with(cx, |v, _| v.transcript.clone());
        let (passes, rendered) = transcript.read_with(cx, |t, _| (t.render_passes, t.rendered));
        assert!(
            rendered > 0 && rendered < 100,
            "expected viewport-sized work, rendered {rendered} rows"
        );
        window
            .update(cx, |v, w, cx| {
                v.input.update(cx, |i, cx| {
                    i.replace_text_in_range(None, "Typing 🙂 stays local", w, cx)
                })
            })
            .unwrap();
        cx.run_until_parked();
        assert_eq!(
            transcript.read_with(cx, |t, _| t.render_passes),
            passes,
            "typing must not rerender the transcript entity"
        );
        window
            .update(cx, |v, _, cx| {
                v.transcript.update(cx, |t, cx| {
                    t.list.pause_following_tail();
                    t.list.scroll_to(ListOffset {
                        item_ix: 5000,
                        offset_in_item: px(7.),
                    });
                    let anchor = t.list.logical_scroll_top();
                    t.patch(
                        9999,
                        vec![Row {
                            role: "assistant".into(),
                            text: "Changed streaming tail".into(),
                            first: true,
                            sent_at: None,
                            id: None,
                        }],
                        cx,
                    );
                    assert_eq!(t.list.logical_scroll_top().item_ix, anchor.item_ix);
                    assert_eq!(
                        t.list.logical_scroll_top().offset_in_item,
                        anchor.offset_in_item
                    );
                });
            })
            .unwrap();
    }
}
