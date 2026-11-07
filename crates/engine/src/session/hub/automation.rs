//! Automation runs, started by the engine that holds the automation's project (engine.md,
//! "Automations"). The workspace hub claims a run (its schedule came, an Inbox event matched, or
//! a viewer asked to run it now) and makes or takes its session; this sends the prompt under the
//! session's stored settings, as the usage-limit resume does, with no viewer open, and records
//! how the run ended when its turn does. A viewer watching the session sees the turn as any other.
use onicode_core::automations::DueAutomationRun;
use onicode_core::provider::Kind;
use onicode_core::transcript::Row;
use onicode_protocol::{MessageId, OpId, session};
use tokio::sync::watch;

use super::{Inner, SessionHub};

/// How much of a failed run's last answer its record keeps.
const ERROR_CHARS: usize = 12_000;

impl SessionHub {
    /// Starts the claimed run `due` in this engine: its session is made or taken with the
    /// automation's settings, on the account its project uses here, and its prompt is sent. A
    /// run that cannot start is recorded as failed, with why. Called with no lock held.
    pub(crate) fn start_automation(self: &std::sync::Arc<Self>, due: DueAutomationRun) {
        let Some(workspace) = self.workspace.get() else {
            return;
        };
        let account = self
            .accounts
            .get()
            .map(|accounts| {
                let book = accounts.state().book;
                let kind = Kind::from_id(&due.automation.harness);
                accounts.profile(&book.selected(kind, &due.automation.cwd))
            })
            .unwrap_or_default();
        let Ok(prepared) = workspace.prepare_automation_run(&due, account) else {
            return;
        };
        let mut inner = self.lock();
        let body = session::Send {
            session: prepared.session,
            message: MessageId::new(),
            text: prepared.prompt,
            attachments: Vec::new(),
            turn: None,
        };
        let (resolve, _) = watch::channel(None);
        match self.launch(&mut inner, Some(OpId::new()), body, &mut Some(resolve)) {
            Ok(()) => {
                if let Some(turn) = inner
                    .sessions
                    .get_mut(&prepared.session)
                    .and_then(|live| live.turn.as_mut())
                {
                    turn.automation = Some(due.run.id);
                }
            }
            Err(error) => {
                workspace.finish_automation_run(&due.run.id, "failed", Some(error));
                forget_idle(&mut inner, prepared.session);
            }
        }
    }
}

/// How an automation's turn ended, as its run records it: `cancelled` when it was stopped,
/// `succeeded` when the provider had the prompt and the turn ended ready, else `failed` with
/// the turn's last status and what the agent last answered.
pub(super) fn ended(
    status: &str,
    rows: &[Row],
    started: bool,
    cancelled: bool,
) -> (&'static str, Option<String>) {
    if cancelled {
        return ("cancelled", None);
    }
    if started && status == "Ready" {
        return ("succeeded", None);
    }
    let from = rows.iter().rposition(|row| row.role == "user").unwrap_or(0);
    let answer = onicode_protocol::response::prose(&rows[from..]);
    let text = format!("{status}\n{answer}");
    (
        "failed",
        Some(onicode_protocol::response::limit(text.trim(), ERROR_CHARS)),
    )
}

/// Lets a session go that was loaded for a run that never started, unless something holds it.
fn forget_idle(inner: &mut Inner, session: onicode_protocol::SessionId) {
    if inner
        .sessions
        .get(&session)
        .is_some_and(|live| live.turn.is_none() && !live.topic.is_watched())
    {
        inner.sessions.remove(&session);
    }
}
