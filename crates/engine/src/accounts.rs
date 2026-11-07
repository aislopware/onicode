//! The Agents group (`docs/onicode/decisions/engine.md`, migration step 6): the provider accounts
//! the engine signs in with, what each provider's published door says of their sign-in, and the
//! usage their turns and polls report, served as `WatchAccounts` on one `Topic`; and the calls
//! behind the model pickers, the composer's skills and commands, and Settings → Providers, Skills
//! and MCP. Everything that asks a provider goes through an [`AgentProbe`], off the hub's lock.
//!
//! A mutating call runs once per op: a repeat gets the first reply from the `Ledger`.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use futures::stream::BoxStream;
use onicode_core::accounts::{AuthAction, Book, DEFAULT, Profile};
use onicode_core::catalog::Model;
use onicode_core::mcp::McpConnection;
use onicode_core::provider::{Kind, NativeCommand};
use onicode_core::session_title::TextModel;
use onicode_core::skills::DiscoveredSkill;
use onicode_core::usage;
use onicode_protocol::agents::{
    AccountsEvent, AccountsState, AddAccount, AddMcpServer, CancelLogin, Catalog, Commands,
    ConsumeUsageReset, CreateSkill, ForgetThread, Generate, Harness, HarnessUpdate, HarnessUpdates,
    HarnessVersion, Harnesses, InspectHarness, KeepIdleAgents, Login, Logout, McpHealth, McpLogin,
    McpServers, RefreshSignIn, RefreshUsage, RemoveAccount, RemoveMcpServer, RenameAccount,
    RunHarnessUpdate, SelectAccount, SignInEntry, Skills, StopGenerate, TransferThread, UsageEntry,
    UsageKey, UsageRead, WatchAccounts,
};
use onicode_protocol::{Call, Item, OpId, Outcome, Rejection, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService};

use crate::feed::{self, Feed, Limits, Pending, Topic};
use crate::ledger::Ledger;
use crate::service::{Group, reply, routed};
use crate::session::Spawn;
use crate::settings::{SettingsHub, json_items};
use crate::turn::{Database, locked};

mod probe;
#[cfg(test)]
mod tests;

pub use probe::{AgentProbe, CliProbe, OfflineProbe, parse_claude_status, parse_codex_account};

/// A polled usage read is fresh this long: a refresh within it reads nothing unless forced.
const USAGE_FRESH: Duration = Duration::from_secs(60);
/// Stops remembered for runs that have not started, at most.
const EARLY_STOPS: usize = 256;

pub(crate) struct AccountsFeed;

/// The billing provider whose usage a turn of `model` spends: Pi's names it first
/// (`pi:provider/model`); every other provider bills its own account.
fn billing(kind: Kind, model: Option<&str>) -> String {
    if kind != Kind::Pi {
        return String::new();
    }
    model
        .unwrap_or("")
        .trim_start_matches("pi:")
        .split('/')
        .next()
        .unwrap_or("")
        .to_owned()
}

/// What an event replaces: the book, one account's sign-in, or one key's usage.
fn target(event: &AccountsEvent) -> (u8, &'static str, &str, &str) {
    match event {
        AccountsEvent::Book(_) => (0, "", "", ""),
        AccountsEvent::SignIn(entry) => (1, entry.provider.id(), &entry.account, ""),
        AccountsEvent::Usage(entry) => (
            2,
            entry.key.provider.id(),
            &entry.key.account,
            &entry.key.billing,
        ),
    }
}

impl Feed for AccountsFeed {
    type Event = AccountsEvent;
    type State = AccountsState;

    fn apply(state: &mut AccountsState, event: &AccountsEvent) {
        state.apply(event);
    }

    fn weight(event: &AccountsEvent) -> usize {
        serde_json::to_vec(event).map_or(1024, |bytes| bytes.len())
    }

    /// Each event replaces what it names, so a newer one of the same target replaces a pending
    /// one.
    fn absorb(pending: &mut Pending<Self>, event: AccountsEvent) -> bool {
        pending.replace_or_append(event, |next, held| target(next) == target(held))
    }
}

