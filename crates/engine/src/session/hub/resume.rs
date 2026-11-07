//! The usage-limit auto-resume. A session whose limit is armed is sent the continuation once its
//! reset has passed, by the engine alone on its scheduler beat: it happens with no viewer open,
//! and once however many viewers there are.
//!
//! A resume is kept in the store (`RESUMES`) from before its limit is cleared until its turn
//! ends, so an engine that stops in between finds it on its next start (the lesson of t3code
//! #31292, whose resume reused its op and so never came back after a restart). One the provider
//! never spoke in is armed again, the copy of its prompt dropped, and sent anew under a fresh op;
//! one the provider spoke in is never sent again.
use std::collections::BTreeMap;
use std::sync::Arc;

use onicode_core::provider::Kind;
use onicode_core::transcript::Row;
use onicode_core::usage::Limit;
use onicode_protocol::session::{self, SessionEvent};
use onicode_protocol::{MessageId, OpId, SessionId};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use super::{Inner, SessionHub, publish};
use crate::turn::{Update, locked};

/// What the engine sends once a limit it resumes has reset.
pub(crate) const CONTINUE: &str = "Continue from where you left off.";
/// The store key the resumes in flight are kept under.
pub(crate) const RESUMES: &str = "engine-usage-resumes";

/// A resume in flight.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Resuming {
    /// The armed limit the resume took: put back when the provider never had the continuation.
    limit: Limit,
    /// Where the continuation's prompt starts in the transcript.
    at: usize,
    /// The provider spoke in the resume's turn: it is never sent again.
    started: bool,
}

/// The resumes in flight, by the session's store row.
pub(crate) type Resumes = BTreeMap<i64, Resuming>;

fn ms(now: u64) -> i64 {
    i64::try_from(now).unwrap_or(i64::MAX)
}

/// Whether the provider showed it had the turn: it got ready, named its turn or thread, or wrote
/// something besides the prompt.
pub(super) fn spoke(update: &Update) -> bool {
    update.turn_ready
        || update.provider_turn_id.is_some()
        || update.provider_id.is_some()
        || update.model.is_some()
        || (!update.finished
            && update
                .rows
                .iter()
                .any(|row| !row.is_prompt() && !row.text.trim().is_empty()))
}

/// The usage limit a viewer's settings store: a viewer arms or disarms the limit the engine
/// holds, and nothing else. Only the engine records a limit (a turn ended at it) and clears one (a
/// turn or its resume started), so a viewer whose copy is older than either, saving it for any
/// reason, can neither bring back an armed limit a resume took nor drop one a turn just recorded.
/// A copy of an older limit (another reset) leaves the newer one's arming alone.
pub(super) fn viewer_limit(held: Option<Limit>, asked: Option<Limit>) -> Option<Limit> {
    let mut held = held?;
    if let Some(asked) = asked
        && (asked.resets_at.is_none() || asked.resets_at == held.resets_at)
    {
        held.resume_at_reset = asked.resume_at_reset;
    }
    Some(held)
}

/// The prompt that starts at `at`, its rows' text joined.
fn prompt_at(rows: &[Row], at: usize) -> Option<String> {
    let first = rows.get(at).filter(|row| row.is_prompt())?;
    let mut text = first.text.clone();
    for row in rows[at + 1..]
        .iter()
        .take_while(|row| row.is_prompt() && !row.first)
    {
        text.push_str(&row.text);
    }
    Some(text)
}

/// Whether anything but the prompt was written from `at` on: the provider answered.
fn answered(rows: &[Row], at: usize) -> bool {
    rows.iter()
        .skip(at)
        .any(|row| !row.is_prompt() && !row.text.trim().is_empty())
}

impl SessionHub {
    /// One beat of the usage-limit resumes at `now` (Unix ms): each local session whose limit is
    /// armed and due, with no turn, rewind or compaction running, is sent the continuation once.
    /// The first beat of an engine run first recovers the resumes an earlier run left in flight.
    pub fn resume_limits(self: &Arc<Self>, now: u64) -> Result<(), String> {
        let mut inner = self.lock();
        let mut resumes = match inner.resumes.take() {
            Some(resumes) => resumes,
            None => self.recover(&mut inner)?,
        };
        let armed = locked(&self.store).and_then(|store| store.armed_usage_limits());
        let mut failed = armed.as_ref().err().cloned();
        for row in armed.unwrap_or_default() {
            if resumes.contains_key(&row) {
                continue;
            }
            if let Err(error) = self.resume(&mut inner, &mut resumes, row, now) {
                failed.get_or_insert(error);
            }
        }
        inner.resumes = Some(resumes);
        failed.map_or(Ok(()), Err)
    }

