//! Automations against their ownership model (engine.md, "Automations"): an automation belongs
//! to the engine that holds its project, which stores it, schedules it and runs it with no
//! viewer. Two whole engines in memory, each with its own project folder and a scripted provider,
//! take seeded interleavings of creating automations, running them now (on their own engine and
//! on the other one), scheduler beats in virtual time and turns ending ready, failed or stopped.
//! Every turn must run on the engine that holds its automation, in that engine's project; every
//! run must be one of that engine's turns and record how it ended; the other engine never lists,
//! schedules or runs it. Nothing here waits on a clock, runs a provider or touches the network.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::{LocalPool, block_on};
use futures::future::BoxFuture;
use futures::task::LocalSpawnExt;
use onicode_core::automations::{AutomationTrigger, AutomationUpsert, Service};
use onicode_core::session_groups::WorkItem;
use onicode_core::session_title::{GeneratedTitle, TextModel, WorkItemHint};
use onicode_protocol::OpId;
use onicode_protocol::work::{AutomationRuns, RunAutomationNow, UpsertAutomation};
use proptest::prelude::*;

use super::chores::Chores;
use crate::local::{DATABASE, Local, Options};
use crate::session::{Runner, Spawn, TurnJob};
use crate::turn::Update;

type Work = Arc<Mutex<Vec<BoxFuture<'static, ()>>>>;
type Jobs = Arc<Mutex<Vec<TurnJob>>>;

/// The engines' background work, held until the test runs it.
struct Queued(Work);

impl Spawn for Queued {
    fn spawn(&self, work: BoxFuture<'static, ()>) {
        self.0.lock().unwrap().push(work);
    }
}

/// No generated titles, work items or branch names.
struct NoChores;

impl Chores for NoChores {
    fn title(&self, _: TextModel, _: &Path, _: &str, _: Arc<AtomicBool>) -> Option<GeneratedTitle> {
        None
    }

    fn work_item(&self, _: &str, _: &Path, _: Option<&WorkItemHint>) -> Option<WorkItem> {
        None
    }

    fn branch(&self, _: TextModel, _: &Path, _: &str, _: Arc<AtomicBool>) -> Option<String> {
        None
    }
}

/// How a turn ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Ready,
    Failed,
    Stopped,
}

impl End {
    /// The status its run records.
    fn status(self) -> &'static str {
        match self {
            Self::Ready => "succeeded",
            Self::Failed => "failed",
            Self::Stopped => "cancelled",
        }
    }
}

/// One machine: its engine, the turns its provider was handed, and its project folder.
struct Machine {
    local: Local,
    jobs: Jobs,
    project: tempfile::TempDir,
    /// How each turn the provider was handed ended, by its place in `jobs`; `None` while it
    /// runs.
    ends: Vec<Option<End>>,
}

fn options(jobs: &Jobs) -> Options {
    let held = jobs.clone();
    let runner: Runner = Arc::new(move |job| held.lock().unwrap().push(job));
    Options {
        chores: Some(Arc::new(NoChores)),
        runner: Some(runner),
        ..crate::testing::quiet()
    }
}

impl Machine {
    fn new(work: &Work) -> Self {
        let jobs = Jobs::default();
        let local = Local::memory(Arc::new(Queued(work.clone())), options(&jobs)).unwrap();
        Self::with(local, jobs)
    }

    fn with(local: Local, jobs: Jobs) -> Self {
        Self {
            local,
            jobs,
            project: tempfile::tempdir().unwrap(),
            ends: Vec::new(),
        }
    }

    fn project(&self) -> String {
        self.project.path().to_string_lossy().into_owned()
    }

    /// Creates an automation in this machine's project, through its own engine: one that runs
    /// every hour, or only when asked.
    fn create(&self, prompt: &str, hourly: bool, now: i64) -> String {
        let mut draft = AutomationUpsert::new(self.project(), "codex".into(), String::new());
        draft.name = format!("Automation {prompt}");
        draft.prompt = prompt.into();
        draft.workspace_mode = "current".into();
        if hourly {
            draft.triggers = Some(vec![AutomationTrigger::new("time", "hourly")]);
        }
        draft.sync_triggers(now);
        let saved = block_on(
            self.local
                .work
                .upsert_automation(Some(OpId::new()), UpsertAutomation { automation: draft }),
        )
        .unwrap();
        saved.id
    }

