//! The Work group's hub (`docs/onicode/decisions/engine.md`, migration step 6): the Inbox's
//! trackers, the automations the workspace hub schedules, and the GitHub activity of the items
//! sessions link to, with the processes the engine's agents run. Every tracker, credential,
//! network, process and `gh` call runs here, off the caller's thread, behind the [`Trackers`] and
//! [`Gh`] seams the tests script.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use onicode_core::automations::{Automation, AutomationRun, DueAutomationRun};
use onicode_core::inbox::{Checks, Details, Diff, Item, Snapshot, Status, Thread, ViewState};
use onicode_protocol::files::Bytes;
use onicode_protocol::sessions::Groups;
use onicode_protocol::work::{
    AgentProcess, AgentProcesses, AutomationRuns, ConnectInbox, DeleteAutomation,
    HasEventAutomations, InboxAvatar, InboxList, InboxListing, InboxMedia, LinkedActivity,
    LinkedCard, ListAutomations, PostInboxComment, ProjectGroups, ReadInboxChecks,
    ReadInboxDetails, ReadInboxDiff, ReadInboxThread, ReadWorkItem, ReadWorkItemDetails,
    RunAutomationNow, RunInboxAction, SetAutomationEnabled, StopAgentTool, UpsertAutomation,
};
use onicode_protocol::{Call, OpId, Outcome};
use onicode_rpc::{RpcError, RpcReply, RpcService};

use crate::ledger::Ledger;
use crate::service::{Group, reply, routed};
use crate::session::Spawn;
use crate::turn::{Database, locked};
use crate::workspace::WorkspaceHub;

mod linked;
#[cfg(test)]
mod tests;

pub use linked::{CliGh, Gh};

/// Where a tracker call runs: the engine's data directory, which keeps the trackers' connections,
/// and the checkout `gh` runs in.
pub type Place = onicode_core::inbox::Service;

/// The trackers behind the Inbox, the media they link to and the agents' processes. A seam, so
/// the tests script them with no network, process or `gh`.
pub trait Trackers: Send + Sync + 'static {
    fn list(&self, at: &Place, projects: &[PathBuf], state: &ViewState) -> Snapshot;
    fn details(&self, at: &Place, item: &Item) -> Result<Details, String>;
    fn thread(&self, at: &Place, item: &Item) -> Result<Thread, String>;
    fn diff(&self, at: &Place, item: &Item) -> Result<Diff, String>;
    fn checks(&self, at: &Place, item: &Item) -> Result<Checks, String>;
    fn comment(
        &self,
        at: &Place,
        item: &Item,
        body: String,
        reply_to: String,
    ) -> Result<String, String>;
    fn connect(
        &self,
        at: &Place,
        provider: &str,
        url: String,
        email: String,
        token: String,
    ) -> Result<Status, String>;
    fn action(&self, at: &Place, item: &Item, action: &str) -> Result<Item, String>;
    /// An image or video an item links to, as a PNG.
    fn media(&self, url: &str) -> Result<Vec<u8>, String>;
    /// A public avatar, as a PNG.
    fn avatar(&self, url: &str) -> Result<Vec<u8>, String>;
    /// Every live agent process and its tools, busiest first.
    fn processes(&self) -> Result<Vec<AgentProcess>, String>;
    /// Stops a tool an agent started, if `pid` is still the process `identity` names.
    fn stop_tool(&self, pid: u32, identity: &str) -> Result<(), String>;
}

/// Trackers that never reach a network: an empty Inbox, and every action refused. What a test
/// engine runs.
pub struct OfflineTrackers;

const OFFLINE: &str = "The Inbox is offline in this engine";

impl Trackers for OfflineTrackers {
    fn list(&self, _: &Place, _: &[PathBuf], _: &ViewState) -> Snapshot {
        Snapshot::default()
    }

    fn details(&self, _: &Place, _: &Item) -> Result<Details, String> {
        Err(OFFLINE.into())
    }

    fn thread(&self, _: &Place, _: &Item) -> Result<Thread, String> {
        Err(OFFLINE.into())
    }

    fn diff(&self, _: &Place, _: &Item) -> Result<Diff, String> {
        Err(OFFLINE.into())
    }

