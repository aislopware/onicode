//! The provider doors the Agents group asks: sign-in state, usage, sign-in and sign-out, model
//! catalogs, skills, commands, MCP servers, the CLIs themselves and one-off text. A seam, so the
//! model tests and the app's tests script them and never run a provider CLI, `npm` or the
//! network.
//!
//! Sign-in state comes only through each provider's published door: `claude auth status --json`
//! and Codex's app-server `account/read`. No tool's credential files or tokens are read.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use onicode_core::accounts::{AuthAction, Identity, Profile, ProfileExt as _};
use onicode_core::agent::{AccessMode, Settings};
use onicode_core::catalog::Model;
use onicode_core::mcp::McpConnection;
use onicode_core::provider::{self, Event, Kind, NativeCommand, Request};
use onicode_core::session_title::TextModel;
use onicode_core::skills::DiscoveredSkill;
use onicode_core::usage;
use onicode_protocol::agents::{
    AddMcpServer, CatalogRead, HarnessUpdate, HarnessVersion, SignIn, UsageRead,
};
use serde_json::Value;

/// Everything the Agents group asks a provider or the disk. Every method blocks: the hub calls
/// them off its lock, on its spawner.
pub trait AgentProbe: Send + Sync + 'static {
    /// Who `profile` is signed in to `kind` as, through the provider's published door.
    fn sign_in(&self, kind: Kind, profile: &Profile) -> SignIn;

    /// The account's usage: what its turns reported, or the provider's poll. `billing` is the
    /// Pi billing provider, empty otherwise.
    fn usage(&self, kind: Kind, profile: &Profile, billing: &str) -> UsageRead;

    /// Runs the provider CLI's browser sign-in or its sign-out, until it ends or `cancel` is set.
    fn authenticate(
        &self,
        kind: Kind,
        profile: &Profile,
        action: AuthAction,
        cancel: Arc<AtomicBool>,
    ) -> Result<(), String>;

    /// Stops the account's agents, signs it out through the provider's door and deletes its
    /// profile.
    fn remove(&self, kind: Kind, profile: &Profile) -> Result<(), String>;

    /// The models `kind` offers `profile`.
    fn catalog(
        &self,
        kind: Kind,
        profile: &Profile,
        read: CatalogRead,
    ) -> Result<Option<Vec<Model>>, String>;

    fn skills(
        &self,
        project: &str,
        disabled: Option<Vec<String>>,
    ) -> Result<Vec<DiscoveredSkill>, String>;

    fn create_skill(&self, project: &str, name: &str, personal: bool) -> Result<String, String>;

    fn commands(
        &self,
        kind: Kind,
        project: &str,
        profile: &Profile,
        model: Option<String>,
    ) -> Result<Vec<NativeCommand>, String>;

    fn mcp_servers(&self, project: &str) -> Result<Vec<McpConnection>, String>;

    fn add_mcp_server(&self, body: AddMcpServer) -> Result<(), String>;

    fn remove_mcp_server(&self, project: &str, name: &str, scope: &str) -> Result<(), String>;

    fn mcp_login(&self, project: &str, provider: &str, name: &str) -> Result<(), String>;

    fn mcp_health(&self, project: &str) -> Result<String, String>;

    /// The CLI the engine runs for `kind`, if it finds one.
    fn harness(&self, kind: Kind) -> Option<String>;

    fn inspect(&self, kind: Kind, path: Option<&str>) -> Result<HarnessVersion, String>;

    /// The CLIs with newer releases, `hidden` providers left out.
    fn harness_updates(&self, hidden: &[String]) -> Vec<HarnessUpdate>;

    fn run_harness_update(&self, update: &HarnessUpdate) -> Result<String, String>;

    /// One read-only run of `model` in `cwd`: the final assistant message's text.
    fn generate(
        &self,
        model: &TextModel,
        cwd: &str,
        prompt: String,
        cancel: Arc<AtomicBool>,
    ) -> Result<String, String>;

    fn consume_reset(&self, profile: &Profile, credit: Option<&str>) -> Result<String, String>;

    fn keep_idle_agents(&self, rows: Vec<i64>);

    fn transfer_thread(
        &self,
        kind: Kind,
        from: &Profile,
        to: &Profile,
        thread: &str,
    ) -> Result<(), String>;

    fn forget_thread(&self, kind: Kind, thread: &str, account: &str, cwd: &str);
}

