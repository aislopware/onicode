//! The Session group as an `onicode-rpc` service: `WatchSession` and the session calls, each
//! wrapped in a `Routed` that names this engine (`None`) and, for a mutating call, its op.
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use onicode_protocol::session::{
    self, Answer, Btw, Compact, Configure, Decide, EditLastTurn, Fork, KeepQuestionOpen, QueueEdit,
    ReadBtw, ReadSession, ReadTranscript, RecallLastTurn, ResetThread, Rewind, SaveBtw, SaveQueue,
    SendNow, Steer, SteerNow, Stop, StopBtw, SwitchProvider, WatchSession,
};
use onicode_protocol::{Call, Routed, Watch};
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

fn reply<T: serde::Serialize>(result: Result<T, String>) -> Result<RpcReply, RpcError> {
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
            return reply(hub.send(op, body).await);
        }
        if method == Steer::NAME {
            let (op, body) = routed::<Steer>(params)?;
            return reply(hub.steer(op, body).await);
        }
        if method == Stop::NAME {
            let (op, body) = routed::<Stop>(params)?;
            return reply(hub.stop(op, body).await);
        }
        if method == Decide::NAME {
            let (op, body) = routed::<Decide>(params)?;
            return reply(hub.decide(op, body).await);
        }
        if method == Answer::NAME {
            let (op, body) = routed::<Answer>(params)?;
            return reply(hub.answer(op, body).await);
        }
        if method == KeepQuestionOpen::NAME {
            let (op, body) = routed::<KeepQuestionOpen>(params)?;
            return reply(hub.keep_question_open(op, body).await);
        }
        if method == QueueEdit::NAME {
            let (op, body) = routed::<QueueEdit>(params)?;
            return reply(hub.queue_edit(op, body).await);
        }
        if method == Configure::NAME {
            let (op, body) = routed::<Configure>(params)?;
            return reply(hub.configure(op, body).await);
        }
        if method == SendNow::NAME {
            let (op, body) = routed::<SendNow>(params)?;
            return reply(hub.send_now(op, body).await);
        }
        if method == SteerNow::NAME {
            let (op, body) = routed::<SteerNow>(params)?;
            return reply(hub.steer_now(op, body).await);
        }
        if method == SaveQueue::NAME {
            let (op, body) = routed::<SaveQueue>(params)?;
            return reply(hub.save_queue(op, body).await);
        }
        if method == RecallLastTurn::NAME {
            let (_, body) = routed::<RecallLastTurn>(params)?;
            return reply(hub.recall_last_turn(&body));
        }
        if method == EditLastTurn::NAME {
            let (op, body) = routed::<EditLastTurn>(params)?;
            return reply(hub.edit_last_turn(op, body).await);
        }
        if method == ReadBtw::NAME {
            let (_, body) = routed::<ReadBtw>(params)?;
            return reply(hub.read_btw(&body));
        }
        if method == SaveBtw::NAME {
            let (op, body) = routed::<SaveBtw>(params)?;
            return reply(hub.save_btw(op, body).await);
        }
        if method == Btw::NAME {
            let (op, body) = routed::<Btw>(params)?;
            return reply(hub.btw(op, body).await);
        }
        if method == StopBtw::NAME {
            let (op, body) = routed::<StopBtw>(params)?;
            return reply(hub.stop_btw(op, body).await);
        }
        if method == ReadSession::NAME {
            let (_, body) = routed::<ReadSession>(params)?;
            return reply(hub.read_session(&body));
        }
        if method == ReadTranscript::NAME {
            let (_, body) = routed::<ReadTranscript>(params)?;
            return reply(hub.read_transcript(&body));
        }
        if method == Rewind::NAME {
            let (op, body) = routed::<Rewind>(params)?;
            return reply(hub.rewind_turns(op, body).await);
        }
        if method == Fork::NAME {
            let (op, body) = routed::<Fork>(params)?;
            return reply(hub.fork(op, body).await);
        }
        if method == SwitchProvider::NAME {
            let (op, body) = routed::<SwitchProvider>(params)?;
            return reply(hub.switch_provider(op, body).await);
        }
        if method == ResetThread::NAME {
            let (op, body) = routed::<ResetThread>(params)?;
            return reply(hub.reset_thread(op, body).await);
        }
        if method == Compact::NAME {
            let (op, body) = routed::<Compact>(params)?;
            return reply(hub.compact(op, body).await);
        }
        Err(RpcError::UnknownMethod(method.into()))
    }
}