    fn checks(&self, _: &Place, _: &Item) -> Result<Checks, String> {
        Err(OFFLINE.into())
    }

    fn comment(&self, _: &Place, _: &Item, _: String, _: String) -> Result<String, String> {
        Err(OFFLINE.into())
    }

    fn connect(
        &self,
        _: &Place,
        _: &str,
        _: String,
        _: String,
        _: String,
    ) -> Result<Status, String> {
        Err(OFFLINE.into())
    }

    fn action(&self, _: &Place, _: &Item, _: &str) -> Result<Item, String> {
        Err(OFFLINE.into())
    }

    fn media(&self, _: &str) -> Result<Vec<u8>, String> {
        Err(OFFLINE.into())
    }

    fn avatar(&self, _: &str) -> Result<Vec<u8>, String> {
        Err(OFFLINE.into())
    }

    fn processes(&self) -> Result<Vec<AgentProcess>, String> {
        Ok(Vec::new())
    }

    fn stop_tool(&self, _: u32, _: &str) -> Result<(), String> {
        Err(OFFLINE.into())
    }
}

/// A `gh` that is never installed. What a test engine runs.
pub struct OfflineGh;

impl Gh for OfflineGh {
    fn run(&self, _: &[String]) -> Result<String, String> {
        Err("GitHub CLI not found".into())
    }
}

/// The trackers' own adapters, over the connections kept in the data directory.
pub struct CoreTrackers;

impl Trackers for CoreTrackers {
    fn list(&self, at: &Place, projects: &[PathBuf], state: &ViewState) -> Snapshot {
        at.list(projects, state)
    }

    fn details(&self, at: &Place, item: &Item) -> Result<Details, String> {
        at.details(item)
    }

    fn thread(&self, at: &Place, item: &Item) -> Result<Thread, String> {
        at.thread(item)
    }

    fn diff(&self, at: &Place, item: &Item) -> Result<Diff, String> {
        at.diff(item)
    }

    fn checks(&self, at: &Place, item: &Item) -> Result<Checks, String> {
        at.checks(item)
    }

    fn comment(
        &self,
        at: &Place,
        item: &Item,
        body: String,
        reply_to: String,
    ) -> Result<String, String> {
        at.comment(item, body, reply_to)
    }

    fn connect(
        &self,
        at: &Place,
        provider: &str,
        url: String,
        email: String,
        token: String,
    ) -> Result<Status, String> {
        at.connect(provider, url, email, token)
    }

    fn action(&self, at: &Place, item: &Item, action: &str) -> Result<Item, String> {
        at.action(item, action)
    }

    fn media(&self, url: &str) -> Result<Vec<u8>, String> {
        onicode_core::inbox::media_png(url)
    }

    fn avatar(&self, url: &str) -> Result<Vec<u8>, String> {
        onicode_core::inbox::avatar_png(url)
    }

    fn processes(&self) -> Result<Vec<AgentProcess>, String> {
        Ok(onicode_core::resources::processes()?
            .into_iter()
            .map(|process| AgentProcess {
                pid: process.pid,
                identity: process.identity,
                session: process.session,
                command: process.command,
                cpu_percent: process.cpu_percent,
                memory_bytes: process.memory_bytes,
                agent: process.agent,
            })
            .collect())
    }

    fn stop_tool(&self, pid: u32, identity: &str) -> Result<(), String> {
        onicode_core::resources::stop_tool(pid, identity)
    }
}

pub struct WorkHub {
    store: Database,
    workspace: Arc<WorkspaceHub>,
    trackers: Arc<dyn Trackers>,
    gh: Arc<dyn Gh>,
    spawn: Arc<dyn Spawn>,
    ledger: Ledger,
    /// The linked items' feeds last read.
    feeds: Mutex<linked::Feeds>,
}

impl WorkHub {
    #[must_use]
    pub fn new(store: Database, workspace: Arc<WorkspaceHub>, spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Self::with(
            store,
            workspace,
            Arc::new(CoreTrackers),
            Arc::new(CliGh),
            spawn,
        )
    }

