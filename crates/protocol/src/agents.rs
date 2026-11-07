//! The Agents group: the provider accounts an engine signs in with, their sign-in state and the
//! usage their streams and polls report ([`WatchAccounts`]), and the calls behind the model
//! pickers, the composer's skills and commands, and Settings → Providers, Skills and MCP.
//!
//! Sign-in state comes only through each provider's published door (`claude auth status --json`,
//! Codex's app-server `account/read`): the engine never reads a tool's credential files or
//! tokens. The mutating calls travel with an [`OpId`](crate::OpId) in their
//! [`Routed`](crate::Routed), and a repeat of the op is answered with the first reply.
use serde::{Deserialize, Serialize};

use crate::accounts::{Account, Book, Identity};
use crate::catalog::Model;
use crate::mcp::McpConnection;
use crate::provider::{Kind, NativeCommand};
use crate::sessions::TextModel;
use crate::skills::DiscoveredSkill;
use crate::usage::{FetchError, Snapshot};
use crate::{Call, Cursor, Outcome, Watch};

/// What a provider's published door says about one account's sign-in.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SignIn {
    /// `None` until the engine has asked the provider, or when the provider has no door to ask.
    pub signed_in: Option<bool>,
    /// Who is signed in, as far as the provider says.
    pub identity: Identity,
    /// How the account signed in ("Claude account", "OAuth token", "API key"), when the provider
    /// says.
    pub method: Option<String>,
}

/// One account's sign-in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SignInEntry {
    pub provider: Kind,
    pub account: String,
    pub sign_in: SignIn,
}

/// Whose usage: an account, and for Pi the billing provider its model runs on (empty otherwise).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UsageKey {
    pub provider: Kind,
    pub account: String,
    pub billing: String,
}

impl Ord for UsageKey {
    /// By provider id, account, then billing provider.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.provider.id(), &self.account, &self.billing).cmp(&(
            other.provider.id(),
            &other.account,
            &other.billing,
        ))
    }
}

impl PartialOrd for UsageKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// What the engine last learned of an account's usage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum UsageRead {
    Snapshot(Snapshot),
    Failed(FetchError),
    /// Nothing to show yet: the account's turns have reported no usage since it signed in or out
    /// (Claude Code publishes no poll).
    Unreported,
}

/// One account's usage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageEntry {
    pub key: UsageKey,
    pub usage: UsageRead,
}

/// The accounts as of a [`Cursor`]: what a watcher starts from.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountsState {
    /// Every account and which one each project uses.
    pub book: Book,
    /// By provider, then account.
    pub sign_ins: Vec<SignInEntry>,
    /// By key.
    pub usage: Vec<UsageEntry>,
}

/// One change to the accounts, in the order the engine made it. Each replaces what it names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AccountsEvent {
    Book(Book),
    SignIn(SignInEntry),
    Usage(UsageEntry),
}

impl AccountsState {
    /// Applies `event`, on the host and in every viewer's mirror alike.
    pub fn apply(&mut self, event: &AccountsEvent) {
        match event {
            AccountsEvent::Book(book) => self.book = book.clone(),
            AccountsEvent::SignIn(entry) => {
                let key = |held: &SignInEntry| (held.provider.id(), held.account.clone());
                match self
                    .sign_ins
                    .binary_search_by(|held| key(held).cmp(&key(entry)))
                {
                    Ok(at) => self.sign_ins[at] = entry.clone(),
                    Err(at) => self.sign_ins.insert(at, entry.clone()),
                }
            }
            AccountsEvent::Usage(entry) => {
                match self.usage.binary_search_by(|held| held.key.cmp(&entry.key)) {
                    Ok(at) => self.usage[at] = entry.clone(),
                    Err(at) => self.usage.insert(at, entry.clone()),
                }
            }
        }
    }

    /// The sign-in of `account` of `provider`, if the engine has one.
    #[must_use]
    pub fn sign_in(&self, provider: Kind, account: &str) -> Option<&SignIn> {
        self.sign_ins
            .iter()
            .find(|entry| entry.provider == provider && entry.account == account)
            .map(|entry| &entry.sign_in)
    }

