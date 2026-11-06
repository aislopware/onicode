//! Original-layout destinations cover the project sessions without changing
//! the remembered sidebar or destroying the conversation workspace.
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

use super::*;

#[derive(Clone, Copy, Debug)]
enum Surface {
    Inbox,
    Notes,
    Automations,
    Search,
}

impl Surface {
    fn open(self, shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
        match self {
            Self::Inbox => shell.open_inbox(window, cx),
            Self::Notes => shell.open_notes(window, cx),
            Self::Automations => {
                // An empty in-memory service keeps this navigation test
                // independent of asynchronous service initialization.
                shell.automation_state.service = Some(Arc::new(Mutex::new(
                    onicode_core::automations::Service::open(std::path::Path::new(":memory:"))
                        .unwrap(),
                )));
                shell.open_automations(window, cx);
            }
            Self::Search => shell.open_app_search(window, cx),
        }
    }

    fn close(self, shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
        match self {
            Self::Inbox | Self::Notes | Self::Automations => shell.close_app_view(window, cx),
            Self::Search => shell.close_app_search(window, cx),
        }
    }

    fn header(self) -> &'static str {
        match self {
            Self::Search => "app-search-header",
            _ => "classic-app-surface-header",
        }
    }

    fn icon_left(self, visual: &mut VisualTestContext) -> Pixels {
        let (label, offset) = match self {
            Self::Search => ("app-search-field", 22.),
            _ => ("classic-app-surface-title", 22.),
        };
        visual.debug_bounds(label).unwrap().left() - px(offset)
    }
}

fn setup(cx: &mut TestAppContext, classic: bool, boxy: bool) -> (WindowHandle<Shell>, i64) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let (window, _, session) = super::tests::window(cx);
    cx.simulate_window_resize(window.into(), gpui::size(px(1500.), px(900.)));
    window
        .update(cx, |shell, window, cx| {
            shell.workspace.classic_layout = classic;
            shell.workspace.new_layout_boxy = boxy;
            shell.workspace.sidebar_visible = true;
            shell.workspace.project_rail_open = true;
            shell.workspace.project_rail_width = 220.;
            shell.workspace.sidebar_width = 260.;
            shell.workspace.right_panel_open = false;
            shell.open(session, window, cx);
            shell.layout_changed(cx);
        })
        .unwrap();
    cx.run_until_parked();
    (window, session)
}

#[gpui::test]
fn original_project_plus_offers_local_and_remote_folders(cx: &mut TestAppContext) {
    let (window, _) = setup(cx, true, false);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    let click = |selector: &'static str, visual: &mut VisualTestContext| {
        visual.refresh().unwrap();
        let bounds = visual.debug_bounds(selector).unwrap();
        visual.simulate_click(bounds.center(), Default::default());
        visual.run_until_parked();
        visual.refresh().unwrap();
    };
    assert!(!visual.did_prompt_for_paths());
    click("rail-open-project", visual);
    assert!(
        !visual.did_prompt_for_paths(),
        "the plus opens a menu before choosing a folder source"
    );
    assert!(visual.debug_bounds("menu-action-open-project").is_some());
    assert!(
        visual
            .debug_bounds("menu-action-open-remote-project")
            .is_some()
    );
    click("menu-action-open-project", visual);
    assert!(visual.did_prompt_for_paths());
    visual.simulate_path_prompt_response(|options| {
        assert!(options.directories);
        assert!(!options.files && !options.multiple);
        None
    });
    visual.run_until_parked();
    click("rail-open-project", visual);
    click("menu-action-open-remote-project", visual);
    assert!(visual.debug_bounds("remote-project-picker").is_some());
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    window
        .read_with(visual, |shell, _| {
            assert!(shell.menu.is_none());
            assert!(shell.remote_project_picker.is_none());
            assert_eq!(shell.workspace.project, "/work/monocode");
        })
        .unwrap();
}