/// Runs a stop can reach by id: a stop that comes first is kept until its run starts.
#[derive(Default)]
struct Stops {
    running: HashMap<String, Arc<AtomicBool>>,
    early: HashSet<String>,
}

impl Stops {
    fn start(&mut self, id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(self.early.remove(id)));
        self.running.insert(id.to_owned(), flag.clone());
        flag
    }

    fn stop(&mut self, id: &str) {
        match self.running.get(id) {
            Some(flag) => flag.store(true, Ordering::SeqCst),
            None => {
                if self.early.len() >= EARLY_STOPS {
                    self.early.clear();
                }
                self.early.insert(id.to_owned());
            }
        }
    }

    fn finish(&mut self, id: &str) {
        self.running.remove(id);
    }
}

#[derive(Default)]
struct Work {
    logins: Stops,
    generates: Stops,
    /// When each key's provider poll last ran.
    polled: HashMap<UsageKey, Instant>,
    /// The engine run's launch check for CLI updates ran.
    launch_checked: bool,
}

pub struct AccountsHub {
    store: Database,
    /// Where non-default accounts keep their profiles: the engine's data directory.
    root: Option<PathBuf>,
    settings: Arc<SettingsHub>,
    probe: Arc<dyn AgentProbe>,
    spawn: Arc<dyn Spawn>,
    ledger: Ledger,
    topic: Mutex<Topic<AccountsFeed>>,
    work: Mutex<Work>,
}

impl AccountsHub {
    pub fn new(
        store: Database,
        settings: Arc<SettingsHub>,
        probe: Arc<dyn AgentProbe>,
        spawn: Arc<dyn Spawn>,
    ) -> Arc<Self> {
        Self::with(store, settings, probe, spawn, Limits::default())
    }