    /// Settles the resumes an earlier engine run left in flight: one whose provider never spoke
    /// is armed again with its prompt dropped, so this run sends it anew; one it spoke in, or
    /// that the conversation went on past, is done. One that cannot be settled now waits for the
    /// next run.
    fn recover(&self, inner: &mut Inner) -> Result<Resumes, String> {
        let raw = locked(&self.store)?.setting(RESUMES)?;
        let Some(raw) = raw else {
            return Ok(Resumes::new());
        };
        let held: Resumes = serde_json::from_str(&raw)
            .map_err(|error| format!("Cannot read the usage-limit resumes: {error}"))?;
        let mut unsettled = Resumes::new();
        for (row, resuming) in held {
            // A turn this run already started in the session continued it.
            let running = inner
                .sessions
                .values()
                .any(|live| live.id == row && live.turn.is_some());
            if resuming.started || running {
                continue;
            }
            match self.rearm_unsent(row, &resuming) {
                Ok(None) => {}
                Ok(Some(dropped)) => {
                    if let Some(live) = inner.sessions.values_mut().find(|live| live.id == row) {
                        if dropped {
                            live.prompts.retain(|at, _| *at < resuming.at);
                        }
                        self.refresh(live)?;
                        publish(
                            live,
                            vec![SessionEvent::UsageLimit(Some(resuming.limit.clone()))],
                        );
                    }
                }
                Err(_) => {
                    unsettled.insert(row, resuming);
                }
            }
        }
        // Held in memory too, so this run neither sends them nor writes over them.
        self.keep(&unsettled)?;
        Ok(unsettled)
    }

    /// Arms again the limit of an earlier run's resume of `row` the provider never had, dropping
    /// the copy of its prompt: whether a copy was dropped, or `None` when the resume is done.
    fn rearm_unsent(&self, row: i64, resuming: &Resuming) -> Result<Option<bool>, String> {
        let store = locked(&self.store)?;
        let Some(mut settings) = store.agent_settings(row)? else {
            return Ok(None);
        };
        let rows = store.load(row)?.rows;
        let copy = prompt_at(&rows, resuming.at).is_some_and(|text| text == CONTINUE);
        // The provider answered it, or the conversation went on past it.
        let later = rows
            .iter()
            .skip(resuming.at + 1)
            .any(|row| row.first && row.is_prompt());
        if answered(&rows, resuming.at) || later || (rows.len() > resuming.at && !copy) {
            return Ok(None);
        }
        if copy {
            store.drop_prompt(row, resuming.at)?;
        }
        settings.usage_limit = Some(resuming.limit.clone());
        store.save_agent_settings(row, &settings)?;
        Ok(Some(copy))
    }

    /// Resumes session `row` if its limit is armed and due at `now`.
    fn resume(
        self: &Arc<Self>,
        inner: &mut Inner,
        resumes: &mut Resumes,
        row: i64,
        now: u64,
    ) -> Result<(), String> {
        let (session, mut settings, provider) = {
            let store = locked(&self.store)?;
            let Some(settings) = store.agent_settings(row)? else {
                return Ok(());
            };
            let session = SessionId(store.session_uid(row)?);
            (session, settings, store.session_provider(row)?)
        };
        let Some(mut limit) = settings
            .usage_limit
            .clone()
            .filter(|limit| limit.resume_at_reset)
        else {
            return Ok(());
        };
        let mut events = Vec::new();
        // The provider named no reset: the account's usage says when its windows reset.
        if limit.resets_at.is_none()
            && let (Some(accounts), Some(provider)) = (self.accounts.get(), provider.as_deref())
            && let Some(at) = accounts.exhausted_reset(
                Kind::from_id(provider),
                &settings.account.id,
                settings.model.as_deref(),
            )
        {
            limit.resets_at = Some(at);
            settings.usage_limit = Some(limit.clone());
            locked(&self.store)?.save_agent_settings(row, &settings)?;
            events.push(SessionEvent::UsageLimit(Some(limit.clone())));
        }
        let busy = inner.rewinding.contains(&session)
            || inner.compacting.contains_key(&session)
            || inner
                .sessions
                .get(&session)
                .is_some_and(|live| live.turn.is_some());
        if !limit.resume_due(ms(now), busy) {
            if !events.is_empty()
                && let Some(live) = inner.sessions.get_mut(&session)
            {
                publish(live, events);
            }
            return Ok(());
        }
        {
            let Inner {
                sessions,
                next_epoch,
                ..
            } = &mut *inner;
            let live = self.live(sessions, next_epoch, session)?;
            if !live.fresh {
                self.refresh(live)?;
                live.fresh = true;
            }
            let at = live.topic.state().first_row + live.topic.state().rows.len();
            // Kept before the limit is cleared: a stop between the two finds it to send again.
            resumes.insert(
                row,
                Resuming {
                    limit: limit.clone(),
                    at,
                    started: false,
                },
            );
            if let Err(error) = self.keep(resumes) {
                resumes.remove(&row);
                return Err(error);
            }
            settings.usage_limit = None;
            if let Err(error) =
                locked(&self.store).and_then(|store| store.save_agent_settings(row, &settings))
            {
                resumes.remove(&row);
                let _ = self.keep(resumes);
                return Err(error);
            }
            events.push(SessionEvent::UsageLimit(None));
            events.push(SessionEvent::Settings(Box::new(settings)));
            publish(live, events);
        }
        // A fresh op and message for every attempt: a retry is never taken for the one before.
        let (resolve, _) = watch::channel(None);
        let body = session::Send {
            session,
            message: MessageId::new(),
            text: CONTINUE.into(),
            attachments: Vec::new(),
            turn: None,
        };
        match self.launch(inner, Some(OpId::new()), body, &mut Some(resolve)) {
            Ok(()) => {
                if let Some(turn) = inner
                    .sessions
                    .get_mut(&session)
                    .and_then(|live| live.turn.as_mut())
                {
                    turn.resume = true;
                }
                Ok(())
            }
            Err(error) => {
                if let Some(event) = self.rearm(resumes, row)
                    && let Some(live) = inner.sessions.get_mut(&session)
                {
                    publish(live, vec![event]);
                }
                // Loaded for the resume alone: nothing holds it now.
                if inner
                    .sessions
                    .get(&session)
                    .is_some_and(|live| live.turn.is_none() && !live.topic.is_watched())
                {
                    inner.sessions.remove(&session);
                }
                Err(error)
            }
        }
    }

