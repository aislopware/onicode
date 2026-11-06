#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

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
#[cfg(debug_assertions)]
mod dev_reload;
mod diff_view;
mod editor_analysis;
mod editor_scrollbar;
mod editor_syntax;
mod explorer;
mod file_editor;
mod file_icons;
mod fixtures;
mod fonts;
mod frame_log;
mod fs_watch;
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
#[cfg(target_os = "macos")]
mod memory_log;
#[cfg(all(test, target_os = "macos"))]
mod metal_tests;
mod mobile;
mod modal;
mod notes;
mod notifications;
mod pane_layout;
mod pane_motion;
mod particle_text;
mod phase_tile;
mod pickers;
mod pixel_mascot;
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
mod runtime;
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
mod usage_footer;
mod visuals;
#[cfg(test)]
mod wheel_glide_tests;
mod window_state;
mod workspace;
mod worktree_switcher;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui::prelude::*;
use gpui::{App, KeyBinding, TitlebarOptions, WindowOptions, actions, point, px, size};
use onicode_core::store::Store;
use onicode_core::transcript::Transcript;
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
    if let Some(code) = onicode_core::remote::ssh_askpass::maybe_run() {
        std::process::exit(code);
    }
    let cli_args: Vec<_> = std::env::args().skip(1).collect();
    if cli_args
        .first()
        .is_some_and(|arg| arg == "--version" || arg == "-V")
    {
        println!("Onicode {}", notifications::updater::version());
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
    let database = data_dir().join("onicode.db");
    let mut db = if check_data {
        Store::read_monocode(&database)
    } else {
        Store::open_monocode(&database)
    }
    .unwrap_or_else(|error| {
        eprintln!("Cannot open {}: {error}", database.display());
        std::process::exit(1);
    });
    if check_data {
        match db.sessions() {
            Ok(sessions) => eprintln!(
                "Database: {}\n{} saved sessions; no import or migration.",
                database.display(),
                sessions.len()
            ),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(1);
            }
        }
        return;
    }
    onicode_core::checkpoint::init(
        database
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("checkpoints"),
    );
    let saved = db.ui_state().unwrap_or_else(|e| {
        eprintln!("Cannot read workspace settings: {e}");
        None
    });
    let existing_project = saved
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|state| {
            state["project"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            db.sessions()
                .ok()
                .and_then(|sessions| sessions.into_iter().find(|s| !s.archived).map(|s| s.cwd))
        });
    // Opened from the Dock or Finder, macOS starts the app in `/`. Resume the
    // saved project instead of deriving a project from an internal chat path.
    let cwd = if let Some(project) = existing_project.filter(|_| !explicit_cwd) {
        project
    } else if explicit_cwd || std::path::Path::new(&cwd).parent().is_some() {
        cwd
    } else {
        db.sessions()
            .ok()
            .and_then(|sessions| sessions.into_iter().map(|s| s.cwd).find(|c| c != &cwd))
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or(cwd))
    };
    if fixture {
        let s = db.create("codex", &cwd).expect("create fixture");
        db.title(s.id, "Scroll lab · 10,000 messages").unwrap();
        let mut t = Transcript::default();
        let started = Instant::now();
        for i in 0..10_000 {
            t.begin(if i % 2 == 0 { "user" } else { "assistant" });
            t.append(&format!("Message {i:05} · Native transcript stress fixture.\nUnicode stays intact: 🙂 漢字 café. Scroll up while another session streams.\nThis message is local fixture data; sending resumes a new real provider conversation."));
        }
        db.write_rows(s.id, 0, &t.rows).unwrap();
        eprintln!("Seeded {} rows in {:?}", t.rows.len(), started.elapsed());
    }
    if chat_fixture {
        let s = db.create("codex", &cwd).expect("create chat fixture");
        db.title(s.id, "Native chat · Markdown and tools").unwrap();
        db.write_rows(s.id, 0, &fixtures::chat().rows).unwrap();
    }
    if visual_fixture {
        let s = db.create("codex", &cwd).expect("create visual fixture");
        db.title(s.id, "Native visuals · Controls, charts and 3D")
            .unwrap();
        db.write_rows(s.id, 0, &fixtures::visuals().rows).unwrap();
        let s = db
            .create("claude", &cwd)
            .expect("create visual story fixture");
        db.title(s.id, "Native visuals · Charts inside an answer")
            .unwrap();
        db.write_rows(s.id, 0, &fixtures::visual_story().rows)
            .unwrap();
    }
    let mut sessions = db.sessions().expect("read native sessions");
    if sessions.is_empty() {
        sessions.push(db.create("codex", &cwd).expect("create initial session"));
    }
    // Gather compact sidebar state before entering the platform event loop.
    // The shell does not query thousands of saved agents on the UI thread.
    let metadata = sessions
        .iter()
        .map(|s| (s.id, s.cwd.clone()))
        .collect::<Vec<_>>();
    let workspace = workspace::Workspace::restore(saved.as_deref(), &metadata, &cwd);
    let mut projects = workspace.projects;
    if !projects.contains(&workspace.project) {
        projects.push(workspace.project);
    }
    let sidebar_metadata = db.session_sidebar_metadata(&projects);
    let db = Arc::new(Mutex::new(db));
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
            hang_watchdog::start(database.with_file_name("gpui-hangs"), cx);
            input::bind(cx);
            image_lightbox::bind(cx);
            composer_completion::bind(cx);
            file_editor::bind(cx);
            popover::bind(cx);
            cx.set_prompt_builder(modal::prompt_builder);
            explorer::bind(cx);
            history::bind(cx);
            diff_view::bind(cx);
            fonts::init(cx);
            terminal::bind(cx);
            let modifier = if cfg!(target_os = "macos") {
                "cmd"
            } else {
                "ctrl"
            };
            cx.bind_keys([KeyBinding::new(&format!("{modifier}-q"), Quit, None)]);
            shell::bind_keys(cx);
            app_menu::bind(cx);
            usage_footer::bind(cx);
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    quit(cx)
                }
            })
            .detach();
            let bounds = workspace.window.bounds(cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(bounds),
                    window_min_size: Some(size(px(820.), px(520.))),
                    window_decorations: workspace
                        .preferences
                        .wayland_decorations(cx.compositor_name()),
                    // Linux desktops match this to the .desktop file's
                    // StartupWMClass to show the app icon.
                    app_id: Some("dev.usemono.gpui".into()),
                    window_background: theme::window_backdrop(true),
                    // GPUI owns the custom chrome. AppKit must not reinterpret
                    // its controls as native titlebar double-click/drag gestures.
                    app_owns_titlebar_drag: cfg!(target_os = "macos"),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Monocode Native".into()),
                        // Windows also drops its caption: the app's drag areas
                        // hit-test as the caption and the frame still resizes.
                        appears_transparent: cfg!(any(target_os = "macos", target_os = "windows")),
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
                    // Off the launch path: it lists every conversation folder.
                    let cleanup = db.clone();
                    window.on_next_frame(move |_, cx| {
                        cx.background_executor()
                            .spawn(async move {
                                let result = cleanup
                                    .lock()
                                    .map_err(|error| error.to_string())
                                    .and_then(|store| store.sweep_thread_dirs());
                                if let Err(error) = result {
                                    eprintln!(
                                        "Cannot clean up deleted conversations' files: {error}"
                                    );
                                }
                            })
                            .detach();
                    });
                    let shell = cx.new(|cx| {
                        shell::Shell::new_with_metadata(
                            db,
                            sessions,
                            cwd,
                            saved,
                            sidebar_metadata,
                            cx,
                        )
                    });
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

#[cfg(test)]
mod native_tests {
    use gpui::{
        Bounds, Context, Entity, EntityInputHandler, ListOffset, Render, StyleRefinement,
        TestAppContext, Window, WindowBounds, div, point,
    };
    use onicode_core::transcript::Row;

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
                |_, cx| {
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