/// The real provider CLIs and disk.
pub struct CliProbe;

impl AgentProbe for CliProbe {
    fn sign_in(&self, kind: Kind, profile: &Profile) -> SignIn {
        let read = match kind {
            Kind::Claude => onicode_core::accounts::claude_auth_status(profile)
                .and_then(|printed| parse_claude_status(&printed)),
            Kind::Codex => onicode_core::catalog::codex_request(
                profile,
                "account/read",
                serde_json::json!({ "refreshToken": false }),
            )
            .and_then(|result| parse_codex_account(&result)),
            _ => return SignIn::default(),
        };
        read.unwrap_or_default()
    }

    fn usage(&self, kind: Kind, profile: &Profile, billing: &str) -> UsageRead {
        if usage::from_turns(kind) {
            return match usage::observed(kind, profile) {
                Some(Ok(snapshot)) => UsageRead::Snapshot(snapshot),
                Some(Err(error)) => UsageRead::Failed(error),
                None => UsageRead::Unreported,
            };
        }
        let model = (!billing.is_empty()).then(|| format!("{billing}/"));
        match usage::fetch_for(kind, profile, model.as_deref()) {
            Ok(snapshot) => UsageRead::Snapshot(snapshot),
            Err(error) => UsageRead::Failed(error),
        }
    }

    fn authenticate(
        &self,
        kind: Kind,
        profile: &Profile,
        action: AuthAction,
        cancel: Arc<AtomicBool>,
    ) -> Result<(), String> {
        if action == AuthAction::SignOut {
            onicode_core::accounts::stop_processes(kind, profile);
        }
        let result = onicode_core::accounts::authenticate(kind, profile, action, cancel);
        onicode_core::catalog::invalidate(kind, profile);
        result
    }

    fn remove(&self, kind: Kind, profile: &Profile) -> Result<(), String> {
        onicode_core::accounts::stop_processes(kind, profile);
        // An account that is already signed out can fail logout; removal still goes on.
        let _ = onicode_core::accounts::sign_out_for_removal(kind, profile);
        profile.remove_credentials(kind)
    }

    fn catalog(
        &self,
        kind: Kind,
        profile: &Profile,
        read: CatalogRead,
    ) -> Result<Option<Vec<Model>>, String> {
        use onicode_core::catalog;
        match read {
            CatalogRead::Cached => Ok(catalog::cached_models_for(kind, profile)),
            CatalogRead::Fresh => catalog::models_for(kind, profile).map(Some),
            CatalogRead::Reload => {
                catalog::invalidate(kind, profile);
                catalog::models_for(kind, profile).map(Some)
            }
        }
    }

    fn skills(
        &self,
        project: &str,
        disabled: Option<Vec<String>>,
    ) -> Result<Vec<DiscoveredSkill>, String> {
        onicode_core::skills::list_skills(project.to_owned(), disabled)
    }

    fn create_skill(&self, project: &str, name: &str, personal: bool) -> Result<String, String> {
        onicode_core::skills::create_blank(project, name, personal)
            .map(|path| path.to_string_lossy().into_owned())
    }

