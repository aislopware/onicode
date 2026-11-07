//! The Agents group against its reference (engine.md "Tests"): accounts added, renamed, removed
//! and selected, some under ops sent twice, their sign-ins and usage read through a scripted
//! probe, while viewers join, fall behind, drop and come back at cursors they held. Every viewer
//! must fold to the model's accounts, and a repeated op must get its first reply and change
//! nothing again. Then what `claude auth status --json` and Codex's `account/read` print, read as
//! sign-ins, and the probe's other answers. No provider CLI runs.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use onicode_core::accounts::{Account, AuthAction, Book, DEFAULT, Identity, Profile};
use onicode_core::catalog::Model;
use onicode_core::mcp::McpConnection;
use onicode_core::provider::{Kind, NativeCommand};
use onicode_core::session_title::TextModel;
use onicode_core::skills::DiscoveredSkill;
use onicode_core::store::Store;
use onicode_core::usage::Snapshot;
use onicode_protocol::agents::{
    AccountsEvent, AccountsState, AddAccount, AddMcpServer, CancelLogin, CatalogRead, Generate,
    HarnessUpdate, HarnessUpdates, HarnessVersion, Login, RefreshSignIn, RefreshUsage,
    RemoveAccount, RenameAccount, SelectAccount, SignIn, SignInEntry, UsageEntry, UsageKey,
    UsageRead, WatchAccounts,
};
use onicode_protocol::engine_settings::{EngineSettings, ModelChoice};
use onicode_protocol::{Cursor, Item, OpId, Outcome, Rejection};
use proptest::prelude::*;
use serde_json::json;

use super::{AccountsFeed, AccountsHub, AgentProbe, parse_claude_status, parse_codex_account};
use crate::feed::{Feed, Limits};
use crate::session::ThreadSpawn;
use crate::settings::{Effects, SettingsHub};
use crate::turn::Database;

type Stream = BoxStream<'static, Item<AccountsState, AccountsEvent>>;

const OFFLINE: &str = "scripted";

/// The provider doors as the script answers them, counting what each was asked.
#[derive(Default)]
struct Scripted {
    sign_ins: AtomicUsize,
    polls: AtomicUsize,
    logins: AtomicUsize,
    removed: Mutex<Vec<(Kind, String)>>,
    /// The CLIs installed.
    installed: Mutex<BTreeSet<&'static str>>,
    generated: Mutex<Vec<TextModel>>,
}

/// Who the script says an account is: a function of the account, so the model knows it.
fn identity(kind: Kind, account: &str) -> SignIn {
    SignIn {
        signed_in: Some(true),
        identity: Identity {
            email: Some(format!("{account}@{}", kind.id())),
            ..Identity::default()
        },
        method: None,
    }
}

/// The usage the script reports for an account, likewise.
fn usage(kind: Kind, account: &str) -> UsageRead {
    if kind == Kind::Claude {
        return UsageRead::Unreported;
    }
    UsageRead::Snapshot(Snapshot {
        updated_at: i64::try_from(account.len()).unwrap(),
        ..Snapshot::default()
    })
}

impl AgentProbe for Scripted {
    fn sign_in(&self, kind: Kind, profile: &Profile) -> SignIn {
        self.sign_ins.fetch_add(1, Ordering::Relaxed);
        identity(kind, &profile.id)
    }

    fn usage(&self, kind: Kind, profile: &Profile, _: &str) -> UsageRead {
        self.polls.fetch_add(1, Ordering::Relaxed);
        usage(kind, &profile.id)
    }