    /// The turns the provider was handed that have not ended.
    fn running(&self) -> Vec<usize> {
        (0..self.ends.len())
            .filter(|at| self.ends[*at].is_none())
            .collect()
    }

    /// Takes in the turns the provider was handed since the last look.
    fn notice(&mut self) {
        let handed = self.jobs.lock().unwrap().len();
        self.ends.resize(handed, None);
    }

    /// Turn `at` writes, then ends as `end` says.
    fn end(&mut self, at: usize, end: End, pool: &mut LocalPool, work: &Work) {
        {
            let jobs = self.jobs.lock().unwrap();
            jobs[at]
                .updates
                .try_send(Update {
                    status: "Working".into(),
                    ..Update::default()
                })
                .unwrap();
        }
        settle(pool, work);
        {
            let jobs = self.jobs.lock().unwrap();
            let job = &jobs[at];
            let status = match end {
                End::Ready => "Ready",
                End::Failed => "The provider stopped answering",
                End::Stopped => {
                    job.cancel.store(true, Ordering::Relaxed);
                    "Stopped"
                }
            };
            job.updates
                .try_send(Update {
                    status: status.into(),
                    finished: true,
                    ..Update::default()
                })
                .unwrap();
        }
        settle(pool, work);
        self.ends[at] = Some(end);
    }
}

/// Runs the engines' work until nothing more can happen.
fn settle(pool: &mut LocalPool, work: &Work) {
    loop {
        let queued = std::mem::take(&mut *work.lock().unwrap());
        let idle = queued.is_empty();
        for queued in queued {
            pool.spawner().spawn_local(queued).unwrap();
        }
        pool.run_until_stalled();
        if idle && work.lock().unwrap().is_empty() {
            return;
        }
    }
}

#[derive(Clone, Debug)]
enum Step {
    /// Machine `on` gets an automation in its project.
    Create { on: usize, hourly: bool },
    /// Machine `on`'s `which`th automation runs now, asked of its own engine.
    RunNow { on: usize, which: usize },
    /// Machine `on`'s `which`th automation is asked of the other engine, which does not hold it.
    Elsewhere { on: usize, which: usize },
    /// Both clocks beat `minutes` later.
    Beat { minutes: u8 },
    /// A running turn of machine `on` ends.
    End { on: usize, which: usize, end: End },
}

fn step() -> impl Strategy<Value = Step> {
    let end = prop_oneof![Just(End::Ready), Just(End::Failed), Just(End::Stopped)];
    prop_oneof![
        2 => (0..2usize, any::<bool>()).prop_map(|(on, hourly)| Step::Create { on, hourly }),
        2 => (0..2usize, any::<usize>()).prop_map(|(on, which)| Step::RunNow { on, which }),
        1 => (0..2usize, any::<usize>()).prop_map(|(on, which)| Step::Elsewhere { on, which }),
        3 => (1..90u8).prop_map(|minutes| Step::Beat { minutes }),
        3 => (0..2usize, any::<usize>(), end)
            .prop_map(|(on, which, end)| Step::End { on, which, end }),
    ]
}

/// What the model holds: each machine's automations, as `(id, prompt)`.
type Owned = [Vec<(String, String)>; 2];

/// Every turn ran on the machine that holds its automation, in its project; every run of a
/// machine's automation is one of its turns and records how that turn ended; the other machine
/// never lists one or holds a run of it.
fn check(machines: &[Machine; 2], owned: &Owned) {
    for (on, machine) in machines.iter().enumerate() {
        let other = &machines[1 - on];
        let mut listed: Vec<String> = machine
            .local
            .work
            .automations()
            .unwrap()
            .into_iter()
            .map(|automation| automation.id)
            .collect();
        listed.sort();
        let mut expected: Vec<String> = owned[on].iter().map(|(id, _)| id.clone()).collect();
        expected.sort();
        assert_eq!(listed, expected, "machine {on} lists its own automations");
        let jobs = machine.jobs.lock().unwrap();
        for job in jobs.iter() {
            assert_eq!(job.summary.cwd, machine.project(), "in its own project");
            assert!(
                owned[on].iter().any(|(_, prompt)| *prompt == job.prompt),
                "machine {on} ran {:?}, an automation it does not hold",
                job.prompt
            );
        }
        for (id, prompt) in &owned[on] {
            let runs = machine
                .local
                .work
                .automation_runs(&AutomationRuns {
                    automation: id.clone(),
                })
                .unwrap();
            assert!(
                runs.iter().all(|run| run.status != "pending"),
                "a claimed run starts at once"
            );
            let turns: Vec<usize> = (0..jobs.len())
                .filter(|at| jobs[*at].prompt == *prompt)
                .collect();
            let started = runs.iter().filter(|run| run.status != "skipped").count();
            assert_eq!(
                started,
                turns.len(),
                "one turn for every run, on machine {on}"
            );
            for status in ["running", "succeeded", "failed", "cancelled"] {
                let recorded = runs.iter().filter(|run| run.status == status).count();
                let ended = turns
                    .iter()
                    .filter(|at| machine.ends[**at].map_or("running", End::status) == status)
                    .count();
                assert_eq!(recorded, ended, "{status} runs of {prompt} on machine {on}");
            }
            assert!(
                other
                    .local
                    .work
                    .automations()
                    .unwrap()
                    .iter()
                    .all(|automation| automation.id != *id),
                "machine {} never lists machine {on}'s automation",
                1 - on
            );
            let theirs = other
                .local
                .work
                .automation_runs(&AutomationRuns {
                    automation: id.clone(),
                })
                .unwrap();
            assert!(theirs.is_empty(), "nor holds a run of it");
        }
    }
}

