//! The Work group's hub (`docs/onicode/decisions/engine.md`, migration step 6): the Inbox's
//! GitHub items, the automations of this engine's projects, which the workspace hub schedules
//! and starts, and the GitHub activity of the items sessions link to. Every network and `gh` call
//! runs here, off the caller's thread, behind the [`Trackers`] and [`Gh`] seams the tests script.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use onicode_core::automations::{Automation, AutomationRun, DueAutomationRun};
use onicode_core::inbox::{Checks, Details, Diff, Item, ProjectRepos, Snapshot, Thread, ViewState};
use onicode_protocol::files::Bytes;
use onicode_protocol::sessions::Groups;
use onicode_protocol::work::{
    AutomationRuns, DeleteAutomation, HasEventAutomations, InboxAvatar, InboxList, InboxListing,
    InboxMedia, InboxRepos, LinkedActivity, LinkedCard, ListAutomations, PostInboxComment,
    ProjectGroups, ReadInboxChecks, ReadInboxDetails, ReadInboxDiff, ReadInboxThread, ReadWorkItem,
    ReadWorkItemDetails, RunAutomationNow, RunInboxAction, SetAutomationEnabled, UpsertAutomation,
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

/// Where an Inbox call runs: the checkout `gh` runs in.
pub type Place = onicode_core::inbox::Service;

/// GitHub behind the Inbox and the media its items link to. A seam, so the tests script them
/// with no network or `gh`.
pub trait Trackers: Send + Sync + 'static {
    /// The items of `projects`, and of the projects other engines hold (`elsewhere`).
    fn list(
        &self,
        at: &Place,
        projects: &[PathBuf],
        elsewhere: &[ProjectRepos],
        state: &ViewState,
    ) -> Snapshot;
    /// The GitHub repositories the checkout at `project` belongs to.
    fn repos(&self, project: &Path) -> Result<Vec<String>, String>;
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
    fn action(&self, at: &Place, item: &Item, action: &str) -> Result<Item, String>;
    /// An image or video an item links to, as a PNG.
    fn media(&self, url: &str) -> Result<Vec<u8>, String>;
    /// A public avatar, as a PNG.
    fn avatar(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Trackers that never reach a network: an empty Inbox, and every action refused. What a test
/// engine runs.
pub struct OfflineTrackers;

const OFFLINE: &str = "The Inbox is offline in this engine";

impl Trackers for OfflineTrackers {
    fn list(&self, _: &Place, _: &[PathBuf], _: &[ProjectRepos], _: &ViewState) -> Snapshot {
        Snapshot::default()
    }

    fn repos(&self, _: &Path) -> Result<Vec<String>, String> {
        Err(OFFLINE.into())
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

    fn action(&self, _: &Place, _: &Item, _: &str) -> Result<Item, String> {
        Err(OFFLINE.into())
    }

    fn media(&self, _: &str) -> Result<Vec<u8>, String> {
        Err(OFFLINE.into())
    }

    fn avatar(&self, _: &str) -> Result<Vec<u8>, String> {
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

/// The Inbox's own GitHub adapter, over `gh`.
pub struct CoreTrackers;

impl Trackers for CoreTrackers {
    fn list(
        &self,
        at: &Place,
        projects: &[PathBuf],
        elsewhere: &[ProjectRepos],
        state: &ViewState,
    ) -> Snapshot {
        at.list(projects, elsewhere, state)
    }

    fn repos(&self, project: &Path) -> Result<Vec<String>, String> {
        onicode_core::inbox::repositories(project)
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

    fn action(&self, at: &Place, item: &Item, action: &str) -> Result<Item, String> {
        at.action(item, action)
    }

    fn media(&self, url: &str) -> Result<Vec<u8>, String> {
        onicode_core::inbox::media_png(url)
    }

    fn avatar(&self, url: &str) -> Result<Vec<u8>, String> {
        onicode_core::inbox::avatar_png(url)
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

    /// Runs an Inbox call for `cwd` off the caller's thread.
    async fn tracker<T, W>(&self, cwd: PathBuf, work: W) -> Result<T, String>
    where
        T: Send + 'static,
        W: FnOnce(&dyn Trackers, &Place) -> Result<T, String> + Send + 'static,
    {
        let at = Place { cwd };
        let trackers = self.trackers.clone();
        self.blocking(move || work(&*trackers, &at)).await
    }

    /// `InboxList`: GitHub's items, and the sessions they are matched against.
    pub async fn inbox_list(&self, body: InboxList) -> Result<InboxListing, String> {
        let InboxList {
            cwd,
            projects,
            elsewhere,
            state,
        } = body;
        let snapshot = self
            .tracker(cwd, move |trackers, at| {
                Ok(trackers.list(at, &projects, &elsewhere, &state))
            })
            .await?;
        let sessions = locked(&self.store)?.sessions().unwrap_or_default();
        Ok(InboxListing {
            items: snapshot.items,
            error: snapshot.error,
            connection: snapshot.connection,
            sessions,
        })
    }

    /// `InboxRepos`: what another engine's Inbox lists for a project this engine holds.
    pub async fn inbox_repos(&self, body: InboxRepos) -> Result<Vec<String>, String> {
        let trackers = self.trackers.clone();
        self.blocking(move || trackers.repos(&body.project)).await
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

    /// `RunAutomationNow`, once per op: starts a run in this engine, and answers it as it
    /// started (`running` in its session, or `failed` with why).
    pub async fn run_automation_now(
        &self,
        op: Option<OpId>,
        body: RunAutomationNow,
    ) -> Result<AutomationRun, String> {
        self.ledger
            .once(op, async move {
                let due = self.workspace.with_automations(|service| {
                    let automation = service
                        .list()?
                        .into_iter()
                        .find(|automation| automation.id == body.automation)
                        .ok_or_else(|| "Automation not found.".to_owned())?;
                    let now = i64::try_from(onicode_core::time::now_ms()).unwrap_or(i64::MAX);
                    let run = service.run_now(&automation.id, now)?;
                    Ok(DueAutomationRun { automation, run })
                })?;
                let (automation, run) = (due.automation.id.clone(), due.run.id.clone());
                self.workspace.start_runs(vec![due]);
                self.workspace
                    .with_automations(|service| service.runs(&automation))?
                    .into_iter()
                    .find(|started| started.id == run)
                    .ok_or_else(|| "Automation run not found.".to_owned())
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

const METHODS: [&str; 21] = [
    InboxList::NAME,
    InboxRepos::NAME,
    ReadInboxDetails::NAME,
    ReadInboxThread::NAME,
    ReadInboxDiff::NAME,
    ReadInboxChecks::NAME,
    PostInboxComment::NAME,
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
            InboxRepos::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.inbox_repos(body).await)
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
            ReadWorkItem::NAME => reply(hub.work_item(routed(params)?.1).await),
            ReadWorkItemDetails::NAME => reply(hub.work_item_details(routed(params)?.1).await),
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