fn assert_original_surface(surface: Surface, cx: &mut TestAppContext) {
    let (window, session) = setup(cx, true, false);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.refresh().unwrap();
    assert!(visual.debug_bounds("sidebar-content").is_some());
    assert!(visual.debug_bounds("title-tab-strip").is_some());
    let (tabs, conversation) = window
        .read_with(visual, |shell, cx| {
            (
                shell.workspace.tabs.clone(),
                shell.sessions[&session].read(cx).input.entity_id(),
            )
        })
        .unwrap();

    window
        .update(visual, |shell, window, cx| surface.open(shell, window, cx))
        .unwrap();
    visual.run_until_parked();
    visual.refresh().unwrap();

    assert!(
        visual.debug_bounds("sidebar-content").is_none(),
        "{surface:?} covers the original sessions sidebar"
    );
    assert!(
        visual.debug_bounds("title-tab-strip").is_none(),
        "{surface:?} replaces conversation tabs with its own header"
    );
    let rail = visual.debug_bounds("project-rail-frame").unwrap();
    assert_eq!(rail.size.width, px(220.));
    let header = visual.debug_bounds(surface.header()).unwrap();
    assert_eq!(header.top(), px(0.));
    assert_eq!(header.size.height, px(40.));
    assert!(header.left() >= rail.right());
    assert_eq!(surface.icon_left(visual), rail.right() + px(12.));
    if matches!(surface, Surface::Search) {
        assert!(visual.debug_bounds("classic-app-surface-header").is_none());
    }
    window
        .read_with(visual, |shell, cx| {
            assert!(shell.workspace.sidebar_visible);
            assert!(shell.workspace.project_rail_open);
            assert_eq!(shell.workspace.tabs, tabs);
            assert_eq!(shell.workspace.active, Some(session));
            assert_eq!(
                shell.sessions[&session].read(cx).input.entity_id(),
                conversation,
                "opening a destination retains the current conversation and draft field"
            );
        })
        .unwrap();

    window
        .update(visual, |shell, window, cx| surface.close(shell, window, cx))
        .unwrap();
    visual.run_until_parked();
    visual.refresh().unwrap();
    assert!(visual.debug_bounds("sidebar-content").is_some());
    assert!(visual.debug_bounds("title-tab-strip").is_some());
    assert!(visual.debug_bounds("classic-app-surface-header").is_none());
    window
        .read_with(visual, |shell, _| {
            assert!(shell.workspace.sidebar_visible);
            assert_eq!(shell.workspace.sidebar_width, 260.);
            assert_eq!(shell.workspace.tabs, tabs);
        })
        .unwrap();
}

#[gpui::test]
fn original_app_surfaces_inbox_uses_own_header_and_restores_sessions(cx: &mut TestAppContext) {
    assert_original_surface(Surface::Inbox, cx);
}

#[gpui::test]
fn original_app_surfaces_notes_uses_own_header_and_restores_sessions(cx: &mut TestAppContext) {
    assert_original_surface(Surface::Notes, cx);
}

#[gpui::test]
fn original_app_surfaces_automations_uses_own_header_and_restores_sessions(
    cx: &mut TestAppContext,
) {
    assert_original_surface(Surface::Automations, cx);
}

#[gpui::test]
fn original_app_surfaces_search_uses_search_field_header_and_restores_sessions(
    cx: &mut TestAppContext,
) {
    assert_original_surface(Surface::Search, cx);
}

