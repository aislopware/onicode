//! The explorer over a scripted engine: requests are read off the wire and
//! answered by hand, so each test sees exactly what the UI asked for.

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    AppContext as _, Entity, ExternalPaths, FileDropEvent, Modifiers, MouseButton, TestAppContext,
    VisualTestContext, point, px, size,
};
use onicode_rpc::{RpcClient, methods};
use serde_json::{Value, json};

use super::client::TransferFrame;
use super::tray::TransferTray;
use super::{ExplorerPage, Side};
use crate::state::{AppState, EngineHandle};

const LOCAL: &str = "mac";
const REMOTE: &str = "studio";

/// The engine's end of the wire.
struct Wire {
    runtime: Rc<tokio::runtime::Runtime>,
    requests: tokio::sync::mpsc::Receiver<String>,
    replies: tokio::sync::mpsc::Sender<String>,
    unanswered: Vec<Value>,
}

impl Wire {
    fn new() -> (Self, RpcClient) {
        let runtime = Rc::new(
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
        );
        let (out, requests) = tokio::sync::mpsc::channel(64);
        let (replies, inbound) = tokio::sync::mpsc::channel(64);
        let client = {
            let _guard = runtime.enter();
            RpcClient::new(out, inbound)
        };
        let wire = Self {
            runtime,
            requests,
            replies,
            unanswered: Vec::new(),
        };
        (wire, client)
    }

    /// Every request of `method` sent so far and not yet taken.
    fn take_all(&mut self, method: &str) -> Vec<Value> {
        while let Ok(raw) = self.requests.try_recv() {
            self.unanswered.push(serde_json::from_str(&raw).unwrap());
        }
        let (taken, rest): (Vec<Value>, Vec<Value>) = std::mem::take(&mut self.unanswered)
            .into_iter()
            .partition(|request| request["method"] == method);
        self.unanswered = rest;
        taken
    }

    fn take(&mut self, method: &str) -> Value {
        let mut taken = self.take_all(method);
        assert_eq!(
            taken.len(),
            1,
            "expected one {method} request, got {taken:?}"
        );
        taken.remove(0)
    }

