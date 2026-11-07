//! The App access hub over a store in memory: a request id starts one session whatever the
//! retries, its prompt waits in the composer, and a kept receipt reads back as kept.
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use onicode_core::agent::Settings;
use onicode_core::store::Store;
use onicode_protocol::app_access::{ReadAppReceipt, SaveAppReceipt, StartAppSession};
use onicode_protocol::provider::Kind;
use onicode_protocol::{OpId, Outcome};
use serde_json::{Value, json};

use super::AppAccessHub;
use crate::session::ThreadSpawn;
use crate::turn::{Database, locked};
use crate::workspace::WorkspaceHub;

fn hub() -> (Database, Arc<AppAccessHub>) {
    let store: Database = Arc::new(Mutex::new(Store::open(Path::new(":memory:")).unwrap()));
    let spawn = Arc::new(ThreadSpawn);
    let workspace = WorkspaceHub::new(store.clone(), spawn.clone());
    (store.clone(), AppAccessHub::new(store, workspace, spawn))
}

fn start(caller: i64, request: &str, input: &Value) -> StartAppSession {
    StartAppSession {
        caller,
        request: request.into(),
        fingerprint: onicode_core::app_access::fingerprint("sessions.start", input),
        input: input.to_string(),
        provider: "claude".into(),
        kind: Kind::Claude,
        project: "/project".into(),
        prompt: input["prompt"].as_str().unwrap().into(),
        settings: Settings::default(),
    }
}

#[test]
fn a_request_starts_one_session_with_its_prompt_waiting() {
    let (store, hub) = hub();
    let caller = locked(&store)
        .unwrap()
        .create("claude", "/project")
        .unwrap();
    let input = json!({"prompt": "Fix the flaky test", "project": "/project", "title": "Flaky"});

    let first = block_on(hub.start(Some(OpId::new()), start(caller.id, "r1", &input))).unwrap();
    let (session, _) = first.started.clone().expect("the first call starts it");
    assert_eq!(session.title, "Flaky");
    let value: Value = serde_json::from_str(&first.value).unwrap();
    assert_eq!(value["sessionId"], json!(session.id));
    assert_eq!(value["submitted"], json!(true));
    let draft = locked(&store)
        .unwrap()
        .follow_ups(session.id)
        .unwrap()
        .draft;
    assert_eq!(
        draft.map(|draft| draft.text).as_deref(),
        Some("Fix the flaky test")
    );

    // A retry under a new op finds the session the request started, and starts nothing.
    let again = block_on(hub.start(Some(OpId::new()), start(caller.id, "r1", &input))).unwrap();
    assert!(again.started.is_none());
    let value: Value = serde_json::from_str(&again.value).unwrap();
    assert_eq!(value["sessionId"], json!(session.id));
    assert_eq!(locked(&store).unwrap().sessions().unwrap().len(), 2);

    // The same request id with other input is refused.
    let other = json!({"prompt": "Something else", "project": "/project"});
    assert!(block_on(hub.start(Some(OpId::new()), start(caller.id, "r1", &other))).is_err());
}

#[test]
fn a_kept_receipt_reads_back() {
    let (_, hub) = hub();
    let read = ReadAppReceipt {
        session: 4,
        request: "r1".into(),
    };
    assert_eq!(hub.receipt(&read).unwrap(), None);
    let receipt = json!(["fingerprint", {"Ok": {"stopped": true}}]).to_string();
    let saved = block_on(hub.save_receipt(
        Some(OpId::new()),
        SaveAppReceipt {
            session: 4,
            request: "r1".into(),
            receipt: receipt.clone(),
        },
    ))
    .unwrap();
    assert_eq!(saved, Outcome::Applied);
    assert_eq!(hub.receipt(&read).unwrap(), Some(receipt));
    let other = ReadAppReceipt {
        session: 5,
        request: "r1".into(),
    };
    assert_eq!(hub.receipt(&other).unwrap(), None);
}