    /// The hub over scripted trackers and `gh`.
    #[must_use]
    pub fn with(
        store: Database,
        workspace: Arc<WorkspaceHub>,
        trackers: Arc<dyn Trackers>,
        gh: Arc<dyn Gh>,
        spawn: Arc<dyn Spawn>,
    ) -> Arc<Self> {
        Arc::new(Self {
            ledger: Ledger::new(store.clone()),
            store,
            workspace,
            trackers,
            gh,
            spawn,
            feeds: Mutex::default(),
        })
    }

    fn feeds(&self) -> MutexGuard<'_, linked::Feeds> {
        self.feeds.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `work` on the hub's spawner and waits for it.
    async fn blocking<T, W>(&self, work: W) -> Result<T, String>
    where
        T: Send + 'static,
        W: FnOnce() -> Result<T, String> + Send + 'static,
    {
        let (reply, answer) = futures::channel::oneshot::channel();
        self.spawn.spawn(Box::pin(async move {
            let _ = reply.send(work());
        }));
        answer
            .await
            .map_err(|_| "The engine stopped before this call finished".to_owned())?
    }

    /// Where a tracker call for `cwd` runs. An engine in memory keeps the connections in `cwd`.
    fn place(&self, cwd: PathBuf) -> Result<Place, String> {
        let data_dir = locked(&self.store)?
            .data_dir()
            .map_or_else(|| cwd.clone(), Path::to_path_buf);
        Ok(Place { data_dir, cwd })
    }

    /// Runs a tracker call off the caller's thread.
    async fn tracker<T, W>(&self, cwd: PathBuf, work: W) -> Result<T, String>
    where
        T: Send + 'static,
        W: FnOnce(&dyn Trackers, &Place) -> Result<T, String> + Send + 'static,
    {
        let at = self.place(cwd)?;
        let trackers = self.trackers.clone();
        self.blocking(move || work(&*trackers, &at)).await
    }

    /// `InboxList`: every connected tracker's items, and the sessions they are matched against.
    pub async fn inbox_list(&self, body: InboxList) -> Result<InboxListing, String> {
        let InboxList {
            cwd,
            projects,
            state,
        } = body;
        let snapshot = self
            .tracker(cwd, move |trackers, at| {
                Ok(trackers.list(at, &projects, &state))
            })
            .await?;
        let sessions = locked(&self.store)?.sessions().unwrap_or_default();
        Ok(InboxListing {
            items: snapshot.items,
            errors: snapshot.errors,
            connections: snapshot.connections,
            linear_teams: snapshot.linear_teams,
            jira_projects: snapshot.jira_projects,
            sessions,
        })
    }

    pub async fn details(&self, body: ReadInboxDetails) -> Result<Details, String> {
        let item = body.item;
        self.tracker(body.cwd, move |trackers, at| trackers.details(at, &item))
            .await
    }

    pub async fn thread(&self, body: ReadInboxThread) -> Result<Thread, String> {
        let item = body.item;
        self.tracker(body.cwd, move |trackers, at| trackers.thread(at, &item))
            .await
    }

    pub async fn diff(&self, body: ReadInboxDiff) -> Result<Diff, String> {
        let item = body.item;
        self.tracker(body.cwd, move |trackers, at| trackers.diff(at, &item))
            .await
    }

    pub async fn checks(&self, body: ReadInboxChecks) -> Result<Checks, String> {
        let item = body.item;
        self.tracker(body.cwd, move |trackers, at| trackers.checks(at, &item))
            .await
    }

    /// `PostInboxComment`, once per op.
    pub async fn comment(
        &self,
        op: Option<OpId>,
        body: PostInboxComment,
    ) -> Result<String, String> {
        let PostInboxComment {
            cwd,
            item,
            body,
            reply_to,
        } = body;
        self.ledger
            .once(
                op,
                self.tracker(cwd, move |trackers, at| {
                    trackers.comment(at, &item, body, reply_to)
                }),
            )
            .await
    }

    /// `ConnectInbox`, once per op. The token stays with the tracker's adapter.
    pub async fn connect(&self, op: Option<OpId>, body: ConnectInbox) -> Result<Status, String> {
        let ConnectInbox {
            cwd,
            provider,
            url,
            email,
            token,
        } = body;
        self.ledger
            .once(
                op,
                self.tracker(cwd, move |trackers, at| {
                    trackers.connect(at, &provider, url, email, token)
                }),
            )
            .await
    }

