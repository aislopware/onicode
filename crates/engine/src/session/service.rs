//! The Session group as an `onicode-rpc` service: `WatchSession` and the session calls, each
//! wrapped in a `Routed` that names this engine (`None`) and, for a mutating call, its op.
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use onicode_protocol::session::{
    self, Answer, Btw, Configure, Decide, EditLastTurn, Fork, KeepQuestionOpen, QueueEdit,
    ReadTranscript, Rewind, SendNow, Steer, SteerNow, Stop, SwitchProvider, WatchSession,
};
use onicode_protocol::{Call, Outcome, Routed, Watch};
use onicode_rpc::{RpcError, RpcReply, RpcService, parse_params};
use serde::de::DeserializeOwned;

use super::hub::SessionHub;

pub struct SessionService {
    hub: Arc<SessionHub>,
}

impl SessionService {
    #[must_use]
    pub fn new(hub: Arc<SessionHub>) -> Self {
        Self { hub }
    }
}

/// The call's body and op, if it is for this engine.
fn routed<T: DeserializeOwned>(
    params: serde_json::Value,
) -> Result<(Option<onicode_protocol::OpId>, T), RpcError> {
    let routed: Routed<T> = parse_params(params)?;
    if routed.engine.is_some() {
        // Forwarding to other engines arrives with peers (engine.md, migration step 9).
        return Err(RpcError::Failed(
            "This engine serves only its own sessions".into(),
        ));
    }
    Ok((routed.op, routed.body))
}

fn outcome(result: Result<Outcome, String>) -> Result<RpcReply, RpcError> {
    RpcReply::value(&result.map_err(RpcError::Failed)?)
}

#[async_trait]
impl RpcService for SessionService {
    async fn handle(&self, method: &str, params: serde_json::Value) -> Result<RpcReply, RpcError> {
        let hub = &self.hub;
        if method == WatchSession::NAME {
            let (_, body) = routed::<WatchSession>(params)?;
            let items = hub.watch(body).map_err(RpcError::Failed)?;
            return Ok(RpcReply::Stream(
                items
                    .filter_map(|item| futures::future::ready(serde_json::to_value(&item).ok()))
                    .boxed(),
            ));
        }
        if method == session::Send::NAME {
            let (op, body) = routed::<session::Send>(params)?;
            return outcome(hub.send(op, body).await);
        }
        if method == Steer::NAME {
            let (op, body) = routed::<Steer>(params)?;
            return outcome(hub.steer(op, body).await);
        }
        if method == Stop::NAME {
            let (op, body) = routed::<Stop>(params)?;
            return outcome(hub.stop(op, body).await);
        }
        if method == Decide::NAME {
            let (op, body) = routed::<Decide>(params)?;
            return outcome(hub.decide(op, body).await);
        }
        if method == Answer::NAME {
            let (op, body) = routed::<Answer>(params)?;
            return outcome(hub.answer(op, body).await);
        }
        if method == KeepQuestionOpen::NAME {
            let (op, body) = routed::<KeepQuestionOpen>(params)?;
            return outcome(hub.keep_question_open(op, body).await);
        }
        if method == QueueEdit::NAME {
            let (op, body) = routed::<QueueEdit>(params)?;
            return outcome(hub.queue_edit(op, body).await);
        }
        if method == Configure::NAME {
            let (op, body) = routed::<Configure>(params)?;
            return outcome(hub.configure(op, body).await);
        }
        if method == ReadTranscript::NAME {
            let (_, body) = routed::<ReadTranscript>(params)?;
            return RpcReply::value(&hub.read_transcript(&body).map_err(RpcError::Failed)?);
        }
        // Still the app's own until they move into the engine (engine.md, migration step 4).
        if [
            SendNow::NAME,
            SteerNow::NAME,
            EditLastTurn::NAME,
            Rewind::NAME,
            Fork::NAME,
            Btw::NAME,
            SwitchProvider::NAME,
        ]
        .contains(&method)
        {
            return Err(RpcError::Failed(format!(
                "{method} is not served by the engine yet"
            )));
        }
        Err(RpcError::UnknownMethod(method.into()))
    }
}