    fn authenticate(
        &self,
        _: Kind,
        _: &Profile,
        _: AuthAction,
        cancel: Arc<AtomicBool>,
    ) -> Result<(), String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("Sign-in cancelled".into());
        }
        self.logins.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn remove(&self, kind: Kind, profile: &Profile) -> Result<(), String> {
        self.removed
            .lock()
            .unwrap()
            .push((kind, profile.id.clone()));
        Ok(())
    }

    fn catalog(&self, _: Kind, _: &Profile, _: CatalogRead) -> Result<Option<Vec<Model>>, String> {
        Ok(None)
    }

    fn skills(
        &self,
        _: &str,
        disabled: Option<Vec<String>>,
    ) -> Result<Vec<DiscoveredSkill>, String> {
        Ok(disabled
            .unwrap_or_default()
            .into_iter()
            .map(|path| DiscoveredSkill {
                name: "disabled".into(),
                description: String::new(),
                path,
                scope: "user".into(),
                source: "claude".into(),
            })
            .collect())
    }

    fn create_skill(&self, _: &str, _: &str, _: bool) -> Result<String, String> {
        Err(OFFLINE.into())
    }

    fn commands(
        &self,
        _: Kind,
        _: &str,
        _: &Profile,
        _: Option<String>,
    ) -> Result<Vec<NativeCommand>, String> {
        Ok(Vec::new())
    }

    fn mcp_servers(&self, _: &str) -> Result<Vec<McpConnection>, String> {
        Ok(Vec::new())
    }

    fn add_mcp_server(&self, _: AddMcpServer) -> Result<(), String> {
        Err(OFFLINE.into())
    }

    fn remove_mcp_server(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err(OFFLINE.into())
    }

    fn mcp_login(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err(OFFLINE.into())
    }

    fn mcp_health(&self, _: &str) -> Result<String, String> {
        Err(OFFLINE.into())
    }

    fn harness(&self, kind: Kind) -> Option<String> {
        self.installed
            .lock()
            .unwrap()
            .contains(kind.id())
            .then(|| format!("/bin/{}", kind.id()))
    }

    fn inspect(&self, _: Kind, _: Option<&str>) -> Result<HarnessVersion, String> {
        Err(OFFLINE.into())
    }

    fn harness_updates(&self, _: &[String]) -> Vec<HarnessUpdate> {
        vec![HarnessUpdate {
            harness: Kind::Codex,
            installed: "1.0.0".into(),
            latest: "1.1.0".into(),
        }]
    }

    fn run_harness_update(&self, _: &HarnessUpdate) -> Result<String, String> {
        Err(OFFLINE.into())
    }

    fn generate(
        &self,
        model: &TextModel,
        _: &str,
        prompt: String,
        _: Arc<AtomicBool>,
    ) -> Result<String, String> {
        self.generated.lock().unwrap().push(model.clone());
        Ok(prompt)
    }

    fn consume_reset(&self, _: &Profile, _: Option<&str>) -> Result<String, String> {
        Ok("reset".into())
    }

    fn keep_idle_agents(&self, _: Vec<i64>) {}

    fn transfer_thread(&self, _: Kind, _: &Profile, _: &Profile, _: &str) -> Result<(), String> {
        Err(OFFLINE.into())
    }

    fn forget_thread(&self, _: Kind, _: &str, _: &str, _: &str) {}
}

struct Quiet;

impl Effects for Quiet {
    fn applied(&self, _: &EngineSettings) {}
}

struct Harness {
    probe: Arc<Scripted>,
    settings: Arc<SettingsHub>,
    hub: Arc<AccountsHub>,
}

impl Harness {
    fn new(limits: Limits) -> Self {
        let store: Database = Arc::new(Mutex::new(Store::in_memory().unwrap()));
        let settings =
            SettingsHub::with(store.clone(), Arc::new(Quiet), Limits::default()).unwrap();
        let probe = Arc::new(Scripted::default());
        let hub = AccountsHub::with(
            store,
            settings.clone(),
            probe.clone(),
            Arc::new(ThreadSpawn),
            limits,
        );
        Self {
            probe,
            settings,
            hub,
        }
    }

    fn watch(&self, since: Option<Cursor>) -> Stream {
        self.hub.watch(&WatchAccounts { since })
    }

    fn shown(&self) -> (Cursor, AccountsState) {
        match self.watch(None).next().now_or_never() {
            Some(Some(Item::Snapshot { at, state })) => (at, state),
            other => panic!("a new watcher starts from a snapshot, not {other:?}"),
        }
    }
}

