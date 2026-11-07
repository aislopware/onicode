//! Alerts, against their rules (alerts.md): the pure [`Alerter`] under seeded interleavings in
//! virtual time (a flicker never rings, a cause rings once after its hold and only while it
//! stands, seeding and replay never ring, the key on screen never rings, terminals are
//! rate-limited), then the hub from scripted workspace, session and terminal news to the
//! `WatchAlerts` items a viewer folds. Nothing here posts a real notification.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use onicode_protocol::agent::{Approval, Question, QuestionItem, QuestionOption};
use onicode_protocol::alerts::{
    self, Alert, AlertAction, AlertKind, AlertsEvent, AlertsState, MuteRule, Mutes, Need, OnScreen,
    ProjectMute, SetViewportFocus, Subject, WatchAlerts,
};
use onicode_protocol::session::{SessionEvent, SessionState, TurnState};
use onicode_protocol::sessions::Session;
use onicode_protocol::terminal::{ProgramState, ProgramStatus, TerminalSummary};
use onicode_protocol::workspace::{SessionSummary, WorkspaceEvent, WorkspaceState};
use onicode_protocol::{Item, SessionId, TerminalId};
use proptest::prelude::*;

use super::alerter::{
    Alerter, Change, EXTERNAL_BURST, EXTERNAL_GAP_MS, EXTERNAL_WINDOW_MS, Fact, HOLD_MS, Key,
    Phase, STALL_MS, stands,
};
use super::banners::Banners;
use super::hub::AlertsHub;
use crate::feed::Limits;

const T0: u64 = 1_800_000_000_000;

fn fact(phase: Phase, need: Option<&str>, ended: u64, unseen: bool, active_at: u64) -> Fact {
    Fact {
        phase,
        need: need.map(str::to_owned),
        ended,
        unseen,
        active_at,
        quiet: false,
    }
}

/// What a presenter shows, folded from the changes.
fn fold(shown: &mut BTreeMap<Key, AlertKind>, changes: &[Change]) {
    for change in changes {
        match *change {
            Change::Raise { key, kind } => {
                shown.insert(key, kind);
            }
            Change::Withdraw { key } => {
                shown.remove(&key);
            }
        }
    }
}

fn raises(changes: &[Change]) -> Vec<(Key, AlertKind)> {
    changes
        .iter()
        .filter_map(|change| match *change {
            Change::Raise { key, kind } => Some((key, kind)),
            Change::Withdraw { .. } => None,
        })
        .collect()
}

#[test]
fn a_question_rings_after_its_hold_and_goes_once_answered() {
    let key = Key::Session(1);
    let mut alerter = Alerter::default();
    alerter.seed(key, Fact::idle(T0));
    alerter.observe(key, fact(Phase::Working, None, 0, false, T0), T0);
    alerter.observe(
        key,
        fact(Phase::NeedsYou, Some("q1"), 0, false, T0 + 10),
        T0 + 10,
    );
    assert!(alerter.step(T0 + 10 + HOLD_MS - 1).is_empty());
    assert_eq!(
        alerter.step(T0 + 10 + HOLD_MS),
        vec![Change::Raise {
            key,
            kind: AlertKind::NeedsYou
        }]
    );
    alerter.observe(
        key,
        fact(Phase::Working, None, 0, false, T0 + 3_000),
        T0 + 3_000,
    );
    assert_eq!(alerter.step(T0 + 3_000), vec![Change::Withdraw { key }]);
}

#[test]
fn a_silent_turn_stalls_once() {
    let key = Key::Session(1);
    let mut alerter = Alerter::default();
    alerter.seed(key, Fact::idle(T0));
    alerter.observe(key, fact(Phase::Working, None, 0, false, T0), T0);
    assert_eq!(alerter.next_deadline(), Some(T0 + STALL_MS));
    assert!(alerter.step(T0 + STALL_MS).is_empty());
    assert_eq!(
        alerter.step(T0 + STALL_MS + HOLD_MS),
        vec![Change::Raise {
            key,
            kind: AlertKind::Stalled
        }]
    );
    // Looking takes it back, and the same silence never stalls again.
    alerter.look(BTreeSet::from([key]));
    assert_eq!(
        alerter.step(T0 + STALL_MS + HOLD_MS + 1),
        vec![Change::Withdraw { key }]
    );
    alerter.look(BTreeSet::new());
    assert!(alerter.step(T0 + 2 * STALL_MS).is_empty());
    // Any news ends the silence; a new one stalls again.
    let later = T0 + 2 * STALL_MS;
    alerter.observe(key, fact(Phase::Working, None, 0, false, later), later);
    assert!(alerter.step(later + STALL_MS).is_empty());
    assert_eq!(
        raises(&alerter.step(later + STALL_MS + HOLD_MS)),
        vec![(key, AlertKind::Stalled)]
    );
}