    pub(crate) fn with(
        store: Database,
        settings: Arc<SettingsHub>,
        probe: Arc<dyn AgentProbe>,
        spawn: Arc<dyn Spawn>,
        limits: Limits,
    ) -> Arc<Self> {
        let (book, root) = locked(&store)
            .map(|store| {
                (
                    store.provider_accounts().unwrap_or_default(),
                    store.data_dir().map(std::path::Path::to_path_buf),
                )
            })
            .unwrap_or_default();
        let state = AccountsState {
            book,
            ..AccountsState::default()
        };
        let epoch = onicode_core::time::now_ms().wrapping_shl(16) | 5;
        Arc::new(Self {
            ledger: Ledger::new(store.clone()),
            store,
            root,
            settings,
            probe,
            spawn,
            topic: Mutex::new(Topic::new(epoch, state, limits)),
            work: Mutex::default(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Topic<AccountsFeed>> {
        self.topic.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn work(&self) -> MutexGuard<'_, Work> {
        self.work.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The accounts as they are now.
    #[must_use]
    pub fn state(&self) -> AccountsState {
        self.lock().state().clone()
    }

    /// The account `id` with its profile's home: what a turn, a probe or a chore runs on.
    #[must_use]
    pub fn profile(&self, id: &str) -> Profile {
        match &self.root {
            Some(root) => Profile::new(id, root),
            None => Profile {
                id: id.to_owned(),
                root: None,
            },
        }
    }

    /// Whether `account` of `kind` is one the engine knows: the default, or one in the book.
    fn known(&self, kind: Kind, account: &str) -> bool {
        account == DEFAULT || self.lock().state().book.exists(kind, account)
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

    /// `WatchAccounts`.
    pub fn watch(
        self: &Arc<Self>,
        params: &WatchAccounts,
    ) -> BoxStream<'static, Item<AccountsState, AccountsEvent>> {
        let (id, notify) = self.lock().join(params.since);
        let (draining, leaving) = (self.clone(), self.clone());
        feed::stream(
            notify,
            move || draining.lock().drain(id),
            move || {
                leaving.lock().leave(id);
            },
        )
    }

    fn publish(&self, event: AccountsEvent) {
        self.lock().publish(vec![event]);
    }

    /// Changes the book with `edit`, saves it and publishes it; `edit`'s error leaves it as it
    /// was.
    fn edit_book<R>(&self, edit: impl FnOnce(&mut Book) -> Result<R, String>) -> Result<R, String> {
        let mut topic = self.lock();
        let mut book = topic.state().book.clone();
        let result = edit(&mut book)?;
        locked(&self.store)?.save_provider_accounts(&book)?;
        topic.publish(vec![AccountsEvent::Book(book)]);
        Ok(result)
    }

    /// `RefreshUsage`.
    pub async fn refresh_usage(&self, body: RefreshUsage) -> Result<Outcome, String> {
        let RefreshUsage { key, force } = body;
        if !self.known(key.provider, &key.account) {
            return Ok(Outcome::Rejected(Rejection::Gone));
        }
        if !usage::from_turns(key.provider) {
            let mut work = self.work();
            if !force
                && work
                    .polled
                    .get(&key)
                    .is_some_and(|at| at.elapsed() < USAGE_FRESH)
            {
                return Ok(Outcome::Applied);
            }
            work.polled.insert(key.clone(), Instant::now());
        }
        self.read_usage(key).await
    }

    async fn read_usage(&self, key: UsageKey) -> Result<Outcome, String> {
        let (probe, profile) = (self.probe.clone(), self.profile(&key.account));
        let (kind, billing) = (key.provider, key.billing.clone());
        let usage = self
            .blocking(move || Ok(probe.usage(kind, &profile, &billing)))
            .await?;
        self.publish(AccountsEvent::Usage(UsageEntry { key, usage }));
        Ok(Outcome::Applied)
    }

    /// A turn of `account` on `kind` ended: what its stream reported of the account's usage.
    pub fn turn_ended(self: &Arc<Self>, kind: Kind, account: &str) {
        if !usage::from_turns(kind) || !self.known(kind, account) {
            return;
        }
        let hub = self.clone();
        let key = UsageKey {
            provider: kind,
            account: account.to_owned(),
            billing: String::new(),
        };
        self.spawn.spawn(Box::pin(async move {
            let _ = hub.read_usage(key).await;
        }));
    }

    /// When every exhausted window of `account` on `kind` resets, as its last usage read says:
    /// what an armed usage limit waits for when the provider named no reset. When that is not
    /// known yet, the usage is read again (a polled provider at most once a minute).
    pub fn exhausted_reset(
        self: &Arc<Self>,
        kind: Kind,
        account: &str,
        model: Option<&str>,
    ) -> Option<i64> {
        let key = UsageKey {
            provider: kind,
            account: account.to_owned(),
            billing: billing(kind, model),
        };
        let known = self
            .lock()
            .state()
            .usage
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| match &entry.usage {
                UsageRead::Snapshot(snapshot) => snapshot.exhausted_reset(),
                _ => None,
            });
        if known.is_none() && self.known(kind, account) {
            let hub = self.clone();
            self.spawn.spawn(Box::pin(async move {
                let _ = hub.refresh_usage(RefreshUsage { key, force: false }).await;
            }));
        }
        known
    }

    /// `RefreshSignIn`.
    pub async fn refresh_sign_in(&self, body: RefreshSignIn) -> Result<Outcome, String> {
        let RefreshSignIn { provider, account } = body;
        if !self.known(provider, &account) {
            return Ok(Outcome::Rejected(Rejection::Gone));
        }
        let (probe, profile) = (self.probe.clone(), self.profile(&account));
        let sign_in = self
            .blocking(move || Ok(probe.sign_in(provider, &profile)))
            .await?;
        self.publish(AccountsEvent::SignIn(SignInEntry {
            provider,
            account,
            sign_in,
        }));
        Ok(Outcome::Applied)
    }

    /// What an account's sign-in or sign-out changed: who it is and its usage, read again.
    async fn signed(&self, provider: Kind, account: &str) {
        let _ = self
            .refresh_sign_in(RefreshSignIn {
                provider,
                account: account.to_owned(),
            })
            .await;
        let key = UsageKey {
            provider,
            account: account.to_owned(),
            billing: String::new(),
        };
        if !usage::from_turns(provider) {
            self.work().polled.insert(key.clone(), Instant::now());
        }
        let _ = self.read_usage(key).await;
    }

    /// `ConsumeUsageReset`: spends the credit, then reads the account's usage again.
    pub async fn consume_reset(
        &self,
        op: Option<OpId>,
        body: ConsumeUsageReset,
    ) -> Result<String, String> {
        self.ledger
            .once(op, async {
                let ConsumeUsageReset { account, credit } = body;
                if !self.known(Kind::Codex, &account) {
                    return Err("Provider account no longer exists".into());
                }
                let (probe, profile) = (self.probe.clone(), self.profile(&account));
                let outcome = self
                    .blocking(move || probe.consume_reset(&profile, credit.as_deref()))
                    .await?;
                let key = UsageKey {
                    provider: Kind::Codex,
                    account,
                    billing: String::new(),
                };
                self.work().polled.insert(key.clone(), Instant::now());
                let _ = self.read_usage(key).await;
                Ok(outcome)
            })
            .await
    }

    /// `Catalog`.
    pub async fn catalog(&self, body: Catalog) -> Result<Option<Vec<Model>>, String> {
        let Catalog {
            provider,
            account,
            read,
        } = body;
        let (probe, profile) = (self.probe.clone(), self.profile(&account));
        self.blocking(move || probe.catalog(provider, &profile, read))
            .await
    }

    /// `Skills`: with `all` unset, the skills Settings turned off are left out.
    pub async fn skills(&self, body: Skills) -> Result<Vec<DiscoveredSkill>, String> {
        let disabled = (!body.all).then(|| self.settings.current().disabled_skill_paths);
        let probe = self.probe.clone();
        self.blocking(move || probe.skills(&body.project, disabled))
            .await
    }

    /// `CreateSkill`.
    pub async fn create_skill(
        &self,
        op: Option<OpId>,
        body: CreateSkill,
    ) -> Result<String, String> {
        let probe = self.probe.clone();
        self.ledger
            .once(op, async {
                self.blocking(move || probe.create_skill(&body.project, &body.name, body.personal))
                    .await
            })
            .await
    }

    /// `Commands`.
    pub async fn commands(&self, body: Commands) -> Result<Vec<NativeCommand>, String> {
        let Commands {
            provider,
            project,
            account,
            model,
        } = body;
        let (probe, profile) = (self.probe.clone(), self.profile(&account));
        self.blocking(move || probe.commands(provider, &project, &profile, model))
            .await
    }

    /// `AddAccount`.
    pub async fn add_account(
        &self,
        op: Option<OpId>,
        body: AddAccount,
    ) -> Result<onicode_core::accounts::Account, String> {
        self.ledger
            .once(op, async {
                self.edit_book(|book| book.add(body.provider, &body.label))
            })
            .await
    }

    /// `RenameAccount`.
    pub async fn rename_account(
        &self,
        op: Option<OpId>,
        body: RenameAccount,
    ) -> Result<Outcome, String> {
        self.ledger
            .once(op, async {
                let RenameAccount {
                    provider,
                    account,
                    label,
                } = body;
                if !self.known(provider, &account) {
                    return Ok(Outcome::Rejected(Rejection::Gone));
                }
                Ok(
                    match self.edit_book(|book| book.rename(provider, &account, &label)) {
                        Ok(()) => Outcome::Applied,
                        Err(error) => Outcome::Rejected(Rejection::Denied(error)),
                    },
                )
            })
            .await
    }

    /// `RemoveAccount`: its agents stop, the provider signs it out, its profile goes, then it
    /// leaves the book. A profile that cannot be deleted keeps the account.
    pub async fn remove_account(
        &self,
        op: Option<OpId>,
        body: RemoveAccount,
    ) -> Result<Outcome, String> {
        self.ledger
            .once(op, async {
                let RemoveAccount { provider, account } = body;
                if account == DEFAULT {
                    return Ok(Outcome::Rejected(Rejection::Denied(
                        "This account cannot be removed".into(),
                    )));
                }
                if !self.known(provider, &account) {
                    return Ok(Outcome::Rejected(Rejection::Gone));
                }
                let (probe, profile) = (self.probe.clone(), self.profile(&account));
                self.blocking(move || probe.remove(provider, &profile))
                    .await?;
                Ok(
                    match self.edit_book(|book| book.remove(provider, &account)) {
                        Ok(()) => Outcome::Applied,
                        Err(_) => Outcome::Rejected(Rejection::Gone),
                    },
                )
            })
            .await
    }

    /// `SelectAccount`.
    pub async fn select_account(
        &self,
        op: Option<OpId>,
        body: SelectAccount,
    ) -> Result<Outcome, String> {
        self.ledger
            .once(op, async {
                let SelectAccount {
                    provider,
                    project,
                    account,
                } = body;
                Ok(
                    match self.edit_book(|book| book.select(provider, &project, &account)) {
                        Ok(()) => Outcome::Applied,
                        Err(_) => Outcome::Rejected(Rejection::Gone),
                    },
                )
            })
            .await
    }

    /// Runs the provider CLI's sign-in or sign-out under `attempt`, then reads the account
    /// again. A failure (or a cancel) is the call's error, so a retry of its op runs again.
    async fn authenticate(
        &self,
        provider: Kind,
        account: String,
        action: AuthAction,
        attempt: Option<String>,
    ) -> Result<Outcome, String> {
        if !self.known(provider, &account) {
            return Ok(Outcome::Rejected(Rejection::Gone));
        }
        let cancel = match &attempt {
            Some(attempt) => self.work().logins.start(attempt),
            None => Arc::new(AtomicBool::new(false)),
        };
        let (probe, profile) = (self.probe.clone(), self.profile(&account));
        let result = self
            .blocking(move || probe.authenticate(provider, &profile, action, cancel))
            .await;
        if let Some(attempt) = &attempt {
            self.work().logins.finish(attempt);
        }
        result?;
        self.signed(provider, &account).await;
        Ok(Outcome::Applied)
    }

    /// `Login`.
    pub async fn login(&self, op: Option<OpId>, body: Login) -> Result<Outcome, String> {
        self.ledger
            .once(op, async {
                let Login {
                    provider,
                    account,
                    attempt,
                } = body;
                self.authenticate(provider, account, AuthAction::SignIn, Some(attempt))
                    .await
            })
            .await
    }

    /// `CancelLogin`.
    pub fn cancel_login(&self, body: &CancelLogin) -> Outcome {
        self.work().logins.stop(&body.attempt);
        Outcome::Applied
    }

    /// `Logout`.
    pub async fn logout(&self, op: Option<OpId>, body: Logout) -> Result<Outcome, String> {
        self.ledger
            .once(op, async {
                self.authenticate(body.provider, body.account, AuthAction::SignOut, None)
                    .await
            })
            .await
    }

    /// `McpServers`.
    pub async fn mcp_servers(&self, body: McpServers) -> Result<Vec<McpConnection>, String> {
        let probe = self.probe.clone();
        self.blocking(move || probe.mcp_servers(&body.project))
            .await
    }

    /// `AddMcpServer`.
    pub async fn add_mcp_server(
        &self,
        op: Option<OpId>,
        body: AddMcpServer,
    ) -> Result<Outcome, String> {
        let probe = self.probe.clone();
        self.ledger
            .once(op, async {
                self.blocking(move || probe.add_mcp_server(body))
                    .await
                    .map(|()| Outcome::Applied)
            })
            .await
    }

    /// `RemoveMcpServer`.
    pub async fn remove_mcp_server(
        &self,
        op: Option<OpId>,
        body: RemoveMcpServer,
    ) -> Result<Outcome, String> {
        let probe = self.probe.clone();
        self.ledger
            .once(op, async {
                self.blocking(move || {
                    probe.remove_mcp_server(&body.project, &body.name, &body.scope)
                })
                .await
                .map(|()| Outcome::Applied)
            })
            .await
    }

    /// `McpLogin`.
    pub async fn mcp_login(&self, op: Option<OpId>, body: McpLogin) -> Result<Outcome, String> {
        let probe = self.probe.clone();
        self.ledger
            .once(op, async {
                self.blocking(move || probe.mcp_login(&body.project, &body.provider, &body.name))
                    .await
                    .map(|()| Outcome::Applied)
            })
            .await
    }

    /// `McpHealth`.
    pub async fn mcp_health(&self, body: McpHealth) -> Result<String, String> {
        let probe = self.probe.clone();
        self.blocking(move || probe.mcp_health(&body.project)).await
    }

    /// `Harnesses`.
    pub async fn harnesses(&self) -> Result<Vec<Harness>, String> {
        let probe = self.probe.clone();
        self.blocking(move || {
            Ok(Kind::ALL
                .into_iter()
                .map(|provider| Harness {
                    provider,
                    path: probe.harness(provider),
                })
                .collect())
        })
        .await
    }

    /// `InspectHarness`.
    pub async fn inspect_harness(&self, body: InspectHarness) -> Result<HarnessVersion, String> {
        let probe = self.probe.clone();
        self.blocking(move || probe.inspect(body.provider, body.path.as_deref()))
            .await
    }

    /// `HarnessUpdates`: with `launch`, only the engine run's first check reads.
    pub async fn harness_updates(
        &self,
        body: HarnessUpdates,
    ) -> Result<Vec<HarnessUpdate>, String> {
        if body.launch && std::mem::replace(&mut self.work().launch_checked, true) {
            return Ok(Vec::new());
        }
        let hidden = self.settings.current().hidden_providers;
        let probe = self.probe.clone();
        self.blocking(move || Ok(probe.harness_updates(&hidden)))
            .await
    }

    /// `RunHarnessUpdate`.
    pub async fn run_harness_update(
        &self,
        op: Option<OpId>,
        body: RunHarnessUpdate,
    ) -> Result<String, String> {
        let probe = self.probe.clone();
        self.ledger
            .once(op, async {
                self.blocking(move || probe.run_harness_update(&body.update))
                    .await
            })
            .await
    }

    /// The model a [`Generate`] runs on: the one asked for, else the background model Settings
    /// picked, if its CLI is installed; else `fallback`, Claude Code, then Codex, on their
    /// default models.
    fn text_model(&self, asked: Option<TextModel>, fallback: Option<Kind>) -> Option<TextModel> {
        let chosen = asked.or_else(|| {
            let choice = self.settings.current().background_model?;
            Some(TextModel {
                kind: Kind::parse(&choice.provider)?,
                model: choice.model,
                account: Profile::default(),
            })
        });
        let mut model = chosen
            .filter(|model| self.probe.harness(model.kind).is_some())
            .or_else(|| {
                fallback
                    .into_iter()
                    .chain([Kind::Claude, Kind::Codex])
                    .find(|kind| self.probe.harness(*kind).is_some())
                    .map(|kind| TextModel {
                        kind,
                        model: None,
                        account: Profile::default(),
                    })
            })?;
        model.account = self.profile(&model.account.id);
        Some(model)
    }

    /// `Generate`.
    pub async fn generate(&self, body: Generate) -> Result<String, String> {
        let Generate {
            request,
            cwd,
            prompt,
            model,
            fallback,
        } = body;
        let cancel = self.work().generates.start(&request);
        let result = match self.text_model(model, fallback) {
            Some(model) => {
                let probe = self.probe.clone();
                self.blocking(move || probe.generate(&model, &cwd, prompt, cancel))
                    .await
            }
            None => Err("Install Claude Code or Codex to write this text.".into()),
        };
        self.work().generates.finish(&request);
        result
    }

    /// `StopGenerate`.
    pub fn stop_generate(&self, body: &StopGenerate) -> Outcome {
        self.work().generates.stop(&body.request);
        Outcome::Applied
    }

    /// `KeepIdleAgents`.
    pub fn keep_idle_agents(&self, body: KeepIdleAgents) -> Outcome {
        self.probe.keep_idle_agents(body.rows);
        Outcome::Applied
    }

    /// `TransferThread`.
    pub async fn transfer_thread(&self, body: TransferThread) -> Result<bool, String> {
        let TransferThread {
            provider,
            from,
            to,
            thread,
        } = body;
        let (probe, from, to) = (self.probe.clone(), self.profile(&from), self.profile(&to));
        self.blocking(move || Ok(probe.transfer_thread(provider, &from, &to, &thread).is_ok()))
            .await
    }

    /// `ForgetThread`.
    pub fn forget_thread(&self, body: &ForgetThread) -> Outcome {
        self.probe
            .forget_thread(body.provider, &body.thread, &body.account, &body.cwd);
        Outcome::Applied
    }
}

/// The Agents group as an `onicode-rpc` service.
pub struct AccountsService {
    hub: Arc<AccountsHub>,
}

impl AccountsService {
    #[must_use]
    pub fn new(hub: Arc<AccountsHub>) -> Self {
        Self { hub }
    }
}

const METHODS: [&str; 29] = [
    WatchAccounts::NAME,
    RefreshUsage::NAME,
    RefreshSignIn::NAME,
    ConsumeUsageReset::NAME,
    Catalog::NAME,
    Skills::NAME,
    CreateSkill::NAME,
    Commands::NAME,
    AddAccount::NAME,
    RenameAccount::NAME,
    RemoveAccount::NAME,
    SelectAccount::NAME,
    Login::NAME,
    CancelLogin::NAME,
    Logout::NAME,
    McpServers::NAME,
    AddMcpServer::NAME,
    RemoveMcpServer::NAME,
    McpLogin::NAME,
    McpHealth::NAME,
    Harnesses::NAME,
    InspectHarness::NAME,
    HarnessUpdates::NAME,
    RunHarnessUpdate::NAME,
    Generate::NAME,
    StopGenerate::NAME,
    KeepIdleAgents::NAME,
    TransferThread::NAME,
    ForgetThread::NAME,
];

impl Group for AccountsService {
    fn serves(&self, method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait::async_trait]
impl RpcService for AccountsService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = &self.hub;
        match method {
            WatchAccounts::NAME => {
                let (_, body) = routed::<WatchAccounts>(params)?;
                Ok(RpcReply::Stream(json_items(hub.watch(&body))))
            }
            RefreshUsage::NAME => reply(hub.refresh_usage(routed(params)?.1).await),
            RefreshSignIn::NAME => reply(hub.refresh_sign_in(routed(params)?.1).await),
            ConsumeUsageReset::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.consume_reset(op, body).await)
            }
            Catalog::NAME => reply(hub.catalog(routed(params)?.1).await),
            Skills::NAME => reply(hub.skills(routed(params)?.1).await),
            CreateSkill::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.create_skill(op, body).await)
            }
            Commands::NAME => reply(hub.commands(routed(params)?.1).await),
            AddAccount::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.add_account(op, body).await)
            }
            RenameAccount::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.rename_account(op, body).await)
            }
            RemoveAccount::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.remove_account(op, body).await)
            }
            SelectAccount::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.select_account(op, body).await)
            }
            Login::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.login(op, body).await)
            }
            CancelLogin::NAME => {
                let (_, body) = routed::<CancelLogin>(params)?;
                reply(Ok(hub.cancel_login(&body)))
            }
            Logout::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.logout(op, body).await)
            }
            McpServers::NAME => reply(hub.mcp_servers(routed(params)?.1).await),
            AddMcpServer::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.add_mcp_server(op, body).await)
            }
            RemoveMcpServer::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.remove_mcp_server(op, body).await)
            }
            McpLogin::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.mcp_login(op, body).await)
            }
            McpHealth::NAME => reply(hub.mcp_health(routed(params)?.1).await),
            Harnesses::NAME => {
                let (_, Harnesses {}) = routed(params)?;
                reply(hub.harnesses().await)
            }
            InspectHarness::NAME => reply(hub.inspect_harness(routed(params)?.1).await),
            HarnessUpdates::NAME => reply(hub.harness_updates(routed(params)?.1).await),
            RunHarnessUpdate::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.run_harness_update(op, body).await)
            }
            Generate::NAME => reply(hub.generate(routed(params)?.1).await),
            StopGenerate::NAME => {
                let (_, body) = routed::<StopGenerate>(params)?;
                reply(Ok(hub.stop_generate(&body)))
            }
            KeepIdleAgents::NAME => reply(Ok(hub.keep_idle_agents(routed(params)?.1))),
            TransferThread::NAME => reply(hub.transfer_thread(routed(params)?.1).await),
            ForgetThread::NAME => {
                let (_, body) = routed::<ForgetThread>(params)?;
                reply(Ok(hub.forget_thread(&body)))
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
