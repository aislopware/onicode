//! The engine settings against their reference (engine.md "Tests"): typed keys set in any order,
//! some under ops sent twice, while viewers join, fall behind, drop and come back at cursors they
//! held. Every viewer must fold to the hub's settings, a snapshot plus the events after it must be
//! the later snapshot, and a repeated op must get its first receipt and change nothing again.
//! Then the move out of monocode's saved preferences, which runs once.
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use onicode_core::store::Store;
use onicode_protocol::engine_settings::{
    EngineSetting, EngineSettings, EngineSettingsEvent, ModelChoice, SetEngineSetting,
    WatchEngineSettings,
};
use onicode_protocol::{Cursor, Item, OpId, Outcome};
use proptest::prelude::*;

use super::{Effects, KEY, LEGACY_WORKSPACE, SettingsFeed, SettingsHub, VIEWER_KEYS};
use crate::feed::{Feed, Limits};
use crate::turn::Database;

type Stream = BoxStream<'static, Item<EngineSettings, EngineSettingsEvent>>;

fn store() -> Database {
    Arc::new(Mutex::new(Store::open(Path::new(":memory:")).unwrap()))
}

/// Counts what the settings changed outside the hub.
#[derive(Default)]
struct Counted {
    applied: AtomicUsize,
    last: Mutex<Option<EngineSettings>>,
}

impl Effects for Counted {
    fn applied(&self, settings: &EngineSettings) {
        self.applied.fetch_add(1, Ordering::Relaxed);
        *self.last.lock().unwrap() = Some(settings.clone());
    }
}

fn hub(store: &Database, limits: Limits) -> (Arc<SettingsHub>, Arc<Counted>) {
    let effects = Arc::new(Counted::default());
    let hub = SettingsHub::with(store.clone(), effects.clone(), limits).unwrap();
    (hub, effects)
}

fn shown(hub: &Arc<SettingsHub>) -> (Cursor, EngineSettings) {
    match hub
        .watch(&WatchEngineSettings { since: None })
        .next()
        .now_or_never()
    {
        Some(Some(Item::Snapshot { at, state })) => (at, state),
        other => panic!("a new watcher starts from a snapshot, not {other:?}"),
    }
}

/// A viewer of the settings: its stream, what it folded, its cursor, and every (cursor, settings)
/// it held, to come back at.
struct Viewer {
    stream: Option<Stream>,
    shown: Option<(Cursor, EngineSettings)>,
    held: Vec<(Cursor, EngineSettings)>,
    /// Each receipt with the seq of the event that carried it.
    receipts: Vec<(u64, OpId, Outcome)>,
}

impl Viewer {
    fn join(hub: &Arc<SettingsHub>) -> Self {
        Self {
            stream: Some(hub.watch(&WatchEngineSettings { since: None })),
            shown: None,
            held: Vec::new(),
            receipts: Vec::new(),
        }
    }

    fn pump(&mut self, hub: &Arc<SettingsHub>) {
        loop {
            let Some(stream) = self.stream.as_mut() else {
                return;
            };
            let Some(Some(item)) = stream.next().now_or_never() else {
                return;
            };
            match item {
                Item::Snapshot { at, state } => self.shown = Some((at, state)),
                Item::Events {
                    first,
                    last,
                    events,
                } => {
                    let (cursor, state) = self.shown.as_mut().expect("events after a snapshot");
                    assert_eq!(first, cursor.seq + 1, "no gap and no repeat");
                    for (seq, event) in (first..).zip(&events) {
                        SettingsFeed::apply(state, event);
                        if let EngineSettingsEvent::Receipt { op, outcome } = event {
                            self.receipts.push((seq, *op, outcome.clone()));
                        }
                    }
                    cursor.seq = last;
                }
                Item::Resync => {
                    let since = self.shown.as_ref().map(|(cursor, _)| *cursor);
                    self.stream = Some(hub.watch(&WatchEngineSettings { since }));
                }
            }
            if let Some(shown) = &self.shown {
                self.held.push(shown.clone());
            }
        }
    }

    fn rejoin(&mut self, hub: &Arc<SettingsHub>, pick: usize) {
        let back = (!self.held.is_empty()).then(|| self.held[pick % self.held.len()].clone());
        self.stream = None;
        let since = back.as_ref().map(|(cursor, _)| *cursor);
        // Coming back to an older cursor replays what followed it, receipts included.
        let kept = since.map_or(0, |cursor| cursor.seq);
        self.receipts.retain(|(seq, ..)| *seq <= kept);
        self.shown = back;
        self.stream = Some(hub.watch(&WatchEngineSettings { since }));
    }
}

