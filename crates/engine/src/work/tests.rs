//! The Work hub over a scripted Inbox and a scripted `gh`: no network, no CLI. The Inbox's
//! calls reach GitHub with what the viewer named and run a mutating op once; automations
//! round-trip through the store the workspace hub schedules from; and linked activity shows an
//! item exactly when it changed after the session last saw it, reading an unchanged item's feeds
//! once.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use onicode_core::automations::AutomationUpsert;
use onicode_core::inbox::{Checks, Details, Diff, Item, Snapshot, Thread, ViewState};
use onicode_core::store::Store;
use onicode_protocol::sessions::{Session, WorkItem};
use onicode_protocol::work::{
    DeleteAutomation, InboxList, LinkedActivity, RunAutomationNow, RunInboxAction,
    SetAutomationEnabled, UpsertAutomation,
};
use onicode_protocol::{OpId, Outcome};
use proptest::prelude::*;

use super::{Gh, Place, Trackers, WorkHub};
use crate::session::ThreadSpawn;
use crate::turn::{Database, locked};
use crate::workspace::WorkspaceHub;

fn store() -> Database {
    Arc::new(Mutex::new(Store::in_memory().unwrap()))
}

/// An Inbox that lists one item per project and counts the actions it runs.
#[derive(Default)]
struct Scripted {
    listed: Mutex<Vec<(PathBuf, Vec<PathBuf>, bool)>>,
    actions: AtomicUsize,
}

impl Trackers for Scripted {
    fn list(&self, at: &Place, projects: &[PathBuf], state: &ViewState) -> Snapshot {
        self.listed.lock().unwrap().push((
            at.cwd.clone(),
            projects.to_vec(),
            state.filters.assigned_to_me,
        ));
        Snapshot {
            items: projects
                .iter()
                .map(|project| Item {
                    project_path: project.to_string_lossy().into_owned(),
                    ..Item::default()
                })
                .collect(),
            ..Snapshot::default()
        }
    }

    fn details(&self, _: &Place, _: &Item) -> Result<Details, String> {
        Ok(Details::default())
    }

    fn thread(&self, _: &Place, _: &Item) -> Result<Thread, String> {
        Ok(Thread::default())
    }

    fn diff(&self, _: &Place, _: &Item) -> Result<Diff, String> {
        Ok(Diff::default())
    }

    fn checks(&self, _: &Place, _: &Item) -> Result<Checks, String> {
        Ok(Checks::default())
    }

    fn comment(&self, _: &Place, _: &Item, body: String, _: String) -> Result<String, String> {
        Ok(body)
    }

    fn action(&self, _: &Place, item: &Item, action: &str) -> Result<Item, String> {
        self.actions.fetch_add(1, Ordering::Relaxed);
        Ok(Item {
            state: action.into(),
            ..item.clone()
        })
    }

    fn media(&self, url: &str) -> Result<Vec<u8>, String> {
        Ok(url.as_bytes().to_vec())
    }

    fn avatar(&self, url: &str) -> Result<Vec<u8>, String> {
        Ok(url.as_bytes().to_vec())
    }
}

/// A GitHub where one pull request was updated at `updated`, with one comment then; the
/// GraphQL summary request fails when `graphql` is unset, so the hub falls back to REST.
struct Hub {
    updated: String,
    graphql: bool,
    calls: Mutex<Vec<Vec<String>>>,
}

impl Hub {
    fn new(updated: &str, graphql: bool) -> Self {
        Self {
            updated: updated.into(),
            graphql,
            calls: Mutex::default(),
        }
    }

    fn rest_calls(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|args| args.get(1).is_some_and(|arg| arg != "graphql"))
            .count()
    }
}

