//! Routing (engine.md, "Routing"): `Routed.engine` names the engine that serves a call or a
//! watch. The app speaks only to its own engine, which serves what names it (or names nothing)
//! and forwards the rest over the tailnet ([`Peering`]), every group alike: a forwarded call is
//! the same envelope on a QUIC stream of its own, and a forwarded watch passes the host's items
//! through untouched, so the viewer's cursor is the host's. When the link under a forwarded watch
//! drops (or loses to a duplicate), the watch subscribes again on the next link with the cursor
//! of the last item it passed on ([`resume_watch`]): a cursor the host's ring still covers costs
//! no snapshot, and a host that restarted (a new epoch) answers with a fresh one. Nothing is
//! buffered on the hop, so the host's backpressure reaches the viewer: a slow viewer's queue at
//! the host overflows to `Resync`, which passes through like any item.
//!
//! A mutating call (one with an `OpId`) for an engine that cannot be reached waits in the
//! durable outbox (`outbox`) until it can, and its reply comes when it is delivered. A
//! peer's engine serves only what names it: a call from a peer, or from an agent's token, for
//! another engine is refused rather than forwarded again. A peer that does not speak this
//! engine's `PROTOCOL` (its hello's `protocol:` capability) is answered only the frozen calls.
use std::sync::{Arc, Mutex, PoisonError, Weak};

use async_trait::async_trait;
use futures::stream::BoxStream;
use onicode_peer::Resume;
use onicode_protocol::{EngineId, MethodKind, OpId, PROTOCOL};
use onicode_rpc::{Caller, RpcError, RpcReply, RpcService};
use serde_json::Value;

use crate::host::FROZEN;
use crate::outbox::Outbox;
use crate::peers::Identity;
use crate::service::EngineService;

/// A hop to another engine that did not come back with its answer.
#[derive(Debug)]
pub enum Hop {
    /// The engine could not be reached, or the link went before its answer: the call may or may
    /// not have run there, and its op makes a retry safe.
    Unreachable(String),
    /// The engine answered with an error.
    Refused(RpcError),
}

/// How an engine reaches its peers: the tailnet's links ([`crate::peers::Peers`]), or a test's.
#[async_trait]
pub trait Peering: Send + Sync + 'static {
    /// Calls `method` on `engine` with `params`, which name no engine (the host serves them as
    /// its own).
    async fn call(&self, engine: &EngineId, method: &str, params: Value) -> Result<Value, Hop>;

    /// Watches `method` on `engine`: the host's items as it sends them, resubscribing with the
    /// cursor of the last one after a lost link.
    async fn watch(
        &self,
        engine: &EngineId,
        method: &str,
        params: Value,
    ) -> Result<BoxStream<'static, Value>, Hop>;

    /// Resolves once `engine` is reachable again; never while it is not.
    async fn reached(&self, engine: &EngineId);

    /// The protocol `device` says it speaks, if its hello said.
    fn protocol(&self, device: &str) -> Option<u32>;
}

/// Where a call or a watch goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// This engine serves it.
    Here,
    /// The engine named serves it: forward it there.
    Forward(EngineId),
    /// It names another engine, and its caller may not reach it through this one.
    Refuse(String),
}

/// Where a call from `caller` naming `target` goes, at the engine `me`.
pub(crate) fn route(me: &EngineId, caller: &Caller, target: Option<&EngineId>) -> Route {
    match target {
        None => Route::Here,
        Some(target) if target == me => Route::Here,
        Some(target) => match caller {
            Caller::Local => Route::Forward(target.clone()),
            Caller::Agent { .. } => Route::Refuse(format!(
                "An agent reaches only the engine it runs on, not {target}"
            )),
            Caller::Peer { .. } => Route::Refuse(format!(
                "This engine serves only its own calls: reach {target} directly"
            )),
        },
    }
}

/// The engine `params` names, if any.
pub(crate) fn target_of(params: &Value) -> Result<Option<EngineId>, RpcError> {
    match params.get("engine") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(engine)) => Ok(Some(EngineId(engine.clone()))),
        Some(other) => Err(RpcError::BadParams(format!("engine: {other}"))),
    }
}

/// `params` as the engine they name serves them: naming no engine, its own.
pub(crate) fn here(mut params: Value) -> Value {
    if let Some(routed) = params.as_object_mut()
        && routed.contains_key("engine")
    {
        routed.insert("engine".into(), Value::Null);
    }
    params
}

/// The mutating call's op, if `params` carry one.
pub(crate) fn op_of(params: &Value) -> Option<OpId> {
    params
        .get("op")
        .and_then(Value::as_str)
        .and_then(|op| op.parse().ok())
}