fn setting() -> impl Strategy<Value = EngineSetting> {
    prop_oneof![
        any::<bool>().prop_map(EngineSetting::KeepAwake),
        any::<bool>().prop_map(EngineSetting::ClaudeHooks),
        (0..6_u32).prop_map(EngineSetting::IdleAgents),
        prop::option::of("[a-c]").prop_map(EngineSetting::DefaultProvider),
        prop::collection::vec("[a-c]", 0..3).prop_map(EngineSetting::HiddenProviders),
        prop::collection::btree_map("[a-c]", "[x-z]{1,3}", 0..3)
            .prop_map(EngineSetting::ProviderEfforts),
        prop::option::of(("[a-c]", prop::option::of("[x-z]"))).prop_map(|choice| {
            EngineSetting::BackgroundModel(
                choice.map(|(provider, model)| ModelChoice { provider, model }),
            )
        }),
    ]
}

#[derive(Clone, Debug)]
enum Step {
    /// A viewer sets a key, under op number `op` when given: a number already used repeats it.
    Set {
        setting: EngineSetting,
        op: Option<u8>,
    },
    Join,
    Pump {
        viewer: usize,
    },
    Drop {
        viewer: usize,
    },
    Rejoin {
        viewer: usize,
        pick: usize,
    },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        5 => (setting(), prop::option::of(0..6_u8))
            .prop_map(|(setting, op)| Step::Set { setting, op }),
        2 => Just(Step::Join),
        4 => any::<usize>().prop_map(|viewer| Step::Pump { viewer }),
        1 => any::<usize>().prop_map(|viewer| Step::Drop { viewer }),
        2 => (any::<usize>(), any::<usize>())
            .prop_map(|(viewer, pick)| Step::Rejoin { viewer, pick }),
    ]
}

fn run(steps: &[Step], limits: Limits) {
    let store = store();
    let (hub, effects) = hub(&store, limits);
    let mut model = EngineSettings::default();
    let mut ops: BTreeMap<u8, OpId> = BTreeMap::new();
    let mut applied = 1;
    let mut viewers: Vec<Viewer> = Vec::new();
    for step in steps {
        match step {
            Step::Set { setting, op } => {
                let repeat = op.is_some_and(|op| ops.contains_key(&op));
                let op = op.map(|op| *ops.entry(op).or_default());
                let body = SetEngineSetting {
                    setting: setting.clone(),
                };
                assert_eq!(block_on(hub.set(op, body)), Ok(Outcome::Applied));
                if !repeat {
                    model.apply(setting);
                    applied += 1;
                }
                assert_eq!(
                    effects.applied.load(Ordering::Relaxed),
                    applied,
                    "a repeated op changes nothing again"
                );
            }
            Step::Join => {
                if viewers.len() < 8 {
                    viewers.push(Viewer::join(&hub));
                }
            }
            Step::Pump { viewer } => {
                if !viewers.is_empty() {
                    let count = viewers.len();
                    let viewer = &mut viewers[viewer % count];
                    viewer.pump(&hub);
                    let (_, folded) = viewer.shown.as_ref().expect("a pumped viewer shows");
                    assert_eq!(folded, &model, "a viewer folds to the settings");
                }
            }
            Step::Drop { viewer } => {
                if !viewers.is_empty() {
                    let count = viewers.len();
                    viewers.remove(viewer % count);
                }
            }
            Step::Rejoin { viewer, pick } => {
                if !viewers.is_empty() {
                    let count = viewers.len();
                    viewers[viewer % count].rejoin(&hub, *pick);
                }
            }
        }
        assert_eq!(
            hub.current(),
            model,
            "the hub holds the model after {step:?}"
        );
    }
    let (cursor, now) = shown(&hub);
    assert_eq!(now, model);
    for viewer in &mut viewers {
        viewer.pump(&hub);
        let (held, folded) = viewer.shown.as_ref().expect("a pumped viewer shows");
        assert_eq!(folded, &now, "every viewer ends up showing the hub");
        assert_eq!(held, &cursor, "every viewer ends up at the hub's cursor");
        let mut seen = std::collections::BTreeSet::new();
        for (_, op, outcome) in &viewer.receipts {
            assert_eq!(outcome, &Outcome::Applied);
            assert!(seen.insert(*op), "an op's receipt is published once");
        }
    }
    // What was saved is what a later engine run starts from.
    let (again, _) = hub_again(&store);
    assert_eq!(again.current(), model);
}

fn hub_again(store: &Database) -> (Arc<SettingsHub>, Arc<Counted>) {
    hub(store, Limits::default())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn viewers_fold_to_the_settings(steps in prop::collection::vec(step(), 0..80)) {
        run(&steps, Limits::default());
    }

    #[test]
    fn slow_viewers_merge_resync_and_rejoin(steps in prop::collection::vec(step(), 0..80)) {
        run(&steps, Limits { ring: (4, 1 << 20), queue: (2, 1 << 20) });
    }
}