impl Gh for Hub {
    fn run(&self, args: &[String]) -> Result<String, String> {
        self.calls.lock().unwrap().push(args.to_vec());
        if args[1] == "graphql" {
            if !self.graphql {
                return Err("GraphQL is down".into());
            }
            return Ok(serde_json::json!({"data": {"i0": {"issueOrPullRequest": {
                "title": "Fix it", "state": "MERGED", "merged": true, "updatedAt": self.updated,
            }}}})
            .to_string());
        }
        let endpoint = &args[3];
        let body = if endpoint == "repos/o/r/pulls/7" {
            serde_json::json!({"title": "Fix it", "state": "closed", "merged": true,
                "updated_at": self.updated})
        } else if endpoint.starts_with("repos/o/r/issues/7/comments") {
            serde_json::json!([{"id": 1, "user": {"login": "ana"}, "body": "Looks  good",
                "created_at": self.updated, "html_url": "https://github.com/o/r/pull/7#c1"}])
        } else {
            serde_json::json!([])
        };
        Ok(body.to_string())
    }
}

fn hub(store: &Database, trackers: Arc<Scripted>, gh: Arc<Hub>) -> Arc<WorkHub> {
    let spawn = Arc::new(ThreadSpawn);
    let workspace = WorkspaceHub::new(store.clone(), spawn.clone());
    WorkHub::with(store.clone(), workspace, trackers, gh, spawn)
}

fn linked(store: &Database, updated_at: u64) -> Session {
    let mut session = locked(store).unwrap().create("claude", "/project").unwrap();
    session.updated_at = updated_at;
    session.work_item = Some(WorkItem {
        kind: "pr".into(),
        repo: "o/r".into(),
        number: 7,
        url: "https://github.com/o/r/pull/7".into(),
    });
    session
}

/// 2026-01-02T00:00:00Z.
const UPDATED: &str = "2026-01-02T00:00:00Z";
const UPDATED_MS: i64 = 1_767_312_000_000;

#[test]
fn the_inbox_lists_what_the_viewer_names_with_the_engines_sessions() {
    let store = store();
    let trackers = Arc::new(Scripted::default());
    let hub = hub(&store, trackers.clone(), Arc::new(Hub::new(UPDATED, true)));
    let session = locked(&store).unwrap().create("claude", "/a").unwrap();
    let mut state = ViewState::default();
    state.filters.assigned_to_me = true;
    let listing = block_on(hub.inbox_list(InboxList {
        cwd: "/a".into(),
        projects: vec!["/a".into(), "/b".into()],
        state,
    }))
    .unwrap();
    assert_eq!(listing.items.len(), 2);
    assert_eq!(listing.sessions, vec![session]);
    assert_eq!(
        *trackers.listed.lock().unwrap(),
        vec![(
            PathBuf::from("/a"),
            vec![PathBuf::from("/a"), PathBuf::from("/b")],
            true
        )]
    );
}

#[test]
fn a_repeated_inbox_action_runs_once() {
    let store = store();
    let trackers = Arc::new(Scripted::default());
    let hub = hub(&store, trackers.clone(), Arc::new(Hub::new(UPDATED, true)));
    let op = OpId::new();
    let body = RunInboxAction {
        cwd: "/a".into(),
        item: Item::default(),
        action: "close".into(),
    };
    let first = block_on(hub.action(Some(op), body.clone())).unwrap();
    let again = block_on(hub.action(Some(op), body)).unwrap();
    assert_eq!(first.state, "close");
    assert_eq!(again.state, "close");
    assert_eq!(trackers.actions.load(Ordering::Relaxed), 1);
}