    /// The usage of `key`, if the engine has read any.
    #[must_use]
    pub fn usage(&self, key: &UsageKey) -> Option<&UsageRead> {
        self.usage
            .iter()
            .find(|entry| &entry.key == key)
            .map(|entry| &entry.usage)
    }
}

/// Watch the accounts: a snapshot (or, with a `since` the engine's ring still covers, only the
/// events after it), then every event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchAccounts {
    pub since: Option<Cursor>,
}

impl Watch for WatchAccounts {
    type Event = AccountsEvent;
    type Params = Self;
    type State = AccountsState;

    const NAME: &'static str = "WatchAccounts";
}

macro_rules! agents_call {
    ($name:ident,reply: $reply:ty) => {
        impl Call for $name {
            type Params = Self;
            type Reply = $reply;

            const NAME: &'static str = stringify!($name);
        }
    };
}

/// Read an account's usage again: the engine reads what the account's turns reported (Claude
/// Code), or polls the provider's published door (Codex's app-server), unless it read one within
/// the last minute and `force` is unset. What it read arrives as [`AccountsEvent::Usage`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshUsage {
    pub key: UsageKey,
    pub force: bool,
}
agents_call!(RefreshUsage, reply: Outcome);

/// Ask the provider's published door who `account` is signed in as. What it said arrives as
/// [`AccountsEvent::SignIn`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshSignIn {
    pub provider: Kind,
    pub account: String,
}
agents_call!(RefreshSignIn, reply: Outcome);

/// Spend one of a Codex account's usage reset credits (`credit`, or any). Replies with Codex's
/// outcome: `reset`, `nothingToReset`, `noCredit` or `alreadyRedeemed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumeUsageReset {
    pub account: String,
    pub credit: Option<String>,
}
agents_call!(ConsumeUsageReset, reply: String);

/// How fresh a [`Catalog`] must be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogRead {
    /// Only what the engine holds; `None` when it holds nothing. Never asks the provider.
    Cached,
    /// What the engine holds while it is fresh, else the provider's list.
    Fresh,
    /// The provider's list, asked again: after a sign-in or a CLI update.
    Reload,
}

/// The models `provider` offers `account`. A model whose limits are missing or malformed is still
/// listed, its [`ModelLimits`](crate::catalog::ModelLimits) empty field by field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub provider: Kind,
    pub account: String,
    pub read: CatalogRead,
}
agents_call!(Catalog, reply: Option<Vec<Model>>);

/// The skills a session in `project` can use: personal and project skills of every provider.
/// With `all` unset, the paths Settings disabled are left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skills {
    pub project: String,
    pub all: bool,
}
agents_call!(Skills, reply: Vec<DiscoveredSkill>);

/// Create a blank skill named `name` in `project`, or the person's own skills with `personal`.
/// Replies with the new skill's file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateSkill {
    pub project: String,
    pub name: String,
    pub personal: bool,
}
agents_call!(CreateSkill, reply: String);

/// The commands `provider` offers in `project` that it announces only when asked (the Pi
/// family's); the others' arrive with their turns as `SessionEvent::Commands`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commands {
    pub provider: Kind,
    pub project: String,
    pub account: String,
    pub model: Option<String>,
}
agents_call!(Commands, reply: Vec<NativeCommand>);

/// Add an account to `provider`, named `label`. Replies with the new account; its sign-in is the
/// next [`Login`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddAccount {
    pub provider: Kind,
    pub label: String,
}
agents_call!(AddAccount, reply: Account);

/// Rename an account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameAccount {
    pub provider: Kind,
    pub account: String,
    pub label: String,
}
agents_call!(RenameAccount, reply: Outcome);

/// Remove an account: its agents stop, the provider signs it out through its own door, and the
/// account leaves the book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveAccount {
    pub provider: Kind,
    pub account: String,
}
agents_call!(RemoveAccount, reply: Outcome);

/// Make `account` the one `provider` runs on in `project`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectAccount {
    pub provider: Kind,
    pub project: String,
    pub account: String,
}
agents_call!(SelectAccount, reply: Outcome);

/// Sign `account` in through the provider's CLI, which opens the browser and keeps the
/// credentials itself. Replies when the CLI finishes, fails, times out (10 minutes) or is stopped
/// with [`CancelLogin`] under the same `attempt`, a viewer-minted id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    pub provider: Kind,
    pub account: String,
    pub attempt: String,
}
agents_call!(Login, reply: Outcome);

