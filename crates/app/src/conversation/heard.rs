//! What an open conversation hears of its session besides the turn it sends: the turns the
//! engine runs on its own word (the usage-limit resume, a send from another device or another
//! window, an automation) and what the engine changes between turns (a refused resume's error
//! row, the usage limit it holds). A turn the view did not send shows exactly as if the person
//! had pressed send: the view goes busy, the prompt and the reply stream in, and stop, steer,
//! approvals and questions reach it. The session's watch is `engine_client::session_turns`; a
//! heard turn is followed with `engine_client::join_turn`, through the same merge as a sent one
//! (`Conversation::follow_turn`).
use std::collections::VecDeque;

use onicode_protocol::MessageId;
use onicode_protocol::transcript::Row;
use onicode_protocol::usage::Limit;

use super::*;
use crate::engine_client::SessionTurn;

/// How many of its own turns' prompt ids a view remembers: enough that the watch reporting one
/// late is never taken for another's.
const OWN_TURNS: usize = 8;

#[derive(Default)]
pub(super) struct State {
    /// The session's watch, from the first load (or send) on, for as long as the view lives.
    hearing: Option<gpui::Task<()>>,
    /// The prompt ids of the turns this view sent, newest last.
    own: VecDeque<MessageId>,
    /// What the watch reported while the view ran a turn, taken once it ends.
    deferred: Vec<SessionTurn>,
    /// A heard turn is being joined: its first update has not arrived yet.
    joining: bool,
}

/// Whether the view's rows from `start` on already are `rows`.
fn shows(held: &[Row], start: usize, rows: &[Row]) -> bool {
    held.get(start..start + rows.len())
        .is_some_and(|held| held == rows)
}