#[test]
fn automations_round_trip_and_a_repeated_run_starts_once() {
    let store = store();
    let hub = hub(
        &store,
        Arc::new(Scripted::default()),
        Arc::new(Hub::new(UPDATED, true)),
    );
    let mut draft = AutomationUpsert::new("/project".into(), "claude".into(), String::new());
    draft.name = "Triage".into();
    draft.prompt = "Triage new issues".into();
    draft.sync_triggers(i64::try_from(onicode_core::time::now_ms()).unwrap());
    let saved =
        block_on(hub.upsert_automation(Some(OpId::new()), UpsertAutomation { automation: draft }))
            .unwrap();
    assert_eq!(
        hub.automations()
            .unwrap()
            .iter()
            .map(|a| a.id.clone())
            .collect::<Vec<_>>(),
        vec![saved.id.clone()]
    );

    let off = block_on(hub.set_automation_enabled(
        Some(OpId::new()),
        SetAutomationEnabled {
            automation: saved.id.clone(),
            enabled: false,
        },
    ))
    .unwrap();
    assert!(!off.enabled);

    let op = OpId::new();
    let run = RunAutomationNow {
        automation: saved.id.clone(),
    };
    let first = block_on(hub.run_automation_now(Some(op), run.clone())).unwrap();
    let again = block_on(hub.run_automation_now(Some(op), run)).unwrap();
    assert_eq!(first.run.id, again.run.id);
    assert_eq!(first.automation.id, saved.id);
    let runs = hub
        .automation_runs(&onicode_protocol::work::AutomationRuns {
            automation: saved.id.clone(),
        })
        .unwrap();
    assert_eq!(runs.len(), 1);

    let deleted = block_on(hub.delete_automation(
        Some(OpId::new()),
        DeleteAutomation {
            automation: saved.id,
        },
    ))
    .unwrap();
    assert_eq!(deleted, Outcome::Applied);
    assert!(hub.automations().unwrap().is_empty());
}

#[test]
fn an_unchanged_item_reads_its_feeds_once() {
    let store = store();
    let gh = Arc::new(Hub::new(UPDATED, true));
    let hub = hub(&store, Arc::new(Scripted::default()), gh.clone());
    let session = linked(&store, 0);
    let ask = || LinkedActivity {
        sessions: vec![session.clone()],
        seen: BTreeMap::new(),
    };
    let first = block_on(hub.linked_activity(ask())).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].state, "merged");
    assert_eq!(first[0].updated_at, UPDATED_MS);
    assert_eq!(first[0].entries.len(), 1);
    assert_eq!(first[0].entries[0].text, "Looks good");
    assert_eq!(first[0].entries[0].author, "ana");
    let reads = gh.rest_calls();
    assert_eq!(
        reads, 4,
        "the comments, reviews, review comments and commits"
    );

    let second = block_on(hub.linked_activity(ask())).unwrap();
    assert_eq!(second, first);
    assert_eq!(gh.rest_calls(), reads, "no feed is read again");
}

#[test]
fn a_failed_summary_query_falls_back_to_rest() {
    let store = store();
    let gh = Arc::new(Hub::new(UPDATED, false));
    let hub = hub(&store, Arc::new(Scripted::default()), gh.clone());
    let cards = block_on(hub.linked_activity(LinkedActivity {
        sessions: vec![linked(&store, 0)],
        seen: BTreeMap::new(),
    }))
    .unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].state, "merged");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// A card shows exactly when the item changed after both the session's own update and the
    /// time its last card was acknowledged; an archived session never shows one.
    #[test]
    fn a_card_shows_exactly_when_the_item_changed_since_last_seen(
        updated_at in 0u64..(2 * UPDATED_MS as u64),
        seen in proptest::option::of(0i64..(2 * UPDATED_MS)),
        archived in any::<bool>(),
    ) {
        let store = store();
        let hub = hub(
            &store,
            Arc::new(Scripted::default()),
            Arc::new(Hub::new(UPDATED, true)),
        );
        let mut session = linked(&store, updated_at);
        session.archived = archived;
        let cards = block_on(hub.linked_activity(LinkedActivity {
            seen: seen.map(|at| (session.id, at)).into_iter().collect(),
            sessions: vec![session.clone()],
        }))
        .unwrap();
        let since = (updated_at as i64).max(seen.unwrap_or(0));
        prop_assert_eq!(cards.len(), usize::from(!archived && UPDATED_MS > since));
    }
}
