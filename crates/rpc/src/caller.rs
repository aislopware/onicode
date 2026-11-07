//! Who is asking: the authority a request arrived with.
//!
//! The engine serves three kinds of caller over the same [`crate::RpcService`]:
//!
//! - [`Caller::Local`]: this device's app, holding the full token (`{data_dir}/ipc-token`), or an
//!   in-process client. The default when nothing set one.
//! - [`Caller::Agent`]: a `onicode mcp` server the engine injected into an agent's run, holding the
//!   agent-scoped token (`{data_dir}/ipc-token-agent`). It reaches only the calls the protocol
//!   marks as an agent's (`Call::AGENT`); nothing that grants power.
//! - [`Caller::Peer`]: another device, over a peer link ([`crate::serve_carried`]).
//!
//! The gates that matter are enforced by method where each connection is admitted (the engine's
//! policy for a socket [`crate::Admission`], the carrier's guard), so they never depend on this
//! value surviving a hop. This task-local exists for finer checks and for the record: read it at
//! dispatch entry, before any `tokio::spawn` (a spawned task starts with no caller, i.e. `Local`).

use std::future::Future;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caller {
    /// The full token over the local socket (the app) or an in-process client.
    Local,
    /// The agent-scoped token over the local socket. `chat` is the chat the MCP server says it
    /// serves: claimed, for the record only.
    Agent { chat: Option<String> },
    /// Another device over a peer link: `device` is the device-id its hello
    /// gave, `None` when the carrier was served without one.
    Peer { device: Option<String> },
}

impl Caller {
    pub fn is_local(&self) -> bool {
        matches!(self, Caller::Local)
    }

    /// `{chat}` / `{device}` for a record of who asked; `None` for the
    /// device's own app.
    pub fn record(&self) -> Option<serde_json::Value> {
        match self {
            Caller::Local => None,
            Caller::Agent { chat } => Some(serde_json::json!({ "agent": true, "chat": chat })),
            Caller::Peer { device } => Some(serde_json::json!({ "peer": true, "device": device })),
        }
    }
}

tokio::task_local! {
    static CALLER: Caller;
}

/// The caller of the request this task serves (`Local` when none was set).
pub fn caller() -> Caller {
    CALLER.try_with(Clone::clone).unwrap_or(Caller::Local)
}

/// Run `f` as `caller`.
pub async fn with_caller<F: Future>(caller: Caller, f: F) -> F::Output {
    CALLER.scope(caller, f).await
}