impl Conversation {
    /// Starts following the session's watch, once: a local session, from its first load or send.
    pub(super) fn hear_session(&mut self, cx: &mut Context<Self>) {
        if self.heard.hearing.is_some() {
            return;
        }
        let turns = crate::engine_client::session_turns(&self.engine, self.summary.id, cx);
        self.heard.hearing = Some(cx.spawn(async move |this, cx| {
            while let Ok(turn) = turns.recv().await {
                if this.update(cx, |this, cx| this.heard(turn, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Notes a turn this view sent, so the watch's report of it is not taken for another's.
    pub(super) fn remember_own_turn(&mut self, message: MessageId) {
        if self.heard.own.len() == OWN_TURNS {
            self.heard.own.pop_front();
        }
        self.heard.own.push_back(message);
    }

    /// One report of the session's watch. While the view runs a turn of its own, it waits for
    /// that turn's end: the engine runs one turn at a time, so a turn heard meanwhile began after
    /// it.
    fn heard(&mut self, turn: SessionTurn, cx: &mut Context<Self>) {
        if let SessionTurn::Started {
            message: Some(message),
        } = &turn
            && self.heard.own.contains(message)
        {
            return;
        }
        if self.busy || self.stopping || self.heard.joining {
            self.heard.deferred.push(turn);
            return;
        }
        match turn {
            SessionTurn::Started { .. } => self.join_engine_turn(cx),
            SessionTurn::Between { rows, usage_limit } => {
                self.between_turns(rows, usage_limit, cx);
            }
        }
    }

    /// Takes what the watch reported while the view's turn ran, in order, until one of them
    /// starts a turn to follow.
    pub(super) fn take_deferred_heard(&mut self, cx: &mut Context<Self>) {
        let mut deferred = std::mem::take(&mut self.heard.deferred).into_iter();
        while !self.busy && !self.stopping && !self.heard.joining {
            let Some(turn) = deferred.next() else {
                return;
            };
            self.heard(turn, cx);
        }
        // A turn is followed now: the rest waits for its end.
        let mut rest: Vec<SessionTurn> = deferred.collect();
        rest.append(&mut self.heard.deferred);
        self.heard.deferred = rest;
    }

    /// Follows the turn the engine runs in this session as if the person had sent it. Its first
    /// update decides: a turn that already ended and left nothing the view lacks shows nothing.
    fn join_engine_turn(&mut self, cx: &mut Context<Self>) {
        if !self.history_ready() {
            self.defer_history_action(|this, cx| this.join_engine_turn(cx), cx);
            return;
        }
        self.heard.joining = true;
        // A one-slot channel, as for a sent turn: what arrives while the view waits for its beat
        // folds into the next update.
        let (tx, rx) = async_channel::bounded(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let from = self.transcript.read(cx).source_rows().len();
        let controls = crate::engine_client::join_turn(
            &self.engine,
            self.summary.id,
            from,
            cancel.clone(),
            tx,
            cx,
        );
        cx.spawn(async move |this, cx| {
            let first = rx.recv().await;
            let _ = this.update(cx, |this, cx| {
                this.heard.joining = false;
                match first {
                    Ok(first) if !(first.finished && first.rows.is_empty()) => {
                        // The person sent something meanwhile: the engine refuses it while this
                        // turn runs, and says so in that send's own end.
                        if this.busy || this.stopping {
                            return;
                        }
                        this.follow_joined(controls, cancel, first, rx, cx);
                    }
                    _ => {}
                }
                this.take_deferred_heard(cx);
            });
        })
        .detach();
    }

    /// What a send does before its turn's updates arrive, for a turn the engine started.
    fn follow_joined(
        &mut self,
        controls: crate::engine_client::TurnControls,
        cancel: Arc<AtomicBool>,
        first: onicode_engine::turn::Update,
        rx: async_channel::Receiver<onicode_engine::turn::Update>,
        cx: &mut Context<Self>,
    ) {
        // The turn continues past any usage limit, as every turn does.
        self.settings.usage_limit = None;
        let model_label = self.model_label();
        self.sync_tool_paths(cx);
        self.transcript
            .update(cx, |v, _| v.identity(&self.summary.provider, &model_label));
        self.begin_turn(cx);
        self.status = "Connecting…".into();
        self.cancel = cancel;
        self.approvals.clear();
        self.questions.clear();
        self.controls = Some(controls);
        let following = Following {
            generation: self.submission_generation,
            source_provider: self.summary.provider.clone(),
            source_account: self.settings.account.id.clone(),
            // The sender names a new checkout's branch.
            worktree_message: String::new(),
            turn_model_id: self
                .model_info()
                .map(|model| model.id.clone())
                .or_else(|| self.model.clone()),
        };
        self.follow_turn(Some(first), rx, following, cx);
        cx.emit(SessionChanged);
        cx.notify();
    }

    /// What the engine changed with no turn running: the rows from an index on, and the usage
    /// limit it holds.
    fn between_turns(
        &mut self,
        rows: Option<(usize, Vec<Row>)>,
        usage_limit: Option<Option<Limit>>,
        cx: &mut Context<Self>,
    ) {
        let mut changed = false;
        // History that is not loaded is read whole when it is.
        if let Some((start, rows)) = rows
            && self.history_ready()
        {
            let held = self.transcript.read(cx).source_rows();
            if start <= held.len() && !rows.is_empty() && !shows(held, start, &rows) {
                self.transcript.update(cx, |v, cx| v.patch(start, rows, cx));
                self.summary.has_messages = true;
                changed = true;
            }
        }
        if let Some(limit) = usage_limit
            && self.settings.usage_limit != limit
        {
            self.settings.usage_limit = limit;
            changed = true;
        }
        if changed {
            cx.emit(SessionChanged);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::TestAppContext;

    use super::*;
    use crate::engine_client::Engine;

    fn row(role: &str, text: &str) -> Row {
        Row {
            role: role.into(),
            text: text.into(),
            first: true,
            sent_at: None,
            id: None,
        }
    }

    /// A session whose turns fail once their prompt is saved, without ever launching a real
    /// provider, with one turn behind it.
    fn session(engine: &Engine) -> Session {
        let mut summary = engine
            .seed()
            .create("codex", "/missing/heard-turns-project")
            .unwrap();
        summary.provider = "fixture".into();
        engine
            .seed()
            .write_rows(
                summary.id,
                0,
                &[row("user", "Refactor the parser"), row("assistant", "Done")],
            )
            .unwrap();
        summary.has_messages = true;
        summary
    }

    /// An open, loaded conversation, and whether it was busy each time it said it changed.
    fn open(
        engine: &Engine,
        summary: Session,
        cx: &mut TestAppContext,
    ) -> (Entity<Conversation>, Rc<RefCell<Vec<bool>>>) {
        let view = cx.new(|cx| Conversation::new(summary, engine.clone(), cx));
        let busy = Rc::new(RefCell::new(Vec::new()));
        let seen = busy.clone();
        cx.update(|cx| {
            cx.subscribe(&view, move |view, _: &SessionChanged, cx| {
                seen.borrow_mut().push(view.read(cx).busy);
            })
            .detach();
        });
        view.update(cx, |view, cx| view.load(cx));
        cx.run_until_parked();
        busy.borrow_mut().clear();
        (view, busy)
    }

    fn prompts(view: &Entity<Conversation>, cx: &mut TestAppContext) -> Vec<String> {
        view.read_with(cx, |view, cx| {
            view.transcript
                .read(cx)
                .source_rows()
                .iter()
                .filter(|row| row.first && row.is_prompt())
                .map(|row| row.text.clone())
                .collect()
        })
    }

    #[gpui::test]
    fn a_usage_limit_resume_appears_live_in_an_open_conversation(cx: &mut TestAppContext) {
        let engine = cx.update(Engine::memory);
        let summary = session(&engine);
        let id = summary.id;
        let now = i64::try_from(onicode_protocol::time::now_ms()).unwrap();
        engine
            .seed()
            .save_agent_settings(
                id,
                &Settings {
                    usage_limit: Some(Limit {
                        resets_at: Some(now - 60_000),
                        resume_at_reset: true,
                    }),
                    ..Settings::default()
                },
            )
            .unwrap();
        let (view, busy) = open(&engine, summary, cx);
        view.read_with(cx, |view, _| {
            assert!(view.settings.usage_limit.is_some(), "the banner shows");
            assert!(!view.busy);
        });

        // The engine's beat sends the continuation; nobody touches the view.
        cx.update(|cx| crate::engine_client::tick(&engine, onicode_protocol::time::now_ms(), cx))
            .unwrap();
        cx.run_until_parked();

        assert_eq!(
            prompts(&view, cx),
            ["Refactor the parser", "Continue from where you left off."]
        );
        view.read_with(cx, |view, cx| {
            assert_eq!(
                view.transcript.read(cx).source_rows(),
                engine.seed().rows(id).unwrap(),
                "the view shows what the engine saved"
            );
            assert!(!view.busy);
            assert_eq!(
                view.settings.usage_limit, None,
                "the banner went with the resume"
            );
        });
        let busy = busy.borrow();
        // The engine lifts the limit as it sends the continuation, so the banner may go a
        // change before the turn shows.
        assert!(busy.contains(&true), "the view ran the turn: {busy:?}");
        assert_eq!(busy.last(), Some(&false), "and saw it end: {busy:?}");
    }

    #[gpui::test]
    fn a_turn_sent_from_another_window_appears_live(cx: &mut TestAppContext) {
        let engine = cx.update(Engine::memory);
        let summary = session(&engine);
        let id = summary.id;
        let (watching, busy) = open(&engine, summary.clone(), cx);
        let (sending, sent) = open(&engine, summary, cx);

        sending.update(cx, |view, cx| {
            view.input.update(cx, |input, cx| {
                input.set_text("From the other window".into(), cx)
            });
            view.send(cx);
        });
        cx.run_until_parked();

        let expected = ["Refactor the parser", "From the other window"];
        assert_eq!(prompts(&watching, cx), expected);
        assert_eq!(
            prompts(&sending, cx),
            expected,
            "the sender shows its turn once"
        );
        let stored = engine.seed().rows(id).unwrap();
        for view in [&watching, &sending] {
            view.read_with(cx, |view, cx| {
                assert_eq!(view.transcript.read(cx).source_rows(), stored);
                assert!(!view.busy);
            });
        }
        let busy = busy.borrow();
        assert_eq!(
            busy.first(),
            Some(&true),
            "the watching window ran the turn: {busy:?}"
        );
        assert_eq!(busy.last(), Some(&false), "and saw it end: {busy:?}");
        // The sender followed its own turn, and never joined it a second time.
        let sent = sent.borrow();
        assert_eq!(
            sent.iter().filter(|busy| **busy).count(),
            sent.iter().take_while(|busy| **busy).count(),
            "the sender went busy once: {sent:?}"
        );
    }

    #[gpui::test]
    fn the_banner_follows_another_window_disarming_the_resume(cx: &mut TestAppContext) {
        let engine = cx.update(Engine::memory);
        let summary = session(&engine);
        let limit = Limit {
            resets_at: Some(i64::try_from(onicode_protocol::time::now_ms()).unwrap() + 3_600_000),
            resume_at_reset: true,
        };
        engine
            .seed()
            .save_agent_settings(
                summary.id,
                &Settings {
                    usage_limit: Some(limit.clone()),
                    ..Settings::default()
                },
            )
            .unwrap();
        let (watching, _) = open(&engine, summary.clone(), cx);
        let (other, _) = open(&engine, summary, cx);

        other.update(cx, |view, cx| view.set_usage_resume_at_reset(false, cx));
        cx.run_until_parked();

        watching.read_with(cx, |view, _| {
            assert_eq!(
                view.settings.usage_limit,
                Some(Limit {
                    resume_at_reset: false,
                    ..limit
                }),
                "the banner shows the resume disarmed"
            );
        });
    }
}
