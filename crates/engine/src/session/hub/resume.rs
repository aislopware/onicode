//! The usage-limit auto-resume. A session whose limit is armed is sent the continuation once its
//! reset has passed, by the engine alone on its scheduler beat: it happens with no viewer open,
//! and once however many viewers there are.
//!
//! A resume is kept in the store (`RESUMES`) from before its limit is cleared until its turn
//! ends, so an engine that stops in between finds it on its next start (the lesson of t3code
//! #31292, whose resume reused its op and so never came back after a restart). One the provider
//! never spoke in is armed again, the copy of its prompt dropped, and sent anew under a fresh op;
//! one the provider spoke in is never sent again.
//!
//! A resume refused before its prompt was saved (the provider's CLI is missing, the folder is
//! gone, the account signed out) is not retried silently on every beat. The conversation says so
//! in an error row, as monocode shows any turn that failed, with the reason and when the engine
//! tries again; each later refusal rewrites that one row. The waits double from 30 s up to
//! 15 min, and once the next try would fall past one limit window (5 h) after the reset, the
//! engine stops: the limit stays for the banner, disarmed, and the row says to choose Resume.
//! The waits are counted from the scheduler beat that sent the resume, so the clock is the
//! beat's (tests run it in virtual time); only this run remembers them, and a restart tries at
//! once.
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
/// The wait after a first refused resume (ms): the next beat.
const RETRY_FIRST: i64 = 30_000;
/// The longest wait between refused resumes (ms).
const RETRY_MAX: i64 = 15 * 60_000;
/// How long after its reset a refused resume is tried again (ms): one usage window, the
/// five hours of Claude's and Codex's session windows. Past it the next window's limit, not
/// this one, is what the conversation waits on.
const LIMIT_WINDOW: i64 = 5 * 60 * 60_000;
/// How the error row a refused resume leaves begins.
const REFUSED: &str = "Could not resume after the usage limit";
/// How much of a refusal's reason its row shows.
const REASON_CHARS: usize = 400;

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

/// The resumes refused in a row for one armed limit. Only this run keeps them.
#[derive(Clone, Debug)]
struct Refused {
    /// The reset of the limit whose resumes were refused: another limit starts afresh.
    resets_at: Option<i64>,
    /// How many were refused in a row.
    count: u32,
    /// When the next may be sent (Unix ms).
    retry_at: i64,
}

/// The usage-limit resumes: the ones in flight by the session's store row, as the store keeps
/// them, and the refusals this run has seen.
#[derive(Debug, Default)]
pub(crate) struct Resumes {
    flight: BTreeMap<i64, Resuming>,
    refused: BTreeMap<i64, Refused>,
    /// The latest beat (Unix ms): a refusal between beats counts its wait from it.
    beat: i64,
}

impl Resumes {
    /// Whether the refusals of `row`'s resumes of `limit` hold the next one back at `now`.
    fn held_back(&mut self, row: i64, limit: &Limit, now: i64) -> bool {
        match self.refused.get(&row) {
            Some(refused) if refused.resets_at != limit.resets_at => {
                self.refused.remove(&row);
                false
            }
            Some(refused) => now < refused.retry_at,
            None => false,
        }
    }
}

/// The wait after the `count`th refusal in a row: 30 s, doubling, at most 15 min.
fn retry_wait(count: u32) -> i64 {
    let doublings = count.saturating_sub(1).min(5);
    (RETRY_FIRST << doublings).min(RETRY_MAX)
}

/// `wait` (ms) as the error row says it.
fn spoken_wait(wait: i64) -> String {
    if wait < 60_000 {
        format!("{} s", wait / 1_000)
    } else {
        format!("{} min", wait / 60_000)
    }
}