/// One step of a seeded run: time moves by `dt`, then `key` takes `fact` (or nothing), and the
/// alerter steps.
#[derive(Clone, Debug)]
struct Beat {
    dt: u64,
    key: u8,
    fact: Option<(u8, u8, bool, bool)>,
    look: Option<Option<u8>>,
}

fn beat() -> impl Strategy<Value = Beat> {
    (
        0u64..3_000,
        0u8..3,
        proptest::option::of((0u8..5, 0u8..3, any::<bool>(), any::<bool>())),
        proptest::option::weighted(0.1, proptest::option::of(0u8..3)),
    )
        .prop_map(|(dt, key, fact, look)| Beat {
            dt,
            key,
            fact,
            look,
        })
}

fn phase(code: u8) -> Phase {
    match code {
        0 => Phase::Idle,
        1 => Phase::Working,
        2 => Phase::NeedsYou,
        3 => Phase::Failed,
        _ => Phase::Done,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Every raise was held: the cause it names stood at every look from its raising transition,
    /// at least the hold before, up to the raise; what shows is what the alerter holds live; a
    /// live alert's cause stands and its key is not on screen; the key on screen never rings.
    #[test]
    fn alerts_follow_their_causes(beats in proptest::collection::vec(beat(), 1..80)) {
        let mut alerter = Alerter::default();
        let mut shown = BTreeMap::new();
        let mut history: BTreeMap<Key, Vec<(u64, Fact)>> = BTreeMap::new();
        let mut looking: Option<Key> = None;
        let mut now = T0;
        let mut ended = [0u64; 3];
        let mut unseen = [false; 3];
        for key in 0..3 {
            alerter.seed(Key::Session(key), Fact::idle(now));
            history.insert(Key::Session(key), vec![(now, Fact::idle(now))]);
        }
        for beat in beats {
            now += beat.dt;
            if let Some(look) = beat.look {
                looking = look.map(|key| Key::Session(i64::from(key)));
                alerter.look(looking.into_iter().collect());
            }
            let key = Key::Session(i64::from(beat.key));
            if let Some((code, need, seen, bump)) = beat.fact {
                // A run's end makes it unseen; only looking makes it seen again.
                let slot = usize::from(beat.key);
                if bump {
                    ended[slot] += 1;
                    unseen[slot] = true;
                } else if seen {
                    unseen[slot] = false;
                }
                let need = (code == 2).then(|| format!("need-{need}"));
                let next = Fact {
                    phase: phase(code),
                    need,
                    ended: ended[slot],
                    unseen: unseen[slot],
                    active_at: now,
                    quiet: false,
                };
                alerter.observe(key, next.clone(), now);
                history.entry(key).or_default().push((now, next));
            }
            let changes = alerter.step(now);
            for (raised, kind) in raises(&changes) {
                prop_assert_ne!(Some(raised), looking, "the key on screen rang");
                if kind == AlertKind::Stalled {
                    continue;
                }
                let looks = &history[&raised];
                let start = looks
                    .iter()
                    .rposition(|(at, _)| *at + HOLD_MS <= now)
                    .expect("a raise has a look at least the hold before it");
                for (at, fact) in &looks[start..] {
                    prop_assert!(stands(kind, fact, *at), "{kind:?} rang over a gap at {at}");
                }
            }
            fold(&mut shown, &changes);
            prop_assert_eq!(&shown, alerter.live());
            for (key, kind) in alerter.live() {
                prop_assert_ne!(Some(*key), looking);
                let fact = alerter.fact(*key).expect("a live alert's key is known");
                prop_assert!(stands(*kind, fact, now));
            }
        }
    }

    /// A cause that ends inside its hold never rings, however often it comes back.
    #[test]
    fn a_flicker_never_rings(
        blips in proptest::collection::vec(
            (2u8..5, 1u64..HOLD_MS, 0u64..HOLD_MS, 0u64..5_000),
            1..40,
        ),
    ) {
        let key = Key::Session(7);
        let mut alerter = Alerter::default();
        alerter.seed(key, Fact::idle(T0));
        let mut now = T0;
        let mut ended = 0;
        for (code, lasts, step_at, gap) in blips {
            ended += 1;
            let need = Some(format!("need-{ended}"));
            alerter.observe(key, fact(phase(code), need.as_deref(), ended, true, now), now);
            prop_assert!(raises(&alerter.step(now + step_at.min(lasts - 1))).is_empty());
            now += lasts;
            alerter.observe(key, fact(Phase::Working, None, ended, false, now), now);
            prop_assert!(raises(&alerter.step(now)).is_empty());
            now += gap;
            prop_assert!(raises(&alerter.step(now)).is_empty());
        }
    }

    /// Whatever stood when a key was first seen, and every replay of what it holds, seeds
    /// silently.
    #[test]
    fn seeding_and_replay_never_ring(
        facts in proptest::collection::vec((0u8..5, 0u8..3, any::<bool>()), 1..20),
        steps in proptest::collection::vec(0u64..10_000, 1..10),
    ) {
        let mut alerter = Alerter::default();
        let mut now = T0;
        for (at, (code, need, unseen)) in facts.iter().enumerate() {
            let key = Key::Session(i64::try_from(at).unwrap_or_default());
            let need = (*code == 2).then(|| format!("need-{need}"));
            let seeded = Fact {
                phase: phase(*code),
                need,
                ended: 3,
                unseen: *unseen,
                active_at: now,
                quiet: false,
            };
            // A first look, and a resync replaying the same.
            alerter.observe(key, seeded.clone(), now);
            alerter.seed(key, seeded);
        }
        for dt in steps {
            now += dt;
            prop_assert!(alerter.step(now).is_empty());
        }
    }

    /// A terminal rings at most once every gap, and terminals at most a burst every window.
    #[test]
    fn terminals_are_rate_limited(
        flips in proptest::collection::vec((0u8..6, 0u64..20_000), 1..60),
    ) {
        let mut alerter = Alerter::default();
        let keys: Vec<Key> = (0..6).map(|_| Key::Terminal(TerminalId::new())).collect();
        for key in &keys {
            alerter.seed(*key, Fact::idle(T0));
        }
        let mut now = T0;
        let mut rung: Vec<(u64, Key)> = Vec::new();
        for (count, (which, dt)) in flips.into_iter().enumerate() {
            let key = keys[usize::from(which)];
            now += dt;
            let need = format!("blocked-{count}");
            alerter.observe(key, fact(Phase::NeedsYou, Some(&need), 0, false, now), now);
            now += HOLD_MS;
            for (raised, _) in raises(&alerter.step(now)) {
                rung.push((now, raised));
            }
            alerter.observe(key, fact(Phase::Idle, None, 0, false, now), now);
            alerter.step(now);
        }
        for (at, key) in &rung {
            let close = rung
                .iter()
                .filter(|(other, same)| same == key && other < at && at - other < EXTERNAL_GAP_MS)
                .count();
            prop_assert_eq!(close, 0);
            let window = rung
                .iter()
                .filter(|(other, _)| other <= at && at - other < EXTERNAL_WINDOW_MS)
                .count();
            prop_assert!(window <= EXTERNAL_BURST);
        }
    }
}

/// Every banner the engine would post, kept instead.
#[derive(Default)]
struct Recorded {
    posted: Mutex<Vec<String>>,
    withdrawn: Mutex<Vec<String>>,
}

impl Banners for Recorded {
    fn post(&self, alert: &Alert, _sound: bool) {
        self.posted.lock().unwrap().push(alert.id.clone());
    }

    fn withdraw(&self, id: &str) {
        self.withdrawn.lock().unwrap().push(id.to_owned());
    }
}

/// A viewer of `WatchAlerts`: what it folded so far.
struct Viewer {
    items: BoxStream<'static, Item<AlertsState, AlertsEvent>>,
    state: AlertsState,
    events: Vec<AlertsEvent>,
}

impl Viewer {
    fn join(hub: &Arc<AlertsHub>) -> Self {
        Self {
            items: hub.watch(&WatchAlerts { since: None }),
            state: AlertsState::default(),
            events: Vec::new(),
        }
    }

    /// Folds everything queued; the events it received since the last pump.
    fn pump(&mut self) -> Vec<AlertsEvent> {
        let before = self.events.len();
        while let Some(Some(item)) = self.items.next().now_or_never() {
            match item {
                Item::Snapshot { state, .. } => self.state = state,
                Item::Events { events, .. } => {
                    for event in events {
                        self.state.apply(&event);
                        self.events.push(event);
                    }
                }
                Item::Resync => panic!("a viewer that keeps up is never resynced"),
            }
        }
        self.events[before..].to_vec()
    }
}

fn summary(row: i64, id: SessionId, busy: bool, unread: bool) -> SessionSummary {
    SessionSummary {
        id,
        session: Session {
            id: row,
            title: "Fix the flaky test".into(),
            provider: "claude".into(),
            cwd: "/work/onicode".into(),
            ..Session::default()
        },
        key: row.to_string(),
        worktree: None,
        draft: false,
        automation: None,
        busy,
        unread,
        model: None,
    }
}

fn events<E>(events: Vec<E>) -> Item<WorkspaceState, E> {
    Item::Events {
        first: 1,
        last: u64::try_from(events.len()).unwrap_or_default(),
        events,
    }
}

fn session_events(events: Vec<SessionEvent>) -> Item<SessionState, SessionEvent> {
    Item::Events {
        first: 1,
        last: u64::try_from(events.len()).unwrap_or_default(),
        events,
    }
}

fn status(text: &str, finished: bool) -> SessionEvent {
    SessionEvent::Status {
        status: text.into(),
        turn: Some(TurnState {
            provider_turn_id: None,
            ready: true,
            finished,
        }),
    }
}

fn report(
    viewport: &str,
    focused: bool,
    on_screen: Option<OnScreen>,
    presents: bool,
) -> SetViewportFocus {
    SetViewportFocus {
        viewport: viewport.into(),
        focused,
        on_screen,
        presents,
        banners: true,
        sound: false,
        mutes: Mutes::default(),
    }
}

/// A chat's turn from the workspace and its session: an approval rings after its hold with its
/// buttons, the engine posts it while no viewer presents, the turn's end takes it back and rings
/// "finished" for the viewer that now presents, and looking at the chat withdraws it and marks it
/// seen. A terminal's blocked program rings too, without buttons; a flicker of one never does.
#[test]
fn hub_news_become_alerts_a_viewer_folds() {
    let banners = Arc::new(Recorded::default());
    let hub = AlertsHub::with(banners.clone(), Limits::default());
    let mut viewer = Viewer::join(&hub);
    let id = SessionId::new();
    let row = 4;

    let wants = hub.workspace(
        Item::Snapshot {
            at: onicode_protocol::Cursor::default(),
            state: WorkspaceState {
                sessions: vec![summary(row, id, false, true)],
                ..WorkspaceState::default()
            },
        },
        T0,
    );
    assert!(wants.watch.is_empty() && wants.ended.is_empty());
    assert!(hub.step(T0).seen.is_empty());
    assert!(
        viewer.pump().is_empty(),
        "an unread chat at boot is no alert"
    );

    let wants = hub.workspace(
        events(vec![WorkspaceEvent::Session(Box::new(summary(
            row, id, true, true,
        )))]),
        T0 + 100,
    );
    assert_eq!(wants.watch, vec![(row, id)]);
    hub.session(
        row,
        Item::Snapshot {
            at: onicode_protocol::Cursor::default(),
            state: SessionState::default(),
        },
        T0 + 100,
    );
    hub.session(
        row,
        session_events(vec![
            SessionEvent::Approval(Approval {
                id: "a1".into(),
                title: "Run cargo test".into(),
                detail: String::new(),
                can_remember: true,
            }),
            status("Waiting for approval", false),
        ]),
        T0 + 1_000,
    );
    assert!(hub.step(T0 + 1_000).next.is_some());
    assert!(viewer.pump().is_empty(), "held, not raised yet");
    hub.step(T0 + 1_000 + 1_500);
    let raised = viewer.pump();
    let [AlertsEvent::Raised(alert)] = raised.as_slice() else {
        panic!("one raise, not {raised:?}");
    };
    assert_eq!(alert.id, alerts::session_alert_id(row));
    assert_eq!(alert.kind, AlertKind::NeedsYou);
    assert_eq!(alert.subject, Subject::Session { session: id, row });
    assert_eq!(alert.body, "Approve: Run cargo test");
    assert_eq!(
        alert.need,
        Some(Need::Approval {
            approval: "a1".into()
        })
    );
    assert_eq!(
        alert.actions,
        vec![AlertAction::Approve, AlertAction::Deny, AlertAction::Stop]
    );
    assert_eq!(*banners.posted.lock().unwrap(), vec![alert.id.clone()]);

    // The app now presents: the engine posts nothing more of its own.
    hub.set_focus(report("app", false, None, true), T0 + 3_000);
    hub.session(
        row,
        session_events(vec![
            SessionEvent::ApprovalResolved("a1".into()),
            status("Ready", true),
        ]),
        T0 + 4_000,
    );
    let wants = hub.workspace(
        events(vec![WorkspaceEvent::Session(Box::new(summary(
            row, id, false, true,
        )))]),
        T0 + 4_000,
    );
    assert_eq!(wants.ended, vec![(row, id)]);
    assert!(!hub.turn_ended(row, T0 + 4_000), "no next turn runs");
    hub.step(T0 + 4_000);
    assert_eq!(
        viewer.pump(),
        vec![AlertsEvent::Withdrawn(alerts::session_alert_id(row))]
    );
    assert_eq!(*banners.withdrawn.lock().unwrap(), vec![alert.id.clone()]);
    hub.step(T0 + 5_500);
    let finished = viewer.pump();
    let [AlertsEvent::Raised(done)] = finished.as_slice() else {
        panic!("one raise, not {finished:?}");
    };
    assert_eq!(done.kind, AlertKind::Finished);
    assert_eq!(done.actions, vec![AlertAction::Reply]);
    assert_eq!(banners.posted.lock().unwrap().len(), 1);

    // Looking at the chat takes it back and marks it seen for every device.
    hub.set_focus(
        report("app", true, Some(OnScreen::Session(row)), true),
        T0 + 6_000,
    );
    let step = hub.step(T0 + 6_000);
    assert_eq!(step.seen, vec![row]);
    assert_eq!(
        viewer.pump(),
        vec![AlertsEvent::Withdrawn(alerts::session_alert_id(row))]
    );

    // A terminal: seeded as it stands, then its program blocks.
    let terminal = TerminalId::new();
    let listed = |status: Option<ProgramStatus>| {
        vec![TerminalSummary {
            terminal,
            cwd: "/work/onicode".into(),
            exited: false,
            status,
        }]
    };
    let blocked = ProgramStatus {
        state: ProgramState::Blocked,
        kind: None,
        progress: None,
        app: Some("claude-code".into()),
        title: None,
        message: Some("Allow Bash(rm -rf target)?".into()),
    };
    hub.terminals(&listed(None), false, T0 + 7_000);
    hub.terminals(&listed(Some(blocked.clone())), true, T0 + 7_100);
    hub.terminals(&listed(None), true, T0 + 7_600);
    hub.step(T0 + 9_000);
    assert!(viewer.pump().is_empty(), "a flicker never rings");
    hub.terminals(&listed(Some(blocked)), true, T0 + 10_000);
    hub.step(T0 + 11_500);
    let rung = viewer.pump();
    let [AlertsEvent::Raised(program)] = rung.as_slice() else {
        panic!("one raise, not {rung:?}");
    };
    assert_eq!(program.subject, Subject::Terminal(terminal));
    assert_eq!(program.title, "Claude Code");
    assert_eq!(program.body, "Allow Bash(rm -rf target)?");
    assert!(program.actions.is_empty());

    // A viewer that joins now starts from what shows.
    assert_eq!(viewer.state, hub.current());
    let mut late = Viewer::join(&hub);
    late.pump();
    assert_eq!(late.state, hub.current());
}

/// A turn stopped while it waited on its owner is no "finished"; a turn that ended on an error
/// rings "failed" with its words; a side chat never rings.
#[test]
fn stopped_failed_and_side_chats() {
    let hub = AlertsHub::with(Arc::new(Recorded::default()), Limits::default());
    let mut viewer = Viewer::join(&hub);
    let (id, row) = (SessionId::new(), 9);
    hub.workspace(
        Item::Snapshot {
            at: onicode_protocol::Cursor::default(),
            state: WorkspaceState {
                sessions: vec![summary(row, id, false, false)],
                ..WorkspaceState::default()
            },
        },
        T0,
    );
    let question = Question {
        id: "q1".into(),
        title: "Pick one".into(),
        items: vec![QuestionItem {
            id: "i1".into(),
            prompt: "Ship it?".into(),
            multiple: false,
            allow_custom: true,
            options: vec![
                QuestionOption {
                    id: "o1".into(),
                    label: "Yes".into(),
                    description: String::new(),
                },
                QuestionOption {
                    id: "o2".into(),
                    label: "No".into(),
                    description: String::new(),
                },
            ],
        }],
        auto_resolve_at: None,
    };
    let turn = |hub: &AlertsHub, at: u64, events: Vec<SessionEvent>| {
        hub.workspace(
            self::events(vec![WorkspaceEvent::Session(Box::new(summary(
                row, id, true, false,
            )))]),
            at,
        );
        hub.session(row, session_events(events), at);
        hub.workspace(
            self::events(vec![WorkspaceEvent::Session(Box::new(summary(
                row, id, false, true,
            )))]),
            at,
        );
        hub.turn_ended(row, at);
        hub.step(at + HOLD_MS);
    };
    turn(
        &hub,
        T0 + 1_000,
        vec![
            SessionEvent::Question(question.clone()),
            status("Ready", true),
        ],
    );
    assert!(viewer.pump().is_empty(), "stopped while it asked");

    turn(
        &hub,
        T0 + 10_000,
        vec![status("Claude exited with 1", true)],
    );
    let failed = viewer.pump();
    let [AlertsEvent::Raised(alert)] = failed.as_slice() else {
        panic!("one raise, not {failed:?}");
    };
    assert_eq!(alert.kind, AlertKind::Failed);
    assert_eq!(alert.body, "Claude exited with 1");

    // A question's quick answers.
    hub.workspace(
        events(vec![WorkspaceEvent::Session(Box::new(summary(
            row, id, true, false,
        )))]),
        T0 + 20_000,
    );
    hub.session(
        row,
        session_events(vec![SessionEvent::Question(question)]),
        T0 + 20_000,
    );
    hub.step(T0 + 20_000 + HOLD_MS);
    let asked = viewer.pump();
    let Some(AlertsEvent::Raised(asked)) = asked.last() else {
        panic!("a raise, not {asked:?}");
    };
    assert_eq!(
        asked.need,
        Some(Need::Question {
            question: "q1".into(),
            approve: Some("o1".into()),
            deny: Some("o2".into()),
            custom: true,
        })
    );
    assert_eq!(asked.actions, AlertAction::ALL.to_vec());

    // Archived: what showed goes, and nothing new rings.
    let mut archived = summary(row, id, true, false);
    archived.session.archived = true;
    hub.workspace(
        events(vec![WorkspaceEvent::Session(Box::new(archived))]),
        T0 + 30_000,
    );
    hub.step(T0 + 30_000 + HOLD_MS);
    assert_eq!(
        viewer.pump(),
        vec![AlertsEvent::Withdrawn(alerts::session_alert_id(row))]
    );
}

/// The folders the mutes test runs chats in: a repository's two checkouts, a folder of its own,
/// one no mute names, and the home folder.
const FOLDERS: [&str; 5] = [
    "/work/onicode",
    "/work/onicode-wt",
    "/work/site",
    "/work/other",
    "/Users/me",
];

fn mute_rule() -> impl Strategy<Value = MuteRule> {
    (
        any::<bool>(),
        prop_oneof![Just(0), Just(T0 - 1), Just(T0 + 60_000), Just(u64::MAX),],
    )
        .prop_map(|(enabled, after)| MuteRule { enabled, after })
}

/// A repository with both checkouts, then the first checkout and the folder of its own, each
/// muted alone: the repository decides for its checkout.
fn mutes() -> impl Strategy<Value = Mutes> {
    proptest::collection::vec((mute_rule(), mute_rule()), 3).prop_map(|rules| {
        let paths = [
            vec![FOLDERS[0].to_owned(), format!("{}/", FOLDERS[1])],
            vec![FOLDERS[0].to_owned()],
            vec![FOLDERS[2].to_owned()],
        ];
        Mutes {
            projects: paths
                .into_iter()
                .zip(rules)
                .map(|(paths, (needs_you, finished))| ProjectMute {
                    paths,
                    needs_you,
                    finished,
                })
                .collect(),
        }
    })
}

fn summary_in(row: i64, id: SessionId, cwd: &str, busy: bool, unread: bool) -> SessionSummary {
    let mut summary = summary(row, id, busy, unread);
    summary.session.cwd = cwd.into();
    summary
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The engine alone applies the project mutes. An alert rings, to every viewer and as the
    /// engine's own banner, exactly when the mutes allow it at its raise: the mutes of every
    /// fresh report (a viewer holding none never loosens another's), or the last report's once
    /// none is fresh. What a mute holds back never shows anywhere, and work outside a project
    /// never rings.
    #[test]
    fn the_engine_alone_applies_project_mutes(
        mutes in mutes(),
        folder in 0..FOLDERS.len(),
        asks in any::<bool>(),
        presents in any::<bool>(),
        stale in any::<bool>(),
        second in any::<bool>(),
        late in 0u64..120_000,
    ) {
        let banners = Arc::new(Recorded::default());
        let hub = AlertsHub::at_home(banners.clone(), Limits::default(), Some("/Users/me".into()));
        let mut viewer = Viewer::join(&hub);
        let (id, row) = (SessionId::new(), 3);
        let cwd = FOLDERS[folder];
        let start = T0 + late;
        hub.workspace(
            Item::Snapshot {
                at: onicode_protocol::Cursor::default(),
                state: WorkspaceState {
                    sessions: vec![summary_in(row, id, cwd, false, false)],
                    ..WorkspaceState::default()
                },
            },
            start,
        );
        // The report goes stale before the raise, or stands at it; a second viewer without
        // mutes may stand beside it.
        let reported = if stale { start - 30_000 } else { start };
        hub.set_focus(
            SetViewportFocus {
                mutes: mutes.clone(),
                ..report("app", false, None, presents)
            },
            reported,
        );
        if second && !stale {
            hub.set_focus(report("phone", false, None, true), reported);
        }
        hub.workspace(
            events(vec![WorkspaceEvent::Session(Box::new(summary_in(row, id, cwd, true, false)))]),
            start,
        );
        hub.session(
            row,
            Item::Snapshot {
                at: onicode_protocol::Cursor::default(),
                state: SessionState::default(),
            },
            start,
        );
        let kind = if asks {
            hub.session(
                row,
                session_events(vec![
                    SessionEvent::Approval(Approval {
                        id: "a1".into(),
                        title: "Run cargo test".into(),
                        detail: String::new(),
                        can_remember: true,
                    }),
                    status("Waiting for approval", false),
                ]),
                start,
            );
            AlertKind::NeedsYou
        } else {
            hub.session(row, session_events(vec![status("Ready", true)]), start);
            hub.workspace(
                events(vec![WorkspaceEvent::Session(Box::new(summary_in(row, id, cwd, false, true)))]),
                start,
            );
            hub.turn_ended(row, start);
            AlertKind::Finished
        };
        hub.step(start);
        let raised_at = start + HOLD_MS;
        hub.step(raised_at);
        let allowed = mutes.allows(cwd, kind, Some("/Users/me"), raised_at);
        let rung: Vec<AlertKind> = viewer
            .pump()
            .into_iter()
            .filter_map(|event| match event {
                AlertsEvent::Raised(alert) => Some(alert.kind),
                AlertsEvent::Withdrawn(_) => None,
            })
            .collect();
        prop_assert_eq!(rung.clone(), if allowed { vec![kind] } else { Vec::new() });
        prop_assert_eq!(hub.current().alerts.len(), usize::from(allowed));
        let presenting = (presents || second) && !stale;
        prop_assert_eq!(
            banners.posted.lock().unwrap().len(),
            usize::from(allowed && !presenting),
            "the engine's own banner follows the same mutes"
        );
        if folder == FOLDERS.len() - 1 {
            prop_assert!(rung.is_empty(), "the home folder is no project");
        }
    }
}