    /// `RunInboxAction`, once per op.
    pub async fn action(&self, op: Option<OpId>, body: RunInboxAction) -> Result<Item, String> {
        let RunInboxAction { cwd, item, action } = body;
        self.ledger
            .once(
                op,
                self.tracker(cwd, move |trackers, at| trackers.action(at, &item, &action)),
            )
            .await
    }

    pub async fn media(&self, body: InboxMedia) -> Result<Bytes, String> {
        let trackers = self.trackers.clone();
        self.blocking(move || trackers.media(&body.url).map(Bytes))
            .await
    }

    pub async fn avatar(&self, body: InboxAvatar) -> Result<Bytes, String> {
        let trackers = self.trackers.clone();
        self.blocking(move || trackers.avatar(&body.url).map(Bytes))
            .await
    }

    pub async fn processes(&self) -> Result<Vec<AgentProcess>, String> {
        let trackers = self.trackers.clone();
        self.blocking(move || trackers.processes()).await
    }

    /// `StopAgentTool`, once per op.
    pub async fn stop_tool(
        &self,
        op: Option<OpId>,
        body: StopAgentTool,
    ) -> Result<Outcome, String> {
        let trackers = self.trackers.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    trackers.stop_tool(body.pid, &body.identity)?;
                    Ok(Outcome::Applied)
                }),
            )
            .await
    }

    pub fn automations(&self) -> Result<Vec<Automation>, String> {
        self.workspace.with_automations(|service| service.list())
    }

    pub fn automation_runs(&self, body: &AutomationRuns) -> Result<Vec<AutomationRun>, String> {
        self.workspace
            .with_automations(|service| service.runs(&body.automation))
    }

    /// `UpsertAutomation`, once per op.
    pub async fn upsert_automation(
        &self,
        op: Option<OpId>,
        body: UpsertAutomation,
    ) -> Result<Automation, String> {
        self.ledger
            .once(op, async move {
                self.workspace
                    .with_automations(|service| service.upsert(body.automation))
            })
            .await
    }

    /// `SetAutomationEnabled`, once per op.
    pub async fn set_automation_enabled(
        &self,
        op: Option<OpId>,
        body: SetAutomationEnabled,
    ) -> Result<Automation, String> {
        self.ledger
            .once(op, async move {
                self.workspace
                    .with_automations(|service| service.set_enabled(&body.automation, body.enabled))
            })
            .await
    }

    /// `RunAutomationNow`, once per op: the run, with the automation as it was listed before it.
    pub async fn run_automation_now(
        &self,
        op: Option<OpId>,
        body: RunAutomationNow,
    ) -> Result<DueAutomationRun, String> {
        self.ledger
            .once(op, async move {
                self.workspace.with_automations(|service| {
                    let automation = service
                        .list()?
                        .into_iter()
                        .find(|automation| automation.id == body.automation)
                        .ok_or_else(|| "Automation not found.".to_owned())?;
                    let now = i64::try_from(onicode_core::time::now_ms()).unwrap_or(i64::MAX);
                    let run = service.run_now(&automation.id, now)?;
                    Ok(DueAutomationRun { automation, run })
                })
            })
            .await
    }

    /// `DeleteAutomation`, once per op.
    pub async fn delete_automation(
        &self,
        op: Option<OpId>,
        body: DeleteAutomation,
    ) -> Result<Outcome, String> {
        self.ledger
            .once(op, async move {
                self.workspace
                    .with_automations(|service| service.delete(&body.automation))?;
                Ok(Outcome::Applied)
            })
            .await
    }

    pub fn has_event_automations(&self) -> Result<bool, String> {
        self.workspace.has_event_automations()
    }

    pub fn project_groups(&self, body: &ProjectGroups) -> Result<Groups, String> {
        locked(&self.store)?.groups(&body.project)
    }

    /// `LinkedActivity`: the cards, reading again only the feeds of items that changed.
    pub async fn linked_activity(&self, body: LinkedActivity) -> Result<Vec<LinkedCard>, String> {
        let previous = std::mem::take(&mut *self.feeds());
        let gh = self.gh.clone();
        let LinkedActivity { sessions, seen } = body;
        let (cards, feeds) = self
            .blocking(move || Ok(linked::fetch(&*gh, sessions, &seen, &previous)))
            .await?;
        *self.feeds() = feeds;
        Ok(cards)
    }

    /// `ReadWorkItem`.
    pub async fn work_item(&self, body: ReadWorkItem) -> Result<Item, String> {
        let ReadWorkItem {
            cwd,
            repo,
            kind,
            number,
        } = body;
        self.blocking(move || onicode_core::inbox::github_work_item(&cwd, &repo, &kind, number))
            .await
    }

    /// `ReadWorkItemDetails`.
    pub async fn work_item_details(&self, body: ReadWorkItemDetails) -> Result<Details, String> {
        let ReadWorkItemDetails {
            cwd,
            repo,
            kind,
            number,
        } = body;
        self.blocking(move || {
            onicode_core::inbox::github_work_item_details(&cwd, &repo, &kind, number)
        })
        .await
    }
}