struct Viewer {
    stream: Option<Stream>,
    shown: Option<(Cursor, AccountsState)>,
    held: Vec<(Cursor, AccountsState)>,
}

impl Viewer {
    fn join(harness: &Harness) -> Self {
        Self {
            stream: Some(harness.watch(None)),
            shown: None,
            held: Vec::new(),
        }
    }

    fn pump(&mut self, harness: &Harness) {
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
                    for event in &events {
                        AccountsFeed::apply(state, event);
                    }
                    cursor.seq = last;
                }
                Item::Resync => {
                    let since = self.shown.as_ref().map(|(cursor, _)| *cursor);
                    self.stream = Some(harness.watch(since));
                }
            }
            if let Some(shown) = &self.shown {
                self.held.push(shown.clone());
            }
        }
    }

    fn rejoin(&mut self, harness: &Harness, pick: usize) {
        let back = (!self.held.is_empty()).then(|| self.held[pick % self.held.len()].clone());
        self.stream = None;
        let since = back.as_ref().map(|(cursor, _)| *cursor);
        self.shown = back;
        self.stream = Some(harness.watch(since));
    }
}

const KINDS: [Kind; 2] = [Kind::Claude, Kind::Codex];

#[derive(Clone, Debug)]
enum Step {
    Add {
        kind: usize,
        label: String,
        op: Option<u8>,
    },
    Rename {
        account: usize,
        label: String,
        op: Option<u8>,
    },
    Remove {
        account: usize,
        op: Option<u8>,
    },
    Select {
        account: usize,
        project: u8,
        op: Option<u8>,
    },
    SignIn {
        account: usize,
    },
    Usage {
        account: usize,
        force: bool,
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

fn op() -> impl Strategy<Value = Option<u8>> {
    prop::option::of(0..6_u8)
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        3 => (0..2_usize, "[a-z ]{0,6}", op())
            .prop_map(|(kind, label, op)| Step::Add { kind, label, op }),
        1 => (any::<usize>(), "[a-z]{0,4}", op())
            .prop_map(|(account, label, op)| Step::Rename { account, label, op }),
        1 => (any::<usize>(), op()).prop_map(|(account, op)| Step::Remove { account, op }),
        1 => (any::<usize>(), 0..3_u8, op())
            .prop_map(|(account, project, op)| Step::Select { account, project, op }),
        2 => any::<usize>().prop_map(|account| Step::SignIn { account }),
        2 => (any::<usize>(), any::<bool>())
            .prop_map(|(account, force)| Step::Usage { account, force }),
        2 => Just(Step::Join),
        4 => any::<usize>().prop_map(|viewer| Step::Pump { viewer }),
        1 => any::<usize>().prop_map(|viewer| Step::Drop { viewer }),
        2 => (any::<usize>(), any::<usize>())
            .prop_map(|(viewer, pick)| Step::Rejoin { viewer, pick }),
    ]
}

/// The accounts a step can name: every provider's default, and every account in the book.
fn named(book: &Book, pick: usize) -> (Kind, String) {
    let mut all: Vec<(Kind, String)> = KINDS.iter().map(|kind| (*kind, DEFAULT.into())).collect();
    for account in &book.accounts {
        all.push((Kind::from_id(&account.provider), account.id.clone()));
    }
    all[pick % all.len()].clone()
}

/// The op a step names: a number already used by the same call repeats its op, as a viewer
/// retrying that call does. Another call never reuses an op.
fn op_id(
    ops: &mut BTreeMap<(&'static str, u8), OpId>,
    call: &'static str,
    op: Option<u8>,
) -> (Option<OpId>, bool) {
    let repeat = op.is_some_and(|op| ops.contains_key(&(call, op)));
    (op.map(|op| *ops.entry((call, op)).or_default()), repeat)
}

fn run(steps: &[Step], limits: Limits) {
    let harness = Harness::new(limits);
    let hub = &harness.hub;
    let mut model = AccountsState::default();
    let mut ops = BTreeMap::new();
    // Each op's first reply, which a repeat must get again.
    let mut replies: BTreeMap<OpId, serde_json::Value> = BTreeMap::new();
    let mut viewers: Vec<Viewer> = Vec::new();
    for step in steps {
        match step {
            Step::Add { kind, label, op } => {
                let (op, repeat) = op_id(&mut ops, "AddAccount", *op);
                let body = AddAccount {
                    provider: KINDS[*kind],
                    label: label.clone(),
                };
                let reply = block_on(hub.add_account(op, body.clone()));
                let first = op.and_then(|op| replies.get(&op).cloned());
                if !repeat {
                    let mut book = model.book.clone();
                    let expected = book.add(body.provider, &body.label);
                    match (&reply, expected) {
                        (Ok(account), Ok(_)) => {
                            model.book.accounts.push(account.clone());
                        }
                        (Err(_), Err(_)) => {}
                        (reply, expected) => panic!("{reply:?} where the book says {expected:?}"),
                    }
                    if let (Some(op), Ok(account)) = (op, &reply) {
                        replies.insert(op, serde_json::to_value(account).unwrap());
                    }
                } else if let (Some(first), Ok(account)) = (first, &reply) {
                    assert_eq!(
                        serde_json::to_value(account).unwrap(),
                        first,
                        "a repeat's reply"
                    );
                }
            }
            Step::Rename { account, label, op } => {
                let (kind, id) = named(&model.book, *account);
                let (op, repeat) = op_id(&mut ops, "RenameAccount", *op);
                let body = RenameAccount {
                    provider: kind,
                    account: id.clone(),
                    label: label.clone(),
                };
                let reply = block_on(hub.rename_account(op, body)).unwrap();
                if !repeat {
                    let expected = match model.book.rename(kind, &id, label) {
                        Ok(()) => Outcome::Applied,
                        Err(error) => Outcome::Rejected(Rejection::Denied(error)),
                    };
                    assert_eq!(reply, expected);
                }
            }
            Step::Remove { account, op } => {
                let (kind, id) = named(&model.book, *account);
                let (op, repeat) = op_id(&mut ops, "RemoveAccount", *op);
                let body = RemoveAccount {
                    provider: kind,
                    account: id.clone(),
                };
                let reply = block_on(hub.remove_account(op, body)).unwrap();
                if !repeat {
                    let expected = if id == DEFAULT {
                        Outcome::Rejected(Rejection::Denied(
                            "This account cannot be removed".into(),
                        ))
                    } else if model.book.remove(kind, &id).is_ok() {
                        Outcome::Applied
                    } else {
                        Outcome::Rejected(Rejection::Gone)
                    };
                    assert_eq!(reply, expected);
                }
            }
            Step::Select {
                account,
                project,
                op,
            } => {
                let (kind, id) = named(&model.book, *account);
                let (op, repeat) = op_id(&mut ops, "SelectAccount", *op);
                let project = format!("/p{project}");
                let body = SelectAccount {
                    provider: kind,
                    project: project.clone(),
                    account: id.clone(),
                };
                let reply = block_on(hub.select_account(op, body)).unwrap();
                if !repeat {
                    let expected = match model.book.select(kind, &project, &id) {
                        Ok(()) => Outcome::Applied,
                        Err(_) => Outcome::Rejected(Rejection::Gone),
                    };
                    assert_eq!(reply, expected);
                }
            }
            Step::SignIn { account } => {
                let (kind, id) = named(&model.book, *account);
                let asked = harness.probe.sign_ins.load(Ordering::Relaxed);
                let reply = block_on(hub.refresh_sign_in(RefreshSignIn {
                    provider: kind,
                    account: id.clone(),
                }))
                .unwrap();
                assert_eq!(
                    reply,
                    Outcome::Applied,
                    "the steps name known accounts only"
                );
                assert_eq!(harness.probe.sign_ins.load(Ordering::Relaxed), asked + 1);
                model.apply(&AccountsEvent::SignIn(SignInEntry {
                    provider: kind,
                    account: id.clone(),
                    sign_in: identity(kind, &id),
                }));
            }
            Step::Usage { account, force } => {
                let (kind, id) = named(&model.book, *account);
                let key = UsageKey {
                    provider: kind,
                    account: id.clone(),
                    billing: String::new(),
                };
                let polls = harness.probe.polls.load(Ordering::Relaxed);
                let reply = block_on(hub.refresh_usage(RefreshUsage {
                    key: key.clone(),
                    force: *force,
                }))
                .unwrap();
                assert_eq!(reply, Outcome::Applied);
                let fresh = model.usage(&key).is_some() && kind == Kind::Codex;
                let polled = harness.probe.polls.load(Ordering::Relaxed) - polls;
                // A poll read within the minute is not read again unless forced; what turns
                // reported is read every time.
                assert_eq!(polled, usize::from(*force || !fresh));
                model.apply(&AccountsEvent::Usage(UsageEntry {
                    key,
                    usage: usage(kind, &id),
                }));
            }
            Step::Join => {
                if viewers.len() < 8 {
                    viewers.push(Viewer::join(&harness));
                }
            }
            Step::Pump { viewer } => {
                if !viewers.is_empty() {
                    let count = viewers.len();
                    let viewer = &mut viewers[viewer % count];
                    viewer.pump(&harness);
                    let (_, folded) = viewer.shown.as_ref().expect("a pumped viewer shows");
                    assert_eq!(folded, &model, "a viewer folds to the model");
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
                    viewers[viewer % count].rejoin(&harness, *pick);
                }
            }
        }
        assert_eq!(hub.state(), model, "the hub holds the model after {step:?}");
    }
    let (cursor, now) = harness.shown();
    for viewer in &mut viewers {
        viewer.pump(&harness);
        let (held, folded) = viewer.shown.as_ref().expect("a pumped viewer shows");
        assert_eq!(folded, &now, "every viewer ends up showing the hub");
        assert_eq!(held, &cursor, "every viewer ends up at the hub's cursor");
    }
    let removed = harness.probe.removed.lock().unwrap().len();
    let left = steps
        .iter()
        .filter(|step| matches!(step, Step::Add { .. }))
        .count();
    assert!(removed <= left, "an account is removed once at most");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn viewers_fold_to_the_accounts(steps in prop::collection::vec(step(), 0..60)) {
        run(&steps, Limits::default());
    }

    #[test]
    fn slow_viewers_merge_resync_and_rejoin(steps in prop::collection::vec(step(), 0..60)) {
        run(&steps, Limits { ring: (4, 1 << 20), queue: (2, 1 << 20) });
    }
}

#[test]
fn a_repeated_add_adds_once() {
    let harness = Harness::new(Limits::default());
    let op = OpId::new();
    let add = || AddAccount {
        provider: Kind::Codex,
        label: "Work".into(),
    };
    let first: Account = block_on(harness.hub.add_account(Some(op), add())).unwrap();
    let again: Account = block_on(harness.hub.add_account(Some(op), add())).unwrap();
    assert_eq!(first, again);
    assert_eq!(harness.hub.state().book.accounts, vec![first]);
}

#[test]
fn removing_an_account_signs_it_out_first() {
    let harness = Harness::new(Limits::default());
    let account = block_on(harness.hub.add_account(
        None,
        AddAccount {
            provider: Kind::Claude,
            label: "Work".into(),
        },
    ))
    .unwrap();
    let op = OpId::new();
    let remove = || RemoveAccount {
        provider: Kind::Claude,
        account: account.id.clone(),
    };
    assert_eq!(
        block_on(harness.hub.remove_account(Some(op), remove())),
        Ok(Outcome::Applied)
    );
    assert_eq!(
        block_on(harness.hub.remove_account(Some(op), remove())),
        Ok(Outcome::Applied)
    );
    assert_eq!(
        *harness.probe.removed.lock().unwrap(),
        vec![(Kind::Claude, account.id)],
        "a repeat removes nothing again"
    );
    assert!(harness.hub.state().book.accounts.is_empty());
}

#[test]
fn a_cancel_that_comes_first_stops_the_sign_in() {
    let harness = Harness::new(Limits::default());
    let attempt = "attempt-1".to_owned();
    harness.hub.cancel_login(&CancelLogin {
        attempt: attempt.clone(),
    });
    let login = Login {
        provider: Kind::Claude,
        account: DEFAULT.into(),
        attempt,
    };
    assert_eq!(
        block_on(harness.hub.login(Some(OpId::new()), login)),
        Err("Sign-in cancelled".into())
    );
    assert_eq!(harness.probe.logins.load(Ordering::Relaxed), 0);
    // A later attempt signs in, and who it is arrives on the watch.
    let login = Login {
        provider: Kind::Claude,
        account: DEFAULT.into(),
        attempt: "attempt-2".into(),
    };
    assert_eq!(
        block_on(harness.hub.login(None, login)),
        Ok(Outcome::Applied)
    );
    assert_eq!(harness.probe.logins.load(Ordering::Relaxed), 1);
    let state = harness.hub.state();
    assert_eq!(
        state.sign_in(Kind::Claude, DEFAULT),
        Some(&identity(Kind::Claude, DEFAULT))
    );
    let key = UsageKey {
        provider: Kind::Claude,
        account: DEFAULT.into(),
        billing: String::new(),
    };
    assert_eq!(state.usage(&key), Some(&UsageRead::Unreported));
}

#[test]
fn an_unknown_account_is_gone() {
    let harness = Harness::new(Limits::default());
    let read = RefreshSignIn {
        provider: Kind::Codex,
        account: "account-missing".into(),
    };
    assert_eq!(
        block_on(harness.hub.refresh_sign_in(read)),
        Ok(Outcome::Rejected(Rejection::Gone))
    );
    assert_eq!(harness.probe.sign_ins.load(Ordering::Relaxed), 0);
}

#[test]
fn the_launch_check_answers_once() {
    let harness = Harness::new(Limits::default());
    let check = |launch| block_on(harness.hub.harness_updates(HarnessUpdates { launch }));
    assert_eq!(check(true).unwrap().len(), 1);
    assert!(check(true).unwrap().is_empty(), "one launch notices once");
    assert_eq!(
        check(false).unwrap().len(),
        1,
        "a manual check always reads"
    );
}

#[test]
fn text_runs_on_the_first_installed_choice() {
    let harness = Harness::new(Limits::default());
    let generate = |model: Option<TextModel>, fallback| {
        block_on(harness.hub.generate(Generate {
            request: uuid::Uuid::new_v4().to_string(),
            cwd: "/w".into(),
            prompt: "say hi".into(),
            model,
            fallback,
        }))
    };
    let pi = TextModel {
        kind: Kind::Pi,
        model: Some("pi-model".into()),
        account: Profile::default(),
    };
    assert!(
        generate(Some(pi.clone()), None).is_err(),
        "nothing installed"
    );
    harness.probe.installed.lock().unwrap().insert("codex");
    harness.probe.installed.lock().unwrap().insert("opencode");
    assert_eq!(
        generate(Some(pi.clone()), Some(Kind::OpenCode)),
        Ok("say hi".into())
    );
    assert_eq!(generate(Some(pi.clone()), None), Ok("say hi".into()));
    harness.probe.installed.lock().unwrap().insert("pi");
    assert_eq!(
        generate(Some(pi.clone()), Some(Kind::OpenCode)),
        Ok("say hi".into())
    );
    // With no model asked for, the background model engine settings name.
    block_on(harness.settings.set(
        None,
        onicode_protocol::engine_settings::SetEngineSetting {
            setting: onicode_protocol::engine_settings::EngineSetting::BackgroundModel(Some(
                ModelChoice {
                    provider: "codex".into(),
                    model: Some("gpt-5.6-luna".into()),
                },
            )),
        },
    ))
    .unwrap();
    assert_eq!(generate(None, None), Ok("say hi".into()));
    let ran: Vec<(Kind, Option<String>)> = harness
        .probe
        .generated
        .lock()
        .unwrap()
        .iter()
        .map(|model| (model.kind, model.model.clone()))
        .collect();
    assert_eq!(
        ran,
        vec![
            (Kind::OpenCode, None),
            (Kind::Codex, None),
            (Kind::Pi, Some("pi-model".into())),
            (Kind::Codex, Some("gpt-5.6-luna".into())),
        ]
    );
}

#[test]
fn skills_leave_out_what_settings_turned_off_unless_all() {
    let harness = Harness::new(Limits::default());
    block_on(harness.settings.set(
        None,
        onicode_protocol::engine_settings::SetEngineSetting {
            setting: onicode_protocol::engine_settings::EngineSetting::DisabledSkillPaths(vec![
                "/s/off".into(),
            ]),
        },
    ))
    .unwrap();
    let skills = |all| {
        block_on(harness.hub.skills(onicode_protocol::agents::Skills {
            project: "/w".into(),
            all,
        }))
        .unwrap()
    };
    // The script lists the paths it was told to leave out: none when `all`.
    assert_eq!(skills(false).len(), 1);
    assert!(skills(true).is_empty());
}

#[test]
fn claude_auth_status_reads_as_a_sign_in() {
    // Signed in to a Claude account.
    let signed_in = parse_claude_status(
        &json!({
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": "ada@example.com",
            "orgName": "Acme",
            "subscriptionType": "max",
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(signed_in.signed_in, Some(true));
    assert_eq!(signed_in.identity.email.as_deref(), Some("ada@example.com"));
    assert_eq!(signed_in.identity.organization.as_deref(), Some("Acme"));
    assert_eq!(signed_in.identity.plan.as_deref(), Some("Max"));
    assert_eq!(signed_in.method.as_deref(), Some("Claude account"));

    // A `claude setup-token` token: no subscription type, and a subscription all the same
    // (claude-code-acp #1376). A line of noise before the JSON is skipped.
    let token = parse_claude_status(
        "Checking…\n{\"loggedIn\":true,\"authMethod\":\"oauth_token\",\"apiProvider\":\"firstParty\"}",
    )
    .unwrap();
    assert_eq!(token.signed_in, Some(true));
    assert_eq!(token.identity.plan.as_deref(), Some("Claude subscription"));
    assert_eq!(token.method.as_deref(), Some("OAuth token"));

    // The same token through a cloud provider is no Claude subscription.
    let cloud = parse_claude_status(
        r#"{"loggedIn":true,"authMethod":"oauth_token","apiProvider":"bedrock"}"#,
    )
    .unwrap();
    assert_eq!(cloud.signed_in, Some(true));
    assert_eq!(cloud.identity.plan, None);

    // Signed out, whatever the exit status said.
    let out = parse_claude_status(r#"{"loggedIn":false}"#).unwrap();
    assert_eq!(out.signed_in, Some(false));
    assert_eq!(out.identity, Identity::default());

    // Malformed: no JSON, no `loggedIn`, or the wrong type.
    for printed in [
        "",
        "Not logged in",
        "{\"email\":\"ada@example.com\"}",
        "{\"loggedIn\":\"yes\"}",
        "{\"loggedIn\":true",
    ] {
        assert!(
            parse_claude_status(printed).is_err(),
            "{printed:?} is malformed"
        );
    }
}

#[test]
fn codex_account_reads_as_a_sign_in() {
    let chatgpt = parse_codex_account(&json!({
        "account": { "type": "chatgpt", "email": "ada@example.com", "planType": "plus" },
        "requiresOpenaiAuth": true,
    }))
    .unwrap();
    assert_eq!(chatgpt.signed_in, Some(true));
    assert_eq!(chatgpt.identity.email.as_deref(), Some("ada@example.com"));
    assert_eq!(chatgpt.identity.plan.as_deref(), Some("Plus"));
    let key = parse_codex_account(&json!({ "account": { "type": "apiKey" } })).unwrap();
    assert_eq!(key.method.as_deref(), Some("API key"));
    let out = parse_codex_account(&json!({ "account": null, "requiresOpenaiAuth": true })).unwrap();
    assert_eq!(out.signed_in, Some(false));
    assert!(parse_codex_account(&json!({})).is_err());
    assert!(parse_codex_account(&json!({ "account": { "type": "other" } })).is_err());
}