    fn reply(&self, cx: &mut VisualTestContext, request: &Value, ok: Value) {
        let frame = json!({ "id": request["id"], "ok": ok }).to_string();
        let replies = self.replies.clone();
        self.runtime.block_on(async {
            replies.send(frame).await.unwrap();
            // The client's reader runs on this runtime; wait until it took the frame.
            tokio::time::timeout(Duration::from_secs(1), async {
                while replies.capacity() < replies.max_capacity() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        });
        cx.run_until_parked();
    }
}

fn entry(name: &str, kind: &str) -> Value {
    json!({ "name": name, "kind": kind, "size": 2048, "modifiedMs": 0, "mode": 0o644 })
}

fn listing(path: &str, parent: Option<&str>, entries: Vec<Value>) -> Value {
    json!({ "path": path, "parent": parent, "entries": entries })
}

fn state(cx: &mut gpui::App, client: RpcClient) -> Entity<AppState> {
    cx.new(|_| {
        let mut state = AppState::new();
        state.local_device_id = Some(LOCAL.into());
        state.set_test_engine(EngineHandle::from_test_client(client));
        state
    })
}

/// The page in a window, both panes showing this Mac's home.
fn open<'a>(
    cx: &'a mut TestAppContext,
    wire: &mut Wire,
    client: RpcClient,
) -> (Entity<ExplorerPage>, &'a mut VisualTestContext) {
    cx.update(|cx| cx.set_global(crate::theme::Theme::default()));
    let (page, cx) = cx.add_window_view(|_, cx| {
        let state = state(cx, client);
        ExplorerPage::new(state, cx)
    });
    cx.simulate_resize(size(px(1280.0), px(800.0)));
    cx.run_until_parked();
    let home = listing(
        "/Users/me",
        Some("/Users"),
        vec![entry("Documents", "dir"), entry("notes.txt", "file")],
    );
    for request in wire.take_all(methods::LIST_PATH) {
        assert_eq!(request["params"]["path"], "~");
        assert!(request["params"].get("targetDeviceId").is_none());
        wire.reply(cx, &request, home.clone());
    }
    (page, cx)
}

/// Shows the second pane on the remote device's empty home.
fn split_to_remote(page: &Entity<ExplorerPage>, cx: &mut VisualTestContext, wire: &mut Wire) {
    page.update(cx, |page, cx| {
        page.dual = true;
        page.navigate(Side::Right, REMOTE.into(), "/home/me".into(), None, cx);
    });
    cx.run_until_parked();
    let request = wire.take(methods::LIST_PATH);
    assert_eq!(request["params"]["targetDeviceId"], REMOTE);
    wire.reply(cx, &request, listing("/home/me", Some("/home"), Vec::new()));
}

#[gpui::test]
fn keys_walk_into_a_folder_and_back_out(cx: &mut TestAppContext) {
    let (mut wire, client) = Wire::new();
    let runtime = wire.runtime.clone();
    let _guard = runtime.enter();
    let (page, cx) = open(cx, &mut wire, client);
    page.update_in(cx, |page, window, cx| page.left.focus.focus(window, cx));

    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();
    let request = wire.take(methods::LIST_PATH);
    assert_eq!(request["params"]["path"], "/Users/me/Documents");
    wire.reply(
        cx,
        &request,
        listing(
            "/Users/me/Documents",
            Some("/Users/me"),
            vec![entry("a.pdf", "file")],
        ),
    );
    page.read_with(cx, |page, _| {
        assert_eq!(page.left.path, "/Users/me/Documents");
        assert_eq!(page.left.rows.len(), 1);
    });

    cx.simulate_keystrokes("cmd-up");
    cx.run_until_parked();
    let request = wire.take(methods::LIST_PATH);
    assert_eq!(request["params"]["path"], "/Users/me");
    wire.reply(
        cx,
        &request,
        listing(
            "/Users/me",
            Some("/Users"),
            vec![entry("Documents", "dir"), entry("notes.txt", "file")],
        ),
    );
    page.read_with(cx, |page, _| {
        let cursor = page.left.cursor_row().map(|row| row.path.clone());
        assert_eq!(cursor.as_deref(), Some("/Users/me/Documents"));
    });
}

#[gpui::test]
fn dragging_a_row_to_the_other_pane_starts_a_transfer(cx: &mut TestAppContext) {
    let (mut wire, client) = Wire::new();
    let runtime = wire.runtime.clone();
    let _guard = runtime.enter();
    let (page, cx) = open(cx, &mut wire, client);
    split_to_remote(&page, cx, &mut wire);

    let row = cx.debug_bounds("explorer-row-left-1").unwrap().center();
    let target = cx.debug_bounds("explorer-list-right").unwrap().center();
    cx.simulate_mouse_down(row, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(
        row + point(px(24.0), px(0.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    let request = wire.take(methods::START_TRANSFER);
    let params = &request["params"];
    assert_eq!(params["from"]["deviceId"], LOCAL);
    assert_eq!(params["from"]["paths"], json!(["/Users/me/notes.txt"]));
    assert_eq!(params["to"]["deviceId"], REMOTE);
    assert_eq!(params["to"]["dir"], "/home/me");
    assert_eq!(params["conflict"], "ask");
    // This Mac's engine takes the job; it is never forwarded.
    assert!(params.get("targetDeviceId").is_none());
}

#[gpui::test]
fn a_finder_drop_is_pulled_from_this_mac(cx: &mut TestAppContext) {
    let (mut wire, client) = Wire::new();
    let runtime = wire.runtime.clone();
    let _guard = runtime.enter();
    let (page, cx) = open(cx, &mut wire, client);
    split_to_remote(&page, cx, &mut wire);

    let target = cx.debug_bounds("explorer-list-right").unwrap().center();
    cx.simulate_event(FileDropEvent::Entered {
        position: target,
        paths: ExternalPaths(vec![std::path::PathBuf::from("/tmp/report.pdf")].into()),
    });
    cx.simulate_event(FileDropEvent::Pending { position: target });
    cx.simulate_event(FileDropEvent::Submit { position: target });
    cx.run_until_parked();

    let request = wire.take(methods::START_TRANSFER);
    let params = &request["params"];
    assert_eq!(params["from"]["deviceId"], LOCAL);
    assert_eq!(params["from"]["paths"], json!(["/tmp/report.pdf"]));
    assert_eq!(params["to"]["deviceId"], REMOTE);
    assert_eq!(params["to"]["dir"], "/home/me");
}

#[gpui::test]
fn a_conflict_offers_its_answers_and_sends_the_chosen_one(cx: &mut TestAppContext) {
    let (mut wire, client) = Wire::new();
    let runtime = wire.runtime.clone();
    let _guard = runtime.enter();
    cx.update(|cx| cx.set_global(crate::theme::Theme::default()));
    let (tray, cx) = cx.add_window_view(|_, cx| TransferTray::new(state(cx, client)));
    cx.simulate_resize(size(px(900.0), px(700.0)));
    let update = json!({
        "transferId": "t1",
        "from": { "deviceId": LOCAL, "paths": ["/Users/me/a.txt"] },
        "to": { "deviceId": REMOTE, "dir": "/home/me" },
        "state": "conflict",
        "items": 1, "itemsDone": 0, "bytes": 10, "bytesDone": 0, "bytesPerSec": 0,
        "conflict": {
            "path": "/home/me/a.txt",
            "existingSize": 5, "existingModifiedMs": 0,
            "incomingSize": 10, "incomingModifiedMs": 0
        }
    });
    tray.update(cx, |tray, cx| {
        tray.apply(REMOTE, TransferFrame::parse(update).unwrap(), cx);
    });
    cx.run_until_parked();

    for selector in [
        "transfer-skip-t1",
        "transfer-keep-both-t1",
        "transfer-replace-t1",
        "transfer-apply-all-t1",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} missing");
    }
    let tick = cx.debug_bounds("transfer-apply-all-t1").unwrap().center();
    cx.simulate_click(tick, Modifiers::none());
    cx.run_until_parked();
    let replace = cx.debug_bounds("transfer-replace-t1").unwrap().center();
    cx.simulate_click(replace, Modifiers::none());
    cx.run_until_parked();

    let request = wire.take(methods::RESOLVE_CONFLICT);
    let params = &request["params"];
    assert_eq!(params["transferId"], "t1");
    assert_eq!(params["choice"], "replace");
    assert_eq!(params["applyToAll"], true);
    // The job's owner answers it.
    assert_eq!(params["targetDeviceId"], REMOTE);
}