    /// Writes the resumes in flight to the store.
    fn keep(&self, resumes: &Resumes) -> Result<(), String> {
        let store = locked(&self.store)?;
        if resumes.is_empty() {
            store.delete_setting(RESUMES)
        } else {
            let raw = serde_json::to_string(resumes).map_err(|error| error.to_string())?;
            store.save_setting(RESUMES, &raw)
        }
    }

    /// Puts back the limit of the resume of `row`, which never reached the provider, and forgets
    /// the resume: the next beat sends it under a new op.
    fn rearm(&self, resumes: &mut Resumes, row: i64) -> Option<SessionEvent> {
        let resuming = resumes.remove(&row)?;
        let armed = locked(&self.store).and_then(|store| {
            let Some(mut settings) = store.agent_settings(row)? else {
                return Ok(None);
            };
            if settings.usage_limit.is_some() {
                return Ok(None);
            }
            settings.usage_limit = Some(resuming.limit.clone());
            store.save_agent_settings(row, &settings)?;
            Ok(Some(SessionEvent::UsageLimit(Some(resuming.limit))))
        });
        // A failed write leaves the stored resume for the next engine run to recover.
        let event = armed.ok()?;
        let _ = self.keep(resumes);
        event
    }

    /// The provider spoke in the resume of `row`: it is never sent again.
    pub(super) fn resume_spoke(&self, resumes: &mut Option<Resumes>, row: i64) {
        let Some(resumes) = resumes.as_mut() else {
            return;
        };
        let Some(resuming) = resumes.get_mut(&row).filter(|resuming| !resuming.started) else {
            return;
        };
        resuming.started = true;
        let _ = self.keep(resumes);
    }

    /// The resume's turn ended. One whose prompt never got in (refused before it was saved) is
    /// armed again unless it was stopped; any other is done.
    pub(super) fn resume_ended(
        &self,
        resumes: &mut Option<Resumes>,
        row: i64,
        delivered: bool,
        stopped: bool,
    ) -> Option<SessionEvent> {
        let resumes = resumes.as_mut()?;
        if !delivered && !stopped {
            return self.rearm(resumes, row);
        }
        resumes.remove(&row)?;
        let _ = self.keep(resumes);
        None
    }

    /// A turn of `row` ended at `limit`: the engine holds it until a turn continues past it.
    /// `reported` says the turn's own events already carry it.
    pub(super) fn hold_limit(
        &self,
        row: i64,
        mut limit: Limit,
        reported: bool,
    ) -> Vec<SessionEvent> {
        let held = locked(&self.store).and_then(|store| {
            let Some(mut settings) = store.agent_settings(row)? else {
                return Ok(None);
            };
            limit.resume_at_reset = settings
                .usage_limit
                .as_ref()
                .is_some_and(|held| held.resume_at_reset);
            settings.usage_limit = Some(limit.clone());
            store.save_agent_settings(row, &settings)?;
            Ok(Some(settings))
        });
        let Ok(Some(settings)) = held else {
            return Vec::new();
        };
        let mut events = Vec::new();
        if !reported || limit.resume_at_reset {
            events.push(SessionEvent::UsageLimit(Some(limit)));
        }
        events.push(SessionEvent::Settings(Box::new(settings)));
        events
    }

    /// A turn of `row` starts: the limit it continues past goes, armed or not.
    pub(super) fn lift_limit(&self, row: i64) -> Result<Option<SessionEvent>, String> {
        let store = locked(&self.store)?;
        let Some(mut settings) = store.agent_settings(row)? else {
            return Ok(None);
        };
        if settings.usage_limit.take().is_none() {
            return Ok(None);
        }
        store.save_agent_settings(row, &settings)?;
        Ok(Some(SessionEvent::UsageLimit(None)))
    }
}