#[gpui::test]
fn original_app_surfaces_keep_icon_rail_and_remember_closed_sessions(cx: &mut TestAppContext) {
    let (window, _) = setup(cx, true, false);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    window
        .update(visual, |shell, _, cx| {
            shell.workspace.project_rail_open = false;
            shell.workspace.sidebar_visible = false;
            shell.workspace.preferences.theme = "dark".into();
            shell.workspace.preferences.sidebar_opacity = 80;
            shell.workspace.preferences.body_glass = true;
            shell.layout_changed(cx);
        })
        .unwrap();
    visual.run_until_parked();
    visual.refresh().unwrap();
    let sessions_rail = visual.debug_bounds("project-rail-frame").unwrap();
    let sessions_picker = visual.debug_bounds("rail-current-project").unwrap();
    let sessions_toggle = visual.debug_bounds("toggle-sidebar").unwrap();
    let sessions_back = visual.debug_bounds("visit-back").unwrap();
    let sessions_forward = visual.debug_bounds("visit-forward").unwrap();
    assert_eq!(sessions_back.left(), sessions_toggle.right());
    assert_eq!(sessions_forward.right(), px(TRAFFIC_LIGHTS + 72.));
    for surface in [
        Surface::Inbox,
        Surface::Notes,
        Surface::Automations,
        Surface::Search,
    ] {
        window
            .update(visual, |shell, window, cx| surface.open(shell, window, cx))
            .unwrap();
        visual.run_until_parked();
        visual.refresh().unwrap();
        let rail = visual.debug_bounds("project-rail-frame").unwrap();
        assert_eq!(rail.size.width, px(48.));
        assert_eq!(
            rail, sessions_rail,
            "{surface:?} keeps the rail below the title bar"
        );
        #[cfg(target_os = "macos")]
        {
            let background = visual.debug_bounds("classic-surface-background").unwrap();
            assert_eq!(background.top(), rail.top());
            assert_eq!(background.bottom(), rail.bottom());
            window
                .update(visual, |_, window, _| {
                    let scale = window.scale_factor();
                    let quads = window.painted_quads();
                    let painted_at = |color: gpui::Hsla, x: f32, y: f32| {
                        let point = gpui::point(
                            gpui::ScaledPixels(x * scale),
                            gpui::ScaledPixels(y * scale),
                        );
                        quads.iter().any(|quad| {
                            quad.background == color.into()
                                && quad.bounds.contains(&point)
                                && quad.content_mask.bounds.contains(&point)
                        })
                    };
                    assert_ne!(shell_fill(), pane_fill());
                    // Test the actual wash on both sides of the rail edge, which
                    // passes between the native traffic lights at x=48.
                    for x in [24., 60.] {
                        assert!(painted_at(shell_fill(), x, 20.));
                        assert!(
                            !painted_at(pane_fill(), x, 20.),
                            "{surface:?} paints content tint behind the traffic lights"
                        );
                    }
                    assert!(
                        painted_at(pane_fill(), 60., 50.),
                        "the destination body retains its content tint"
                    );
                })
                .unwrap();
        }
        assert_eq!(
            visual.debug_bounds("rail-current-project").unwrap(),
            sessions_picker,
            "{surface:?} keeps collapsed rail controls in place"
        );
        assert!(visual.debug_bounds("rail-expand-projects").is_none());
        let toggle = visual.debug_bounds("toggle-sidebar").unwrap();
        assert_eq!(toggle, sessions_toggle);
        let forward = visual.debug_bounds("visit-forward").unwrap();
        assert_eq!(visual.debug_bounds("visit-back").unwrap(), sessions_back);
        assert_eq!(forward, sessions_forward);
        assert_eq!(surface.icon_left(visual), forward.right() + px(6.));
        let main = visual.debug_bounds("main-pane-frame").unwrap();
        assert_eq!(main.left(), rail.right());
        assert!(visual.debug_bounds("sidebar-content").is_none());
        assert!(visual.debug_bounds("title-tab-strip").is_none());
        let header = visual.debug_bounds(surface.header()).unwrap();
        assert_eq!(header.top(), px(0.));
        assert_eq!(header.size.height, px(40.));
        window
            .read_with(visual, |shell, _| {
                assert!(!shell.workspace.sidebar_visible);
                assert!(!shell.workspace.project_rail_open);
            })
            .unwrap();
        // The one titlebar button remains usable on every destination.
        for open in [true, false] {
            let toggle = visual.debug_bounds("toggle-sidebar").unwrap();
            visual.simulate_click(toggle.center(), Default::default());
            visual.run_until_parked();
            visual.refresh().unwrap();
            assert_eq!(
                visual.debug_bounds("toggle-sidebar").unwrap(),
                sessions_toggle
            );
            assert_eq!(
                visual
                    .debug_bounds("project-rail-frame")
                    .unwrap()
                    .size
                    .width,
                px(if open { 220. } else { 48. })
            );
            assert!(visual.debug_bounds("rail-expand-projects").is_none());
        }
        window
            .update(visual, |shell, window, cx| surface.close(shell, window, cx))
            .unwrap();
        visual.run_until_parked();
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("sidebar-content").is_none());
        assert!(visual.debug_bounds("title-tab-strip").is_some());
        window
            .read_with(visual, |shell, _| {
                assert!(!shell.workspace.sidebar_visible);
                assert!(!shell.workspace.project_rail_open);
                assert_eq!(shell.workspace.sidebar_width, 260.);
            })
            .unwrap();
    }
}

#[gpui::test]
fn original_app_surfaces_leave_both_new_layout_styles_tabbed(cx: &mut TestAppContext) {
    for boxy in [false, true] {
        let (window, _) = setup(cx, false, boxy);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        for surface in [
            Surface::Inbox,
            Surface::Notes,
            Surface::Automations,
            Surface::Search,
        ] {
            window
                .update(visual, |shell, window, cx| surface.open(shell, window, cx))
                .unwrap();
            visual.run_until_parked();
            visual.refresh().unwrap();
            assert!(
                visual.debug_bounds("sidebar-content").is_some(),
                "{surface:?} keeps the new layout sidebar (boxy={boxy})"
            );
            assert!(
                visual.debug_bounds("title-tab-strip").is_some(),
                "{surface:?} stays a workspace tab (boxy={boxy})"
            );
            let app = match surface {
                Surface::Inbox => Some(app_views::AppView::Inbox),
                Surface::Notes => Some(app_views::AppView::Notes),
                Surface::Automations => Some(app_views::AppView::Automations),
                _ => None,
            };
            if let Some(app) = app {
                let selector = Box::leak(format!("app-title-tab-{}", app.label()).into_boxed_str());
                assert!(
                    visual.debug_bounds(selector).is_some(),
                    "the destination itself has a tab (boxy={boxy})"
                );
                window
                    .read_with(visual, |shell, cx| {
                        assert!(shell.visible_tabs(cx).contains(&TitleTab::App(app)));
                        assert_eq!(shell.active_title_tab(), Some(TitleTab::App(app)));
                    })
                    .unwrap();
            }
            assert!(visual.debug_bounds("classic-app-surface-header").is_none());
            assert!(visual.debug_bounds("project-rail-frame").is_none());
            window
                .read_with(visual, |shell, _| {
                    assert!(!shell.workspace.classic_layout);
                    assert_eq!(shell.workspace.new_layout_boxy, boxy);
                    assert!(shell.workspace.sidebar_visible);
                })
                .unwrap();
            window
                .update(visual, |shell, window, cx| surface.close(shell, window, cx))
                .unwrap();
            visual.run_until_parked();
        }
    }
}