/// Stop the [`Login`] running under `attempt`. A cancel that arrives first stops it as it starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelLogin {
    pub attempt: String,
}
agents_call!(CancelLogin, reply: Outcome);

/// Sign `account` out through the provider's CLI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Logout {
    pub provider: Kind,
    pub account: String,
}
agents_call!(Logout, reply: Outcome);

/// The MCP servers every provider configures for `project`, personal and project ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServers {
    pub project: String,
}
agents_call!(McpServers, reply: Vec<McpConnection>);

/// Add an MCP server to `provider`'s configuration, from the JSON `config` Settings took.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddMcpServer {
    pub project: String,
    pub provider: String,
    pub scope: String,
    pub name: String,
    pub config: String,
}
agents_call!(AddMcpServer, reply: Outcome);

/// Remove an MCP server from the configuration that holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveMcpServer {
    pub project: String,
    pub name: String,
    pub scope: String,
}
agents_call!(RemoveMcpServer, reply: Outcome);

/// Sign in to an MCP server through `provider`'s own flow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpLogin {
    pub project: String,
    pub provider: String,
    pub name: String,
}
agents_call!(McpLogin, reply: Outcome);

/// Claude Code's own health report of its MCP servers in `project` (`claude mcp list`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpHealth {
    pub project: String,
}
agents_call!(McpHealth, reply: String);

/// One provider's CLI as the engine resolves it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Harness {
    pub provider: Kind,
    /// `None` when the engine finds no CLI.
    pub path: Option<String>,
}

/// Every provider's CLI, as the engine resolves it with the paths Settings set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Harnesses {}
agents_call!(Harnesses, reply: Vec<Harness>);

/// A CLI's path and the version it reports.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessVersion {
    pub path: String,
    pub version: String,
}

/// Check the CLI at `path` (absolute), or the one the engine resolves for `provider`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectHarness {
    pub provider: Kind,
    pub path: Option<String>,
}
agents_call!(InspectHarness, reply: HarnessVersion);

/// A provider CLI with a newer release than the one installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessUpdate {
    pub harness: Kind,
    pub installed: String,
    pub latest: String,
}

/// The provider CLIs with newer releases, hidden providers left out. With `launch`, only the
/// engine run's first such check answers; later ones reply with none, so one launch notices once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessUpdates {
    pub launch: bool,
}
agents_call!(HarnessUpdates, reply: Vec<HarnessUpdate>);

/// Install `update` with the provider's own package manager. Replies with the version now
/// installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHarnessUpdate {
    pub update: HarnessUpdate,
}
agents_call!(RunHarnessUpdate, reply: String);

/// Run `prompt` once, read-only, as a background text job in `cwd` (a commit message, a pull
/// request's text) on `model` (else the background model engine settings name). When that CLI is
/// missing it runs on `fallback`, then Claude Code, then Codex, each on its default model.
/// Replies with the final message's text. [`StopGenerate`] under the same `request` stops it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Generate {
    pub request: String,
    pub cwd: String,
    pub prompt: String,
    pub model: Option<TextModel>,
    pub fallback: Option<Kind>,
}
agents_call!(Generate, reply: String);

/// Stop the [`Generate`] running under `request`; one that arrives first stops it as it starts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopGenerate {
    pub request: String,
}
agents_call!(StopGenerate, reply: Outcome);

/// Keep the parked agents of these sessions (by row) warm; the engine may let the others go
/// beyond the idle-agent count engine settings name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepIdleAgents {
    pub rows: Vec<i64>,
}
agents_call!(KeepIdleAgents, reply: Outcome);

/// Move the provider thread `thread` from account `from` to account `to`, where the provider keeps
/// its threads per account (Claude Code, Codex). Replies whether it moved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferThread {
    pub provider: Kind,
    pub from: String,
    pub to: String,
    pub thread: String,
}
agents_call!(TransferThread, reply: bool);

/// Forget the provider thread `thread` that ran on `account` in `cwd`: the provider's parked agent
/// for it goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgetThread {
    pub provider: Kind,
    pub thread: String,
    pub account: String,
    pub cwd: String,
}
agents_call!(ForgetThread, reply: Outcome);
