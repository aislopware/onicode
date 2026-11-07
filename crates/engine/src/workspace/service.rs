//! The Workspace group as an `onicode-rpc` service: `WatchWorkspace` and the workspace calls,
//! each wrapped in a `Routed` that names this engine (`None`) and, for a mutating call, its op.
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use onicode_protocol::workspace::{
    Archive, ClaimInboxEvents, CreateNote, CreateSession, Delete, DeleteNote, FinishAutomationRun,
    GenerateTitle, ListNotes, MarkSeen, NameWorktree, Pin, PrepareAutomationRun, PreviewSession,
    Rename, SaveGroups, SaveResponseNote, SearchSessions, SetReminder, SetWorkItem, UpsertNote,
    WatchWorkspace,
};
use onicode_protocol::{Call, OpId, Routed, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService, parse_params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::hub::WorkspaceHub;

pub struct WorkspaceService {
    hub: Arc<WorkspaceHub>,
}

impl WorkspaceService {
    #[must_use]
    pub fn new(hub: Arc<WorkspaceHub>) -> Self {
        Self { hub }
    }

    /// `StopSearch` for a session search.
    pub(crate) fn stop_search(&self, search: &str) {
        self.hub.stop_search(search);
    }
}

/// Every method this service serves.
const METHODS: [&str; 22] = [
    WatchWorkspace::NAME,
    CreateSession::NAME,
    Rename::NAME,
    Archive::NAME,
    Pin::NAME,
    Delete::NAME,
    SetWorkItem::NAME,
    SetReminder::NAME,
    SaveGroups::NAME,
    SearchSessions::NAME,
    PreviewSession::NAME,
    MarkSeen::NAME,
    GenerateTitle::NAME,
    NameWorktree::NAME,
    ListNotes::NAME,
    CreateNote::NAME,
    UpsertNote::NAME,
    DeleteNote::NAME,
    SaveResponseNote::NAME,
    PrepareAutomationRun::NAME,
    FinishAutomationRun::NAME,
    ClaimInboxEvents::NAME,
];

impl WorkspaceService {
    /// Whether `method` is one of the Workspace group's.
    #[must_use]
    pub fn serves(method: &str) -> bool {
        METHODS.contains(&method)
    }
}

/// The call's body and op, if it is for this engine.
fn routed<T: DeserializeOwned>(params: serde_json::Value) -> Result<(Option<OpId>, T), RpcError> {
    let routed: Routed<T> = parse_params(params)?;
    if routed.engine.is_some() {
        // Forwarding to other engines arrives with peers (engine.md, migration step 9).
        return Err(RpcError::Failed(
            "This engine serves only its own workspace".into(),
        ));
    }
    Ok((routed.op, routed.body))
}

fn reply<T: Serialize>(result: Result<T, String>) -> Result<RpcReply, RpcError> {
    RpcReply::value(&result.map_err(RpcError::Failed)?)
}

#[async_trait]
impl RpcService for WorkspaceService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = &self.hub;
        match method {
            WatchWorkspace::NAME => {
                let (_, body) = routed::<WatchWorkspace>(params)?;
                let items = hub.watch(body).map_err(RpcError::Failed)?;
                Ok(RpcReply::Stream(
                    items
                        .filter_map(|item| futures::future::ready(serde_json::to_value(&item).ok()))
                        .boxed(),
                ))
            }
            CreateSession::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.create_session(op, body).await)
            }
            Rename::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.rename(op, body).await)
            }
            Archive::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.archive(op, body).await)
            }
            Pin::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.pin(op, body).await)
            }
            Delete::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.delete(op, body).await)
            }
            SetWorkItem::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.set_work_item(op, body).await)
            }
            SetReminder::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.set_reminder(op, body).await)
            }
            SaveGroups::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.save_groups(op, body).await)
            }
            SearchSessions::NAME => {
                let (_, body) = routed(params)?;
                reply(hub.search_sessions(body).await)
            }
            PreviewSession::NAME => {
                let (_, body) = routed::<PreviewSession>(params)?;
                reply(hub.preview_session(&body))
            }
            MarkSeen::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.mark_seen(op, body).await)
            }
            GenerateTitle::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.generate_title(op, body).await)
            }
            NameWorktree::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.name_worktree(op, body).await)
            }
            ListNotes::NAME => {
                let (_, ListNotes {}) = routed(params)?;
                reply(hub.list_notes())
            }
            CreateNote::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.create_note(op, body).await)
            }
            UpsertNote::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.upsert_note(op, body).await)
            }
            DeleteNote::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.delete_note(op, body).await)
            }
            SaveResponseNote::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.save_response_note(op, body).await)
            }
            PrepareAutomationRun::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.prepare_automation_run(op, body).await)
            }
            FinishAutomationRun::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.finish_automation_run(op, body).await)
            }
            ClaimInboxEvents::NAME => {
                let (op, body) = routed(params)?;
                reply(hub.claim_inbox_events(op, body).await)
            }
            _ => Err(RpcError::UnknownMethod(method.into())),
        }
    }
}