#[gpui::test]
fn new_layout_app_tabs_survive_frame_changes_and_close_like_session_tabs(cx: &mut TestAppContext) {
    cx.update(super::bind_keys);
    for boxy in [false, true] {
        let (window, session) = setup(cx, false, boxy);
        let visual = &mut VisualTestContext::from_window(window.into(), cx);
        for surface in [Surface::Inbox, Surface::Notes, Surface::Automations] {
            window
                .update(visual, |shell, window, cx| surface.open(shell, window, cx))
                .unwrap();
            visual.run_until_parked();
        }
        let order = window
            .update(visual, |shell, _, cx| {
                for app in [
                    app_views::AppView::Inbox,
                    app_views::AppView::Notes,
                    app_views::AppView::Automations,
                ] {
                    assert_eq!(
                        shell
                            .workspace
                            .title_order
                            .iter()
                            .filter(|tab| **tab == TitleTab::App(app))
                            .count(),
                        1
                    );
                }
                let order = shell.workspace.title_order.clone();
                shell.workspace.new_layout_boxy = !boxy;
                shell.layout_changed(cx);
                order
            })
            .unwrap();
        visual.run_until_parked();
        visual.refresh().unwrap();
        for app in [
            app_views::AppView::Inbox,
            app_views::AppView::Notes,
            app_views::AppView::Automations,
        ] {
            let selector = Box::leak(format!("app-title-tab-{}", app.label()).into_boxed_str());
            let tab = visual.debug_bounds(selector).unwrap();
            visual.simulate_click(tab.center(), Default::default());
            visual.run_until_parked();
            window
                .read_with(visual, |shell, _| {
                    assert_eq!(shell.active_app_view, Some(app));
                    assert_eq!(shell.workspace.title_order, order);
                })
                .unwrap();
        }
        // Close an inactive tab through its button, then the selected tabs
        // through Cmd-W, preserving the underlying conversation.
        visual.refresh().unwrap();
        let close = visual.debug_bounds("close-app-tab-Inbox").unwrap();
        visual.simulate_click(close.center(), Default::default());
        visual.run_until_parked();
        for _ in 0..2 {
            visual.simulate_keystrokes("cmd-w");
            visual.run_until_parked();
        }
        window
            .read_with(visual, |shell, _| {
                assert!(!shell.app_view_open());
                assert_eq!(shell.workspace.active, Some(session));
                assert!(
                    shell
                        .workspace
                        .title_order
                        .iter()
                        .all(|tab| !matches!(tab, TitleTab::App(_)))
                );
                assert_eq!(shell.active_title_tab(), Some(TitleTab::Session(session)));
            })
            .unwrap();
    }
}

fn assert_settings_navigation(classic: bool, cx: &mut TestAppContext) {
    let (window, session) = setup(cx, classic, false);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    window
        .update(visual, |shell, window, cx| {
            shell.open_settings(crate::settings::Section::General, window, cx)
        })
        .unwrap();
    visual.run_until_parked();
    visual.refresh().unwrap();

    // The toggle steps aside but keeps its slot, so the title bar holds
    // still, and the shortcut can't hide what Settings put in the sidebar's
    // place.
    let toggle = visual.debug_bounds("toggle-sidebar").unwrap();
    assert_eq!(toggle.size.width, px(24.));
    visual.dispatch_action(ToggleSidebar);
    visual.run_until_parked();
    window
        .read_with(visual, |shell, _| {
            assert!(shell.settings.is_some());
            assert!(shell.workspace.sidebar_visible);
            assert!(shell.workspace.project_rail_open);
        })
        .unwrap();

    visual.refresh().unwrap();
    let back = visual.debug_bounds("visit-back").unwrap();
    visual.simulate_click(back.center(), Default::default());
    visual.run_until_parked();
    window
        .read_with(visual, |shell, _| {
            assert!(shell.settings.is_none(), "Back leaves Settings");
            assert_eq!(shell.workspace.active, Some(session));
        })
        .unwrap();
}

#[gpui::test]
fn settings_back_and_sidebar_toggle_in_new_layout(cx: &mut TestAppContext) {
    assert_settings_navigation(false, cx);
}

#[gpui::test]
fn settings_back_and_sidebar_toggle_in_original_layout(cx: &mut TestAppContext) {
    assert_settings_navigation(true, cx);
}