#[test]
fn snapshot_plus_events_is_the_later_snapshot() {
    let store = store();
    let (hub, _) = hub(&store, Limits::default());
    let (at, mut state) = shown(&hub);
    let mut stream = hub.watch(&WatchEngineSettings { since: Some(at) });
    for setting in [
        EngineSetting::IdleAgents(5),
        EngineSetting::ClaudeHooks(false),
        EngineSetting::IdleAgents(1),
    ] {
        block_on(hub.set(Some(OpId::new()), SetEngineSetting { setting })).unwrap();
    }
    while let Some(Some(item)) = stream.next().now_or_never() {
        match item {
            Item::Events { events, .. } => {
                for event in &events {
                    SettingsFeed::apply(&mut state, event);
                }
            }
            other => panic!("a watcher whose cursor the ring holds gets events, not {other:?}"),
        }
    }
    assert_eq!(state, shown(&hub).1);
    assert_eq!(state.idle_agents, 1);
    assert!(!state.claude_hooks);
}

/// The store a monocode build left: its whole layout and preferences under `workspace`, and the
/// app's markers beside them.
fn monocode_store() -> Database {
    let store = store();
    let workspace = serde_json::json!({
        "sidebarWidth": 312,
        "preferences": {
            "theme": "dark",
            "idle_agents": 5,
            "claude_hooks": false,
            "keep_awake": true,
            "binary_paths": { "claude": "/opt/claude" },
            "hidden_providers": ["pi"],
            "background_model": { "provider": "codex", "model": "gpt-5.6-luna" },
            "provider_efforts": { "codex": "high" },
            // The wrong shape: it keeps its default, and the others still move.
            "default_models": ["not", "a", "map"],
            "sound": true,
        },
    });
    {
        let store = store.lock().unwrap();
        store
            .save_setting(LEGACY_WORKSPACE, &workspace.to_string())
            .unwrap();
        store
            .save_setting("native-inbox-state", "{\"seen\":[1]}")
            .unwrap();
        store
            .save_setting("onicode.updateReportSeen", "0.4.1")
            .unwrap();
    }
    store
}

#[test]
fn monocode_preferences_move_once() {
    let store = monocode_store();
    let (hub, effects) = hub(&store, Limits::default());
    let moved = hub.current();
    assert_eq!(moved.idle_agents, 5);
    assert!(!moved.claude_hooks);
    assert!(moved.keep_awake);
    assert_eq!(
        moved.binary_paths.get("claude").map(String::as_str),
        Some("/opt/claude")
    );
    assert_eq!(moved.hidden_providers, vec!["pi".to_owned()]);
    assert_eq!(
        moved.background_model,
        Some(ModelChoice {
            provider: "codex".into(),
            model: Some("gpt-5.6-luna".into()),
        })
    );
    assert_eq!(
        moved.provider_efforts.get("codex").map(String::as_str),
        Some("high")
    );
    assert!(
        moved.default_models.is_empty(),
        "a malformed value keeps its default"
    );
    assert_eq!(
        effects.last.lock().unwrap().as_ref(),
        Some(&moved),
        "the moved settings take effect at once"
    );
    assert!(store.lock().unwrap().setting(KEY).unwrap().is_some());

    // A later launch reads its own key: the saved preferences no longer count, and a change
    // made since stays.
    block_on(hub.set(
        None,
        SetEngineSetting {
            setting: EngineSetting::IdleAgents(2),
        },
    ))
    .unwrap();
    let (again, _) = hub_again(&store);
    assert_eq!(again.current().idle_agents, 2);
    assert_eq!(again.current(), hub.current());
}

#[test]
fn an_empty_store_starts_from_the_defaults() {
    let store = store();
    let (hub, _) = hub(&store, Limits::default());
    assert_eq!(hub.current(), EngineSettings::default());
    assert!(store.lock().unwrap().setting(KEY).unwrap().is_some());
}

#[test]
fn the_viewer_state_is_handed_over_once() {
    let store = monocode_store();
    let (hub, _) = hub(&store, Limits::default());
    let op = OpId::new();
    let taken = block_on(hub.take_viewer_state(Some(op))).unwrap();
    assert!(taken.values.contains_key(LEGACY_WORKSPACE));
    assert_eq!(
        taken
            .values
            .get("onicode.updateReportSeen")
            .map(String::as_str),
        Some("0.4.1")
    );
    assert_eq!(taken.values.len(), 3, "only the keys the store held");
    for key in VIEWER_KEYS {
        assert_eq!(
            store.lock().unwrap().setting(key).unwrap(),
            None,
            "{key} is forgotten"
        );
    }
    // The settings moved before the hand-over and stay.
    assert_eq!(hub.current().idle_agents, 5);
    // A repeat of the op gets the same values; a new take finds nothing.
    assert_eq!(block_on(hub.take_viewer_state(Some(op))).unwrap(), taken);
    assert!(
        block_on(hub.take_viewer_state(Some(OpId::new())))
            .unwrap()
            .values
            .is_empty()
    );
}