/// The Work group as an `onicode-rpc` service.
pub struct WorkService {
    hub: Arc<WorkHub>,
}

impl WorkService {
    #[must_use]
    pub fn new(hub: Arc<WorkHub>) -> Self {
        Self { hub }
    }
}

const METHODS: [&str; 23] = [
    InboxList::NAME,
    ReadInboxDetails::NAME,
    ReadInboxThread::NAME,
    ReadInboxDiff::NAME,
    ReadInboxChecks::NAME,
    PostInboxComment::NAME,
    ConnectInbox::NAME,
    RunInboxAction::NAME,
    InboxMedia::NAME,
    InboxAvatar::NAME,
    ListAutomations::NAME,
    AutomationRuns::NAME,
    UpsertAutomation::NAME,
    SetAutomationEnabled::NAME,
    RunAutomationNow::NAME,
    DeleteAutomation::NAME,
    HasEventAutomations::NAME,
    ProjectGroups::NAME,
    LinkedActivity::NAME,
    AgentProcesses::NAME,
    StopAgentTool::NAME,
    ReadWorkItem::NAME,
    ReadWorkItemDetails::NAME,
];

impl Group for WorkService {
    fn serves(&self, method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait::async_trait]
impl RpcService for WorkService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = &self.hub;
        match method {
            InboxList::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.inbox_list(body).await)
            }
            ReadInboxDetails::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.details(body).await)
            }
            ReadInboxThread::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.thread(body).await)
            }
            ReadInboxDiff::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.diff(body).await)
            }
            ReadInboxChecks::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.checks(body).await)
            }
            PostInboxComment::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.comment(op, body).await)
            }
            ConnectInbox::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.connect(op, body).await)
            }
            RunInboxAction::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.action(op, body).await)
            }
            InboxMedia::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.media(body).await)
            }
            InboxAvatar::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.avatar(body).await)
            }
            ListAutomations::NAME => {
                let (_, ListAutomations {}) = routed(params)?;
                reply(hub.automations())
            }
            AutomationRuns::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.automation_runs(&body))
            }
            UpsertAutomation::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.upsert_automation(op, body).await)
            }
            SetAutomationEnabled::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.set_automation_enabled(op, body).await)
            }
            RunAutomationNow::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.run_automation_now(op, body).await)
            }
            DeleteAutomation::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.delete_automation(op, body).await)
            }
            HasEventAutomations::NAME => {
                let (_, HasEventAutomations {}) = routed(params)?;
                reply(hub.has_event_automations())
            }
            ProjectGroups::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.project_groups(&body))
            }
            LinkedActivity::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.linked_activity(body).await)
            }
            AgentProcesses::NAME => {
                let (_, AgentProcesses {}) = routed(params)?;
                reply(hub.processes().await)
            }
            StopAgentTool::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.stop_tool(op, body).await)
            }
            ReadWorkItem::NAME => reply(hub.work_item(routed(params)?.1).await),
            ReadWorkItemDetails::NAME => reply(hub.work_item_details(routed(params)?.1).await),
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