fn run_script(steps: &[Step]) {
    let work = Work::default();
    let mut pool = LocalPool::new();
    let mut machines = [Machine::new(&work), Machine::new(&work)];
    let mut owned: Owned = [Vec::new(), Vec::new()];
    let mut now = i64::try_from(onicode_core::time::now_ms()).unwrap();
    let mut made = 0;
    for step in steps {
        match step {
            Step::Create { on, hourly } => {
                made += 1;
                let prompt = format!("Prompt {made}");
                let id = machines[*on].create(&prompt, *hourly, now);
                owned[*on].push((id, prompt));
            }
            Step::RunNow { on, which } => {
                let Some((id, _)) = pick(&owned[*on], *which) else {
                    continue;
                };
                let run = block_on(machines[*on].local.work.run_automation_now(
                    Some(OpId::new()),
                    RunAutomationNow {
                        automation: id.clone(),
                    },
                ))
                .unwrap();
                assert_eq!(run.status, "running", "started in its own engine");
                assert!(run.session_id.is_some(), "in a session of its own engine");
            }
            Step::Elsewhere { on, which } => {
                let Some((id, _)) = pick(&owned[*on], *which) else {
                    continue;
                };
                let asked = block_on(machines[1 - on].local.work.run_automation_now(
                    Some(OpId::new()),
                    RunAutomationNow {
                        automation: id.clone(),
                    },
                ));
                assert!(asked.is_err(), "the other engine does not hold it");
            }
            Step::Beat { minutes } => {
                now += i64::from(*minutes) * 60_000;
                for machine in &machines {
                    machine.local.tick(u64::try_from(now).unwrap()).unwrap();
                }
            }
            Step::End { on, which, end } => {
                let running = machines[*on].running();
                if running.is_empty() {
                    continue;
                }
                let at = running[which % running.len()];
                machines[*on].end(at, *end, &mut pool, &work);
            }
        }
        settle(&mut pool, &work);
        for machine in &mut machines {
            machine.notice();
        }
        check(&machines, &owned);
    }
}

fn pick(owned: &[(String, String)], which: usize) -> Option<&(String, String)> {
    (!owned.is_empty()).then(|| &owned[which % owned.len()])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    #[test]
    fn each_engine_schedules_and_runs_only_its_own_automations(
        steps in proptest::collection::vec(step(), 1..24),
    ) {
        run_script(&steps);
    }
}

/// The proptest's first find, kept as a fixed case: a run asked for before the scheduler's first
/// beat keeps running through it. It failed when the run was made in the millisecond the engine
/// started, which the first beat took for a run a stopped engine left behind; core's
/// `a_run_made_the_millisecond_the_engine_started_is_not_recovered` pins that millisecond.
#[test]
fn a_run_asked_for_before_the_first_beat_keeps_running() {
    run_script(&[
        Step::Create {
            on: 1,
            hourly: false,
        },
        Step::RunNow { on: 1, which: 0 },
        Step::Beat { minutes: 9 },
        Step::End {
            on: 1,
            which: 0,
            end: End::Failed,
        },
        Step::Beat { minutes: 49 },
    ]);
}