    fn commands(
        &self,
        kind: Kind,
        project: &str,
        profile: &Profile,
        model: Option<String>,
    ) -> Result<Vec<NativeCommand>, String> {
        let Some(binary) = provider::resolve(kind) else {
            return Ok(Vec::new());
        };
        provider::discover_commands(
            Request {
                kind,
                binary,
                cwd: project.into(),
                prompt: String::new(),
                provider_id: None,
                model,
                settings: Settings {
                    account: profile.clone(),
                    ..Settings::default()
                },
            },
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn mcp_servers(&self, project: &str) -> Result<Vec<McpConnection>, String> {
        onicode_core::mcp::mcp_discover(project.to_owned())
    }

    fn add_mcp_server(&self, body: AddMcpServer) -> Result<(), String> {
        let AddMcpServer {
            project,
            provider,
            scope,
            name,
            config,
        } = body;
        onicode_core::mcp::mcp_add(project, provider, scope, name, config)
    }

    fn remove_mcp_server(&self, project: &str, name: &str, scope: &str) -> Result<(), String> {
        onicode_core::mcp::remove(project, name, scope)
    }

    fn mcp_login(&self, project: &str, provider: &str, name: &str) -> Result<(), String> {
        onicode_core::mcp::login(project, provider, name)
    }

    fn mcp_health(&self, project: &str) -> Result<String, String> {
        onicode_core::mcp::claude_health(project)
    }

    fn harness(&self, kind: Kind) -> Option<String> {
        provider::resolve(kind).map(|path| path.to_string_lossy().into_owned())
    }

    fn inspect(&self, kind: Kind, path: Option<&str>) -> Result<HarnessVersion, String> {
        onicode_core::cli::inspect(kind, path).map(|inspection| HarnessVersion {
            path: inspection.path.to_string_lossy().into_owned(),
            version: inspection.version,
        })
    }

    fn harness_updates(&self, hidden: &[String]) -> Vec<HarnessUpdate> {
        onicode_core::harness_updates::find_updates(hidden)
            .into_iter()
            .map(|update| HarnessUpdate {
                harness: update.harness,
                installed: update.installed,
                latest: update.latest,
            })
            .collect()
    }

    fn run_harness_update(&self, update: &HarnessUpdate) -> Result<String, String> {
        onicode_core::harness_updates::run_update(&onicode_core::harness_updates::Update {
            harness: update.harness,
            installed: update.installed.clone(),
            latest: update.latest.clone(),
        })
    }

    fn generate(
        &self,
        model: &TextModel,
        cwd: &str,
        prompt: String,
        cancel: Arc<AtomicBool>,
    ) -> Result<String, String> {
        let binary = provider::resolve(model.kind)
            .ok_or_else(|| format!("{} CLI not installed", model.kind.label()))?;
        let request = Request {
            kind: model.kind,
            binary,
            cwd: cwd.into(),
            prompt,
            provider_id: None,
            model: model.model.clone(),
            settings: Settings {
                access: AccessMode::ReadOnly,
                account: model.account.clone(),
                ..Settings::default()
            },
        };
        let mut text = String::new();
        provider::run(request, cancel.clone(), |events| {
            if cancel.load(Ordering::SeqCst) {
                return Err("Cancelled".into());
            }
            for event in events {
                match event {
                    Event::BeginAssistant => text.clear(),
                    Event::Text(chunk) => text.push_str(&chunk),
                    _ => {}
                }
            }
            Ok(())
        })?;
        Ok(text)
    }

    fn consume_reset(&self, profile: &Profile, credit: Option<&str>) -> Result<String, String> {
        usage::consume_reset(profile, credit)
    }

    fn keep_idle_agents(&self, rows: Vec<i64>) {
        provider::keep_idle_agents(rows);
    }

    fn transfer_thread(
        &self,
        kind: Kind,
        from: &Profile,
        to: &Profile,
        thread: &str,
    ) -> Result<(), String> {
        from.transfer(to, kind, thread)
    }

    fn forget_thread(&self, kind: Kind, thread: &str, account: &str, cwd: &str) {
        provider::forget_session(kind, thread, account, Path::new(cwd));
    }
}

/// A probe for a viewer's tests: it runs nothing and answers as a machine with no provider CLI
/// would.
pub struct OfflineProbe;

const OFFLINE: &str = "No provider CLI runs in this engine";

impl AgentProbe for OfflineProbe {
    fn sign_in(&self, _: Kind, _: &Profile) -> SignIn {
        SignIn::default()
    }

    fn usage(&self, _: Kind, _: &Profile, _: &str) -> UsageRead {
        UsageRead::Unreported
    }

    fn authenticate(
        &self,
        _: Kind,
        _: &Profile,
        _: AuthAction,
        _: Arc<AtomicBool>,
    ) -> Result<(), String> {
        Err(OFFLINE.into())
    }

    fn remove(&self, _: Kind, _: &Profile) -> Result<(), String> {
        Ok(())
    }

    fn catalog(&self, _: Kind, _: &Profile, _: CatalogRead) -> Result<Option<Vec<Model>>, String> {
        Ok(None)
    }

    fn skills(&self, _: &str, _: Option<Vec<String>>) -> Result<Vec<DiscoveredSkill>, String> {
        Ok(Vec::new())
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

    fn harness(&self, _: Kind) -> Option<String> {
        None
    }

    fn inspect(&self, _: Kind, _: Option<&str>) -> Result<HarnessVersion, String> {
        Err("CLI could not be resolved".into())
    }

    fn harness_updates(&self, _: &[String]) -> Vec<HarnessUpdate> {
        Vec::new()
    }

    fn run_harness_update(&self, _: &HarnessUpdate) -> Result<String, String> {
        Err(OFFLINE.into())
    }

    fn generate(
        &self,
        _: &TextModel,
        _: &str,
        _: String,
        _: Arc<AtomicBool>,
    ) -> Result<String, String> {
        Err(OFFLINE.into())
    }

    fn consume_reset(&self, _: &Profile, _: Option<&str>) -> Result<String, String> {
        Err(OFFLINE.into())
    }

    fn keep_idle_agents(&self, _: Vec<i64>) {}

    fn transfer_thread(&self, _: Kind, _: &Profile, _: &Profile, _: &str) -> Result<(), String> {
        Err(OFFLINE.into())
    }

    fn forget_thread(&self, _: Kind, _: &str, _: &str, _: &str) {}
}

/// The first JSON object in `printed`.
fn json_object(printed: &str) -> Option<Value> {
    let start = printed.find('{')?;
    serde_json::from_str::<Value>(printed[start..].trim_end())
        .ok()
        .filter(Value::is_object)
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(String::from)
}

/// "claude_max" reads "Max"; "pro" reads "Pro".
fn plan_label(raw: &str) -> String {
    let plan = raw.strip_prefix("claude_").unwrap_or(raw);
    let mut chars = plan.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// What `claude auth status --json` printed, read as a sign-in. A token from `claude
/// setup-token` reports `authMethod: "oauth_token"` on the first-party API with no subscription
/// type, and is a Claude subscription all the same (claude-code-acp #1376). `Err` when it printed
/// no status this can read, whatever it exited with.
pub fn parse_claude_status(printed: &str) -> Result<SignIn, String> {
    let malformed = || "Claude Code's sign-in status could not be read".to_owned();
    let status = json_object(printed).ok_or_else(malformed)?;
    let signed_in = status
        .get("loggedIn")
        .and_then(Value::as_bool)
        .ok_or_else(malformed)?;
    if !signed_in {
        return Ok(SignIn {
            signed_in: Some(false),
            ..SignIn::default()
        });
    }
    let method = text(&status, "authMethod");
    let first_party = text(&status, "apiProvider").is_none_or(|api| api == "firstParty");
    let plan = text(&status, "subscriptionType")
        .map(|plan| plan_label(&plan))
        .or_else(|| {
            (method.as_deref() == Some("oauth_token") && first_party)
                .then(|| "Claude subscription".to_owned())
        });
    Ok(SignIn {
        signed_in: Some(true),
        identity: Identity {
            email: text(&status, "email"),
            name: text(&status, "displayName").or_else(|| text(&status, "name")),
            plan,
            organization: text(&status, "orgName"),
        },
        method: method.map(|method| method_label(&method)),
    })
}

fn method_label(method: &str) -> String {
    match method {
        "claude.ai" | "claude_ai" | "oauth" => "Claude account".into(),
        "oauth_token" => "OAuth token".into(),
        "api_key" | "apiKey" | "console" => "API key".into(),
        "third_party" | "thirdParty" => "Cloud provider".into(),
        other => other.into(),
    }
}

/// Codex's app-server `account/read` result, read as a sign-in.
pub fn parse_codex_account(result: &Value) -> Result<SignIn, String> {
    let Some(account) = result.get("account") else {
        return Err("Codex's account could not be read".into());
    };
    if account.is_null() {
        return Ok(SignIn {
            signed_in: Some(false),
            ..SignIn::default()
        });
    }
    match account.get("type").and_then(Value::as_str) {
        Some("chatgpt") => Ok(SignIn {
            signed_in: Some(true),
            identity: Identity {
                email: text(account, "email"),
                plan: text(account, "planType").map(|plan| plan_label(&plan)),
                ..Identity::default()
            },
            method: Some("ChatGPT account".into()),
        }),
        Some("apiKey") => Ok(SignIn {
            signed_in: Some(true),
            identity: Identity::default(),
            method: Some("API key".into()),
        }),
        _ => Err("Codex's account could not be read".into()),
    }
}
