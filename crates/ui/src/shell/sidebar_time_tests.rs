//! Sidebar time labels turn over on their own under retained drawing.
use std::time::Duration;

use gpui::{AppContext, RetainedCheck, TestAppContext, VisualTestContext};

use super::*;

/// Draws the real sidebar view, as the window does, and nothing else.
struct PaneHost {
    pane: Entity<SidebarPane>,
    _shell: Entity<Shell>,
    _data_dir: tempfile::TempDir,
}

impl Render for PaneHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(280.0)).h(px(800.0)).child(self.pane.clone())
    }
}

fn setup(cx: &mut TestAppContext) -> &mut VisualTestContext {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| {
        gpui_base::init(cx);
        cx.set_global(Theme::default());
        crate::app_menus::init(cx);
        settings::init(UiSettings::default(), dir.path(), cx);
        crate::history::init(
            Default::default(),
            Default::default(),
            Default::default(),
            Default::default(),
            cx,
        );
        apply_keymap(
            cx,
            &KeymapConfig::default(),
            ComposerSendBehavior::default(),
        );
    });
    let (_, cx) = cx.add_window_view(|_, cx| {
        let shell = cx.new(|cx| {
            let state = cx.new(|_| AppState::new());
            let mut shell = Shell::new(
                state,
                EngineBootConfig {
                    data_dir: dir.path().into(),
                    ipc_port: 0,
                    edge_url: "http://127.0.0.1:1".into(),
                    edge_token: None,
                    org_id: None,
                    workos_client_id: None,
                    default_harness: onicode_proto::HarnessId::Mock,
                },
                cx,
            );
            shell.settings.sidebar_organization = SidebarOrganization::InOneList;
            // The roomy row wears the time in its corner.
            shell.settings.sidebar_compact = false;
            shell.reduced_motion = true;
            // Active 50s ago: "now" for ten more seconds, then "1m".
            let created = crate::label_clock::utc_now(cx) - chrono::Duration::seconds(50);
            shell.state.update(cx, |state, _| {
                state.workspace_scope = Some(WorkspaceScope::Local);
                state.local_device_id = Some("local".into());
                state.no_project = true;
                state.chats = vec![
                    serde_json::from_value(serde_json::json!({
                        "id": "idle", "title": "idle", "deviceId": "local", "archived": false,
                        "createdAt": created,
                    }))
                    .unwrap(),
                ];
            });
            shell
        });
        PaneHost {
            pane: shell.read(cx).sidebar_pane.clone(),
            _shell: shell,
            _data_dir: dir,
        }
    });
    cx.update(|window, cx| {
        // A frame drawn from the last one must match one built afresh.
        window.set_check_retained(Some(RetainedCheck::Panic));
        window.draw(cx).clear(cx);
    });
    cx
}

/// Nothing but the clock changes, yet the row's "now" becomes "1m": the
/// sidebar asked to be woken when its label turns over. A stale sidebar
/// keeps "now" (and the retained check panics on the explicit frame).
#[gpui::test]
fn an_idle_rows_time_label_turns_over_on_its_own(cx: &mut TestAppContext) {
    let cx = setup(cx);
    assert!(cx.debug_bounds("chat-idle-time-now").is_some());

    cx.executor().advance_clock(Duration::from_secs(9));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("chat-idle-time-now").is_some());

    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("chat-idle-time-1m").is_some());
    assert!(cx.debug_bounds("chat-idle-time-now").is_none());
}