/// Keeps a forwarded watch's params at the cursor of the last item it passed on: a snapshot's,
/// then each batch's last number. `Resync` ends it, as it ends the watch at the host.
pub fn resume_watch() -> Resume {
    Box::new(|item, params| {
        match item {
            Value::String(name) if name == "Resync" => return false,
            Value::Object(item) => {
                if let Some(at) = item.get("Snapshot").and_then(|snapshot| snapshot.get("at")) {
                    if let Some(body) = params.get_mut("body").and_then(Value::as_object_mut) {
                        body.insert("since".into(), at.clone());
                    }
                } else if let Some(last) = item
                    .get("Events")
                    .and_then(|events| events.get("last"))
                    .and_then(Value::as_u64)
                    && let Some(since) = params
                        .get_mut("body")
                        .and_then(|body| body.get_mut("since"))
                        .and_then(Value::as_object_mut)
                {
                    since.insert("seq".into(), Value::from(last));
                }
            }
            _ => {}
        }
        true
    })
}

/// The engine's service as its viewers and peers reach it: every group, routed.
pub struct Router {
    inner: Arc<EngineService>,
    identity: Arc<Identity>,
    peering: Mutex<Option<Weak<dyn Peering>>>,
    outbox: Arc<Outbox>,
}

impl Router {
    /// Routes over `inner`, as the engine `identity` names, with calls for an engine that cannot
    /// be reached waiting in `outbox`.
    pub(crate) fn new(
        inner: Arc<EngineService>,
        identity: Arc<Identity>,
        outbox: Arc<Outbox>,
    ) -> Self {
        Self {
            inner,
            identity,
            peering: Mutex::new(None),
            outbox,
        }
    }

    /// Forwards through `peering` from now on, and delivers what waits in the outbox.
    pub(crate) fn attach(&self, peering: &Arc<dyn Peering>) {
        *self.peering.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(Arc::downgrade(peering));
        self.outbox.start(peering);
    }

    fn peering(&self) -> Option<Arc<dyn Peering>> {
        self.peering
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade)
    }

    /// Whether `caller` may make `method`: a peer on another protocol only the frozen calls.
    fn admits(&self, caller: &Caller, method: &str) -> Result<(), RpcError> {
        let Caller::Peer { device } = caller else {
            return Ok(());
        };
        if FROZEN.contains(&method) {
            return Ok(());
        }
        let speaks = device
            .as_deref()
            .and_then(|device| self.peering()?.protocol(device));
        if speaks == Some(PROTOCOL) {
            Ok(())
        } else {
            Err(RpcError::Failed(format!(
                "{method}: this engine speaks protocol {PROTOCOL}; update the engine that asked"
            )))
        }
    }

    async fn forward(
        &self,
        engine: EngineId,
        method: &str,
        params: Value,
    ) -> Result<RpcReply, RpcError> {
        let kind = onicode_protocol::method(method)
            .map(|method| method.kind)
            .ok_or_else(|| RpcError::UnknownMethod(method.to_owned()))?;
        let params = here(params);
        let peering = self.peering();
        if let Some(peering) = &peering
            && !FROZEN.contains(&method)
            && let Some(theirs) = peering.protocol(&engine.0)
            && theirs != PROTOCOL
        {
            return Err(RpcError::Failed(format!(
                "{engine} needs an update: it speaks protocol {theirs}, this engine {PROTOCOL}"
            )));
        }
        let offline = |why: &str| RpcError::Failed(format!("{engine} is offline: {why}"));
        match kind {
            MethodKind::Watch => {
                let peering =
                    peering.ok_or_else(|| offline("this engine is not on the tailnet"))?;
                match peering.watch(&engine, method, params).await {
                    Ok(items) => Ok(RpcReply::Stream(items)),
                    Err(Hop::Unreachable(why)) => Err(offline(&why)),
                    Err(Hop::Refused(error)) => Err(error),
                }
            }
            MethodKind::Call => match op_of(&params) {
                Some(op) => self
                    .outbox
                    .send(peering, &engine, op, method, params)
                    .await
                    .map(RpcReply::Value),
                None => {
                    let peering =
                        peering.ok_or_else(|| offline("this engine is not on the tailnet"))?;
                    match peering.call(&engine, method, params).await {
                        Ok(reply) => Ok(RpcReply::Value(reply)),
                        Err(Hop::Unreachable(why)) => Err(offline(&why)),
                        Err(Hop::Refused(error)) => Err(error),
                    }
                }
            },
        }
    }
}

