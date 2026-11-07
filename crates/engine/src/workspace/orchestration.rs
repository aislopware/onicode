//! A reviewed orchestration plan's dispatch: which tasks start, which wait on a dependency or an
//! overlapping file scope, which a failure blocks, and what the lead and the sidebar are told.
//! Pure: the caller creates each job's worker session (`CreateSession`), reports how each worker
//! ended, and keeps the summary (`SaveOrchestration`).
use std::collections::HashSet;
use std::path::Path;

use onicode_core::agent::Settings;
use onicode_core::orchestration::{self, Plan, Summary, SummaryTask, Task};

/// At most this many workers run at once.
pub const MAX_WORKERS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Starting,
    Running,
    Done,
    Failed,
    Blocked,
}

#[derive(Clone, Debug)]
pub struct Assignment {
    pub task: Task,
    pub session: Option<i64>,
    pub state: TaskState,
    pub report: String,
}

/// A worker to start: its task, where and with what, and the reports of the tasks done so far.
#[derive(Clone, Debug)]
pub struct Job {
    pub index: usize,
    pub task: Task,
    pub cwd: String,
    pub provider: String,
    pub settings: Settings,
    pub reports: String,
}

/// One lead's run of its plan.
#[derive(Clone, Debug)]
pub struct Run {
    pub cwd: String,
    pub provider: String,
    pub settings: Settings,
    pub tasks: Vec<Assignment>,
    pub stopped: bool,
}

impl Run {
    /// A run of `plan` in the checkout `cwd`, its workers on `provider` with the lead's
    /// `settings`, less what belongs to the lead alone.
    #[must_use]
    pub fn new(plan: Plan, cwd: String, provider: String, mut settings: Settings) -> Self {
        settings.plan = false;
        settings.orchestrator = false;
        settings.operator = false;
        settings.operator_enabled = false;
        settings.handoff = None;
        settings.turns.clear();
        settings.environment.clear();
        settings.attachments.clear();
        Self {
            cwd,
            provider,
            settings,
            tasks: plan
                .tasks
                .into_iter()
                .map(|task| Assignment {
                    task,
                    session: None,
                    state: TaskState::Pending,
                    report: String::new(),
                })
                .collect(),
            stopped: false,
        }
    }

    /// Stops the run: what has not started never will. Returns the sessions still working, for
    /// the caller to stop.
    pub fn stop(&mut self) -> Vec<i64> {
        self.stopped = true;
        let mut running = Vec::new();
        for task in &mut self.tasks {
            if matches!(task.state, TaskState::Pending | TaskState::Starting) {
                task.state = TaskState::Blocked;
            }
            if task.state == TaskState::Running
                && let Some(session) = task.session
            {
                running.push(session);
            }
        }
        running
    }

    /// The worker in `session` went idle: its task is done or failed, with its report.
    pub fn worker_finished(&mut self, session: i64, success: bool, report: String) {
        for task in &mut self.tasks {
            if task.session == Some(session) && task.state == TaskState::Running {
                task.report = report;
                task.state = if success {
                    TaskState::Done
                } else {
                    TaskState::Failed
                };
                return;
            }
        }
    }

    /// Job `index`'s worker session exists; it runs unless the run stopped meanwhile. Returns
    /// whether to start it.
    pub fn worker_created(&mut self, index: usize, session: i64) -> bool {
        let start = !self.stopped
            && self
                .tasks
                .get(index)
                .is_some_and(|task| task.state == TaskState::Starting);
        if start && let Some(task) = self.tasks.get_mut(index) {
            task.session = Some(session);
            task.state = TaskState::Running;
        }
        start
    }

    /// Job `index`'s worker could not be created.
    pub fn worker_failed(&mut self, index: usize, error: String) {
        if let Some(task) = self.tasks.get_mut(index) {
            task.state = TaskState::Failed;
            task.report = error;
        }
    }