/// An automation created on machine B for B's project is scheduled and run by B when its hour
/// comes, with no viewer, and never by A, whose clock beats past the same hour.
#[test]
fn an_automation_runs_on_the_engine_that_holds_its_project() {
    let work = Work::default();
    let mut pool = LocalPool::new();
    let mut machines = [Machine::new(&work), Machine::new(&work)];
    let wall = i64::try_from(onicode_core::time::now_ms()).unwrap();
    let id = machines[1].create("Review the night's changes", true, wall);
    let due = machines[1].local.work.automations().unwrap()[0].next_run_at;
    for machine in &machines {
        machine.local.tick(u64::try_from(due).unwrap()).unwrap();
    }
    settle(&mut pool, &work);
    for machine in &mut machines {
        machine.notice();
    }
    assert_eq!(machines[0].ends.len(), 0, "A runs nothing");
    assert_eq!(machines[1].ends.len(), 1, "B runs it, once");
    {
        let jobs = machines[1].jobs.lock().unwrap();
        assert_eq!(jobs[0].prompt, "Review the night's changes");
        assert_eq!(jobs[0].summary.cwd, machines[1].project());
    }
    machines[1].end(0, End::Ready, &mut pool, &work);
    let runs = machines[1]
        .local
        .work
        .automation_runs(&AutomationRuns {
            automation: id.clone(),
        })
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "succeeded");
    let automation = machines[1].local.work.automations().unwrap().remove(0);
    assert_eq!(automation.last_run_status.as_deref(), Some("succeeded"));
    assert_eq!(automation.last_session_id, runs[0].session_id);
    // The session it ran in is B's, linked to the automation, for every viewer of B.
    let (_, state) = machines[1].local.workspace.snapshot().unwrap();
    let session = state
        .sessions
        .iter()
        .find(|summary| Some(&summary.key) == runs[0].session_id.as_ref())
        .expect("B's workspace lists the run's session");
    assert_eq!(session.automation.as_deref(), Some(id.as_str()));
    assert_eq!(
        session.session.title,
        "Automation Review the night's changes"
    );
    let (_, theirs) = machines[0].local.workspace.snapshot().unwrap();
    assert!(theirs.sessions.is_empty(), "A has no session for it");
}

/// An engine refuses an automation whose project is another machine's: it would schedule a
/// folder it does not hold.
#[test]
fn an_automation_in_a_peer_project_is_refused() {
    let work = Work::default();
    let machine = Machine::new(&work);
    let mut draft = AutomationUpsert::new(
        "peer://nStudio/Users/me/app".into(),
        "codex".into(),
        String::new(),
    );
    draft.name = "Elsewhere".into();
    draft.prompt = "Run there".into();
    let refused = block_on(
        machine
            .local
            .work
            .upsert_automation(Some(OpId::new()), UpsertAutomation { automation: draft }),
    );
    assert!(refused.is_err());
    assert!(machine.local.work.automations().unwrap().is_empty());
}

/// A run its engine claimed but had not started when the engine stopped starts when the engine
/// next runs, in its project.
#[test]
fn a_run_left_pending_starts_when_its_engine_comes_back() {
    let dir = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let path = dir.path().join(DATABASE);
    let earlier = i64::try_from(onicode_core::time::now_ms()).unwrap() - 60_000;
    let automation = {
        // Opened as an engine opens it, before the engine runs.
        drop(onicode_core::store::Store::open(&path).unwrap());
        let mut service = Service::open(&path).unwrap();
        let mut draft = AutomationUpsert::new(
            project.path().to_string_lossy().into_owned(),
            "codex".into(),
            String::new(),
        );
        draft.name = "Left behind".into();
        draft.prompt = "Pick up where it stopped".into();
        draft.workspace_mode = "current".into();
        let saved = service.upsert(draft).unwrap();
        service.run_now(&saved.id, earlier).unwrap();
        saved
    };
    let work = Work::default();
    let mut pool = LocalPool::new();
    let jobs = Jobs::default();
    let local = Local::open(dir.path(), Arc::new(Queued(work.clone())), options(&jobs)).unwrap();
    local
        .tick(u64::try_from(earlier + 120_000).unwrap())
        .unwrap();
    settle(&mut pool, &work);
    let jobs = jobs.lock().unwrap();
    assert_eq!(jobs.len(), 1, "the pending run started");
    assert_eq!(jobs[0].prompt, "Pick up where it stopped");
    let runs = Service::open(&path).unwrap().runs(&automation.id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, "running");
}