/// The error row a refused resume leaves: why, and what the engine does next (`retry`, a wait
/// in ms, or `None` once it stopped).
fn refused_text(reason: &str, retry: Option<i64>) -> String {
    // One line, short enough that the row stays one row (`ROW_BYTES`).
    let reason = reason.lines().next().unwrap_or_default();
    let reason = match reason.char_indices().nth(REASON_CHARS) {
        Some((end, _)) => &reason[..end],
        None => reason,
    };
    let reason = reason.trim().trim_end_matches('.');
    let mut text = REFUSED.to_owned();
    // A refusal the turn gave no reason for ends as "Ready".
    if !reason.is_empty() && reason != "Ready" {
        text.push_str(": ");
        text.push_str(reason);
    }
    text.push_str(". ");
    match retry {
        Some(wait) => {
            text.push_str("Trying again in ");
            text.push_str(&spoken_wait(wait));
            text.push('.');
        }
        None => text.push_str("Automatic resume stopped; choose Resume to try again."),
    }
    text
}

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
        resumes.beat = ms(now);
        let armed = locked(&self.store).and_then(|store| store.armed_usage_limits());
        let mut failed = armed.as_ref().err().cloned();
        for row in armed.unwrap_or_default() {
            if resumes.flight.contains_key(&row) {
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
            return Ok(Resumes::default());
        };
        let held: BTreeMap<i64, Resuming> = serde_json::from_str(&raw)
            .map_err(|error| format!("Cannot read the usage-limit resumes: {error}"))?;
        let mut unsettled = BTreeMap::new();
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
        Ok(Resumes {
            flight: unsettled,
            ..Resumes::default()
        })
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

    /// Resumes session `row` if its limit is armed and due at `now`, and no refusal holds it
    /// back.
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
        if !limit.resume_due(ms(now), busy) || resumes.held_back(row, &limit, ms(now)) {
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
            resumes.flight.insert(
                row,
                Resuming {
                    limit: limit.clone(),
                    at,
                    started: false,
                },
            );
            if let Err(error) = self.keep(&resumes.flight) {
                resumes.flight.remove(&row);
                return Err(error);
            }
            settings.usage_limit = None;
            if let Err(error) =
                locked(&self.store).and_then(|store| store.save_agent_settings(row, &settings))
            {
                resumes.flight.remove(&row);
                let _ = self.keep(&resumes.flight);
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
                let events = self.refuse(resumes, row, &error);
                if !events.is_empty()
                    && let Some(live) = inner.sessions.get_mut(&session)
                {
                    publish(live, events);
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
    fn keep(&self, flight: &BTreeMap<i64, Resuming>) -> Result<(), String> {
        let store = locked(&self.store)?;
        if flight.is_empty() {
            store.delete_setting(RESUMES)
        } else {
            let raw = serde_json::to_string(flight).map_err(|error| error.to_string())?;
            store.save_setting(RESUMES, &raw)
        }
    }

    /// The resume of `row` was refused before its prompt was saved, for `reason`. Its limit is
    /// put back and the conversation's error row says why and when the engine tries again (an
    /// earlier refusal's row rewritten, while nothing came after it); once the next try would
    /// fall past the limit window, the limit is put back disarmed and the row says the engine
    /// stopped. Returns what the session's watchers are told.
    fn refuse(&self, resumes: &mut Resumes, row: i64, reason: &str) -> Vec<SessionEvent> {
        let Some(resuming) = resumes.flight.remove(&row) else {
            return Vec::new();
        };
        let limit = resuming.limit;
        let last = resumes
            .refused
            .get(&row)
            .filter(|refused| refused.resets_at == limit.resets_at);
        let count = last.map_or(1, |refused| refused.count.saturating_add(1));
        let wait = retry_wait(count);
        let retry_at = resumes.beat.saturating_add(wait);
        let stop = limit
            .resets_at
            .is_some_and(|reset| retry_at > reset.saturating_add(LIMIT_WINDOW));
        let sent_at = u64::try_from(resumes.beat).ok();
        let written = locked(&self.store).and_then(|mut store| {
            let Some(mut settings) = store.agent_settings(row)? else {
                return Ok(None);
            };
            let rows = store.load(row)?.rows;
            // An earlier refusal's row, while nothing came after it, is rewritten.
            let at = match rows.last() {
                Some(last) if last.role == "error" && last.text.starts_with(REFUSED) => {
                    rows.len() - 1
                }
                _ => rows.len(),
            };
            let error = Row {
                role: "error".into(),
                text: refused_text(reason, (!stop).then_some(wait)),
                first: true,
                sent_at,
                id: None,
            };
            store.write_rows(row, at, std::slice::from_ref(&error))?;
            let mut held = limit.clone();
            held.resume_at_reset = !stop;
            // A limit the engine holds now is a later turn's: it stays as it is.
            let put = settings.usage_limit.is_none();
            if put {
                settings.usage_limit = Some(held.clone());
                store.save_agent_settings(row, &settings)?;
            }
            Ok(Some((at, error, put.then_some(held))))
        });
        // A failed write leaves the stored resume for the next engine run to recover.
        let Ok(Some((at, error, held))) = written else {
            return Vec::new();
        };
        if stop {
            resumes.refused.remove(&row);
        } else {
            resumes.refused.insert(
                row,
                Refused {
                    resets_at: limit.resets_at,
                    count,
                    retry_at,
                },
            );
        }
        let _ = self.keep(&resumes.flight);
        let mut events = vec![SessionEvent::Rows {
            start: at,
            rows: vec![error],
        }];
        if let Some(held) = held {
            events.push(SessionEvent::UsageLimit(Some(held)));
        }
        events
    }

    /// The provider spoke in the resume of `row`: it is never sent again.
    pub(super) fn resume_spoke(&self, resumes: &mut Option<Resumes>, row: i64) {
        let Some(resumes) = resumes.as_mut() else {
            return;
        };
        let Some(resuming) = resumes
            .flight
            .get_mut(&row)
            .filter(|resuming| !resuming.started)
        else {
            return;
        };
        resuming.started = true;
        resumes.refused.remove(&row);
        let _ = self.keep(&resumes.flight);
    }

    /// The resume's turn ended. One whose prompt never got in (refused before it was saved, for
    /// `reason`, the turn's last status) is put back unless it was stopped (`refuse`); any other
    /// is done.
    pub(super) fn resume_ended(
        &self,
        resumes: &mut Option<Resumes>,
        row: i64,
        delivered: bool,
        stopped: bool,
        reason: &str,
    ) -> Vec<SessionEvent> {
        let Some(resumes) = resumes.as_mut() else {
            return Vec::new();
        };
        if !delivered && !stopped {
            return self.refuse(resumes, row, reason);
        }
        resumes.refused.remove(&row);
        if resumes.flight.remove(&row).is_some() {
            let _ = self.keep(&resumes.flight);
        }
        Vec::new()
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

#[cfg(test)]
mod tests;