    /// Blocks what a failure blocks, then starts what is ready: its dependencies done, its files
    /// clear of every running task's, within [`MAX_WORKERS`].
    pub fn schedule(&mut self) -> Vec<Job> {
        // Failed dependencies transitively block their dependants.
        loop {
            let failed: HashSet<_> = self
                .tasks
                .iter()
                .filter(|t| matches!(t.state, TaskState::Failed | TaskState::Blocked))
                .map(|t| t.task.id.clone())
                .collect();
            let mut changed = false;
            for task in &mut self.tasks {
                if task.state == TaskState::Pending
                    && task.task.depends_on.iter().any(|id| failed.contains(id))
                {
                    task.state = TaskState::Blocked;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut occupied: Vec<Vec<String>> = self
            .tasks
            .iter()
            .filter(|t| matches!(t.state, TaskState::Starting | TaskState::Running))
            .map(|t| t.task.files.clone())
            .collect();
        let completed: HashSet<_> = self
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Done)
            .map(|t| t.task.id.clone())
            .collect();
        let reports = self
            .tasks
            .iter()
            .filter(|t| t.state == TaskState::Done)
            .map(|t| format!("{}: {}", t.task.title, t.report))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut jobs = Vec::new();
        if self.stopped {
            return jobs;
        }
        for (index, task) in self.tasks.iter_mut().enumerate() {
            if occupied.len() >= MAX_WORKERS {
                break;
            }
            if task.state != TaskState::Pending
                || !task.task.depends_on.iter().all(|id| completed.contains(id))
                || occupied
                    .iter()
                    .any(|files| orchestration::overlaps(files, &task.task.files))
            {
                continue;
            }
            task.state = TaskState::Starting;
            occupied.push(task.task.files.clone());
            jobs.push(Job {
                index,
                task: task.task.clone(),
                cwd: self.cwd.clone(),
                provider: self.provider.clone(),
                settings: self.settings.clone(),
                reports: reports.clone(),
            });
        }
        jobs
    }

    /// Every task is done, failed or blocked.
    #[must_use]
    pub fn done(&self) -> bool {
        self.tasks.iter().all(|t| {
            matches!(
                t.state,
                TaskState::Done | TaskState::Failed | TaskState::Blocked
            )
        })
    }

    /// What the lead shows while the run goes on.
    #[must_use]
    pub fn progress(&self) -> String {
        self.tasks
            .iter()
            .map(|t| {
                format!(
                    "{}: {}{}",
                    t.task.title,
                    match t.state {
                        TaskState::Pending => "waiting",
                        TaskState::Starting => "starting",
                        TaskState::Running => "working",
                        TaskState::Done => "done",
                        TaskState::Failed => "failed",
                        TaskState::Blocked => "blocked",
                    },
                    t.session
                        .map(|id| format!(" (session {id})"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// What the sidebar lists of the lead's agents, during and after the run.
    #[must_use]
    pub fn summary(&self) -> Summary {
        Summary {
            live: !self.done(),
            tasks: self
                .tasks
                .iter()
                .map(|t| SummaryTask {
                    session: t.session,
                    title: t.task.title.clone(),
                    provider: self.provider.clone(),
                    model: self.settings.model.clone(),
                    status: match t.state {
                        TaskState::Pending | TaskState::Starting => "queued",
                        TaskState::Running => "running",
                        TaskState::Done => "completed",
                        TaskState::Failed => "failed",
                        TaskState::Blocked => "blocked",
                    }
                    .into(),
                })
                .collect(),
        }
    }

    /// Every worker's report, for the lead to verify once the run is done.
    #[must_use]
    pub fn reports(&self) -> String {
        self.tasks
            .iter()
            .map(|t| {
                format!(
                    "{} [{}]\n{}",
                    t.task.title,
                    if t.state == TaskState::Done {
                        "completed"
                    } else {
                        "incomplete"
                    },
                    t.report
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Checks that none of `files` escapes the checkout at `cwd`, existing symlinked parents
/// included: `CreateSession` asks it of a worker on the disk that holds its checkout.
pub(crate) fn check_scope(cwd: &str, files: &[String]) -> Result<(), String> {
    let root = Path::new(cwd).canonicalize().map_err(|e| e.to_string())?;
    for file in files {
        let mut path = root.join(file);
        while !path.exists() {
            if !path.pop() {
                break;
            }
        }
        if !path
            .canonicalize()
            .map_err(|e| e.to_string())?
            .starts_with(&root)
        {
            return Err(format!("Assignment scope escapes the checkout: {file}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(files: &[&str]) -> Plan {
        Plan {
            title: "Review".into(),
            summary: String::new(),
            tasks: files
                .iter()
                .enumerate()
                .map(|(ix, file)| Task {
                    id: format!("task-{ix}"),
                    title: format!("Assignment {ix}"),
                    prompt: "Inspect and report".into(),
                    files: vec![(*file).into()],
                    depends_on: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn dispatch_caps_workers_and_serializes_overlapping_scopes() {
        let mut run = Run::new(
            plan(&["src", "src/main.rs", "tests", "docs", "examples", "scripts"]),
            "/project".into(),
            "codex".into(),
            Settings::default(),
        );
        let jobs = run.schedule();
        assert_eq!(jobs.len(), MAX_WORKERS);
        assert_eq!(
            run.tasks[1].state,
            TaskState::Pending,
            "src/main.rs waits for src"
        );
        assert!(run.stop().is_empty());
        assert!(run.done());
        assert!(run.schedule().is_empty());
    }

    #[test]
    fn a_failed_dependency_blocks_its_dependants() {
        let mut plan = plan(&["a", "b", "c"]);
        plan.tasks[1].depends_on = vec!["task-0".into()];
        plan.tasks[2].depends_on = vec!["task-1".into()];
        let mut run = Run::new(plan, "/project".into(), "codex".into(), Settings::default());
        let jobs = run.schedule();
        assert_eq!(jobs.len(), 1);
        assert!(run.worker_created(jobs[0].index, 7));
        run.worker_finished(7, false, "broke".into());
        assert!(run.schedule().is_empty());
        assert!(run.done());
        assert_eq!(run.tasks[2].state, TaskState::Blocked);
        assert!(!run.summary().live);
    }
}