#[async_trait]
impl RpcService for Router {
    async fn handle(&self, method: &str, params: Value) -> Result<RpcReply, RpcError> {
        let caller = onicode_rpc::caller();
        self.admits(&caller, method)?;
        let target = target_of(&params)?;
        match route(&self.identity.id(), &caller, target.as_ref()) {
            Route::Here => self.inner.handle(method, here(params)).await,
            Route::Forward(engine) => self.forward(engine, method, params).await,
            Route::Refuse(why) => Err(RpcError::Failed(why)),
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn engine() -> impl Strategy<Value = EngineId> {
        prop_oneof![Just("mac"), Just("laptop"), Just("vps")]
            .prop_map(|engine| EngineId(engine.into()))
    }

    fn caller() -> impl Strategy<Value = Caller> {
        prop_oneof![
            Just(Caller::Local),
            Just(Caller::Agent { chat: None }),
            proptest::option::of(Just("phone".to_owned()))
                .prop_map(|device| Caller::Peer { device }),
        ]
    }

    fn json() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::from),
            any::<i64>().prop_map(Value::from),
            ".{0,6}".prop_map(Value::from),
        ];
        leaf.prop_recursive(3, 16, 4, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::from),
                proptest::collection::btree_map(".{0,4}", inner, 0..4)
                    .prop_map(|map| Value::Object(map.into_iter().collect())),
            ]
        })
    }

    proptest! {
        /// The model of routing: what names no engine or this one is served here, whoever asks;
        /// what names another engine is forwarded only for this machine's own app, and refused
        /// for a peer or an agent, so nothing is ever forwarded twice.
        #[test]
        fn routes_follow_the_model(
            me in engine(),
            caller in caller(),
            target in proptest::option::of(engine()),
        ) {
            let routed = route(&me, &caller, target.as_ref());
            match (&target, &caller) {
                (None, _) => prop_assert_eq!(routed, Route::Here),
                (Some(target), _) if *target == me => prop_assert_eq!(routed, Route::Here),
                (Some(target), Caller::Local) => {
                    prop_assert_eq!(routed, Route::Forward(target.clone()));
                }
                (Some(_), _) => prop_assert!(matches!(routed, Route::Refuse(_))),
            }
        }

        /// A forwarded call reaches its host naming no engine, its op and body untouched; the
        /// engine it named is read back exactly.
        #[test]
        fn forwarding_rewrites_only_the_engine(
            target in proptest::option::of(engine()),
            op in proptest::option::of(any::<u128>()),
            body in json(),
        ) {
            let op = op.map(|op| OpId(uuid::Uuid::from_u128(op)));
            let params = serde_json::to_value(onicode_protocol::Routed {
                engine: target.clone(),
                op,
                body: body.clone(),
            }).unwrap();
            prop_assert_eq!(target_of(&params).unwrap(), target);
            prop_assert_eq!(op_of(&params), op);
            let forwarded = here(params);
            prop_assert_eq!(target_of(&forwarded).unwrap(), None);
            let back: onicode_protocol::Routed<Value> = serde_json::from_value(forwarded).unwrap();
            prop_assert_eq!(back.op, op);
            prop_assert_eq!(back.body, body);
        }

        /// The resume hook follows the cursor a viewer would hold: a snapshot's, then each batch's
        /// last number, whatever the batches held; `Resync` ends it.
        #[test]
        fn the_resume_hook_holds_the_viewers_cursor(
            epoch in any::<u64>(),
            start in 0_u64..1000,
            batches in proptest::collection::vec((0_u64..5, any::<bool>()), 0..12),
        ) {
            let mut resume = resume_watch();
            let mut params = serde_json::json!({
                "engine": null, "op": null,
                "body": { "session": "s", "since": null, "window_turns": 1 },
            });
            let snapshot: onicode_protocol::Item<(), ()> = onicode_protocol::Item::Snapshot {
                at: onicode_protocol::Cursor { epoch, seq: start },
                state: (),
            };
            prop_assert!(resume(&serde_json::to_value(snapshot).unwrap(), &mut params));
            let mut seq = start;
            for (count, empty) in batches {
                let first = seq + 1;
                let last = seq + count;
                let events = if empty {
                    Vec::new()
                } else {
                    vec![(); usize::try_from(count).unwrap()]
                };
                let item: onicode_protocol::Item<(), ()> =
                    onicode_protocol::Item::Events { first, last, events };
                prop_assert!(resume(&serde_json::to_value(item).unwrap(), &mut params));
                seq = last;
                let since: onicode_protocol::Cursor =
                    serde_json::from_value(params["body"]["since"].clone()).unwrap();
                prop_assert_eq!(since, onicode_protocol::Cursor { epoch, seq });
            }
            let resync: onicode_protocol::Item<(), ()> = onicode_protocol::Item::Resync;
            prop_assert!(!resume(&serde_json::to_value(resync).unwrap(), &mut params));
        }
    }
}
