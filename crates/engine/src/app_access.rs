//! The App access group's hub: what Operator's app actions read and write in the store. The
//! viewer that runs the calling thread keeps the bridge socket, decides whether the thread may
//! make the call and shows its effect (engine.md, migration step 8 moves the rest); this hub
//! keeps the receipts that make a request id answer once, starts the sessions an action asks for,
//! and reads conversations, notes and the model catalog. Inputs and replies are the Operator
//! tool's JSON, passed through as text.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use onicode_core::app_access as app;
use onicode_protocol::app_access::{
    AppModels, AppNotes, AppStarted, ReadAppConversation, ReadAppReceipt, SaveAppReceipt,
    StartAppSession,
};
use onicode_protocol::{Call, OpId, Outcome};
use onicode_rpc::{RpcError, RpcReply, RpcService};
use serde_json::{Value, json};

use crate::ledger::Ledger;
use crate::service::{Group, reply, routed};
use crate::session::Spawn;
use crate::turn::{Database, locked};
use crate::workspace::WorkspaceHub;

#[cfg(test)]
mod tests;

pub struct AppAccessHub {
    store: Database,
    workspace: Arc<WorkspaceHub>,
    spawn: Arc<dyn Spawn>,
    ledger: Ledger,
}

/// An action's input, as the tool sent it.
fn input(raw: &str) -> Result<Value, String> {
    serde_json::from_str(raw).map_err(|error| format!("Invalid input: {error}"))
}

impl AppAccessHub {
    #[must_use]
    pub fn new(store: Database, workspace: Arc<WorkspaceHub>, spawn: Arc<dyn Spawn>) -> Arc<Self> {
        Arc::new(Self {
            ledger: Ledger::new(store.clone()),
            store,
            workspace,
            spawn,
        })
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

    /// Where the accounts' homes are: the engine's data directory.
    fn accounts_root(&self) -> Result<Option<PathBuf>, String> {
        Ok(locked(&self.store)?.data_dir().map(Path::to_path_buf))
    }

    /// `ReadAppReceipt`.
    pub fn receipt(&self, body: &ReadAppReceipt) -> Result<Option<String>, String> {
        locked(&self.store)?.setting(&app::receipt_key(body.session, &body.request))
    }

    /// `SaveAppReceipt`, once per op.
    pub async fn save_receipt(
        &self,
        op: Option<OpId>,
        body: SaveAppReceipt,
    ) -> Result<Outcome, String> {
        self.ledger
            .once(op, async move {
                locked(&self.store)?.save_setting(
                    &app::receipt_key(body.session, &body.request),
                    &body.receipt,
                )?;
                Ok(Outcome::Applied)
            })
            .await
    }

    /// `AppModels`: each provider's models, read for its account.
    pub async fn models(&self, body: AppModels) -> Result<String, String> {
        let root = self.accounts_root()?;
        let profiles = body
            .profiles
            .into_iter()
            .map(|(kind, mut profile)| {
                profile.root.clone_from(&root);
                (kind, profile)
            })
            .collect();
        self.blocking(move || Ok(app::models_catalog(profiles)?.to_string()))
            .await
    }

    /// `StartAppSession`, once per op and once per request id: the session reserved, its
    /// checkout, title, settings and prompt saved, and the workspace told.
    pub async fn start(
        &self,
        op: Option<OpId>,
        body: StartAppSession,
    ) -> Result<AppStarted, String> {
        let store = self.store.clone();
        let root = self.accounts_root()?;
        let started = self
            .ledger
            .once(op, self.blocking(move || start(&store, root, body)))
            .await?;
        if let Some((session, _)) = &started.started {
            self.workspace.refresh_session(session.id);
        }
        Ok(started)
    }

    /// `ReadAppConversation`.
    pub async fn conversation(&self, body: ReadAppConversation) -> Result<String, String> {
        let store = self.store.clone();
        self.blocking(move || {
            let input = input(&body.input)?;
            Ok(locked(&store)?
                .app_conversation(body.session, &input, body.busy, body.draft)?
                .to_string())
        })
        .await
    }

    /// `AppNotes`, once per op.
    pub async fn notes(&self, op: Option<OpId>, body: AppNotes) -> Result<String, String> {
        let store = self.store.clone();
        self.ledger
            .once(
                op,
                self.blocking(move || {
                    let input = input(&body.input)?;
                    Ok(app::notes_action(
                        &*locked(&store)?,
                        body.caller,
                        &body.request,
                        &body.action,
                        &input,
                        &body.projects,
                    )?
                    .to_string())
                }),
            )
            .await
    }
}

/// `sessions.start`'s store side, as monocode's app ran it.
fn start(
    store: &Database,
    root: Option<PathBuf>,
    body: StartAppSession,
) -> Result<AppStarted, String> {
    let StartAppSession {
        caller,
        request,
        fingerprint,
        input,
        provider,
        kind,
        project,
        prompt,
        mut settings,
    } = body;
    let input = self::input(&input)?;
    settings.account.root = root;
    let models = if input.get("model").is_some()
        || input.get("modelSettings").is_some()
        || input.get("effort").is_some()
    {
        onicode_core::catalog::models_for(kind, &settings.account)?
    } else {
        onicode_core::catalog::cached_models_for(kind, &settings.account).unwrap_or_default()
    };
    let mut settings = app::launch_settings(&input, &provider, settings, kind, &models)?;
    settings.revision = 0;
    let title = app::optional_text(&input, "title", 160)?
        .unwrap_or_else(|| prompt.chars().take(160).collect());
    let mut store = locked(store)?;
    let (mut summary, fresh) =
        store.create_app_session(caller, &request, &fingerprint, kind.id(), &project)?;
    if fresh {
        if let Err(error) = app::launch_checkout(&input, &project, summary.id, &mut settings) {
            let _ = store.delete(summary.id);
            return Err(error);
        }
        store.title(summary.id, &title)?;
        summary.title = title;
        store.configure_agent(summary.id, kind.id(), &settings)?;
        store.save_started_link(summary.id, caller)?;
        store.prepare_app_prompt(summary.id, &request, &prompt)?;
        summary.has_messages = true;
    }
    let draft = input["draft"].as_bool().unwrap_or(false);
    let value = json!({
        "id": summary.id,
        "sessionId": summary.id,
        "cwd": project,
        "harness": kind.id(),
        "model": settings.model,
        "submitted": !draft,
        "draft": draft,
        "accepted": true,
    });
    Ok(AppStarted {
        value: value.to_string(),
        started: fresh.then_some((summary, settings)),
    })
}

/// The App access group as an `onicode-rpc` service.
pub struct AppAccessService {
    hub: Arc<AppAccessHub>,
}

impl AppAccessService {
    #[must_use]
    pub fn new(hub: Arc<AppAccessHub>) -> Self {
        Self { hub }
    }
}

const METHODS: [&str; 6] = [
    ReadAppReceipt::NAME,
    SaveAppReceipt::NAME,
    AppModels::NAME,
    StartAppSession::NAME,
    ReadAppConversation::NAME,
    AppNotes::NAME,
];

impl Group for AppAccessService {
    fn serves(&self, method: &str) -> bool {
        METHODS.contains(&method)
    }
}

#[async_trait::async_trait]
impl RpcService for AppAccessService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = &self.hub;
        match method {
            ReadAppReceipt::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.receipt(&body))
            }
            SaveAppReceipt::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.save_receipt(op, body).await)
            }
            AppModels::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.models(body).await)
            }
            StartAppSession::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.start(op, body).await)
            }
            ReadAppConversation::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.conversation(body).await)
            }
            AppNotes::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.notes(op, body).await)
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
