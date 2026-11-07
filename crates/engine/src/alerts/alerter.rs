//! The decision: which causes become alerts, and when they are taken back. Pure, on a clock of
//! Unix milliseconds the caller passes, so the property tests drive it in virtual time.
//!
//! - A chat or terminal is seen first silently ([`Alerter::seed`]): a boot, a resync or a terminal
//!   that appears never rings for what already stood.
//! - A cause is held [`HOLD_MS`] before it is raised, and dropped if it no longer stands then; a
//!   dropped cause is never replayed, so a flicker never rings.
//! - One alert per key: a newer raise replaces it. It is withdrawn once its cause ends, the key is
//!   on screen in a focused window, the key turns quiet (a side or archived chat) or goes.
//! - A terminal's program is outside code: its raises are limited to one per terminal every
//!   [`EXTERNAL_GAP_MS`] and [`EXTERNAL_BURST`] across terminals every [`EXTERNAL_WINDOW_MS`].
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use onicode_protocol::TerminalId;
use onicode_protocol::alerts::AlertKind;

/// How long a cause must stand before it is raised.
pub(crate) const HOLD_MS: u64 = 1_500;
/// How long a running chat may go without any event before it counts as stalled.
pub(crate) const STALL_MS: u64 = 15 * 60_000;
/// The least time between two raises of one terminal.
pub(crate) const EXTERNAL_GAP_MS: u64 = 30_000;
/// The most raises of terminals in [`EXTERNAL_WINDOW_MS`].
pub(crate) const EXTERNAL_BURST: usize = 4;
pub(crate) const EXTERNAL_WINDOW_MS: u64 = 60_000;

/// What an alert belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Key {
    /// A chat, by the engine's row id.
    Session(i64),
    Terminal(TerminalId),
}

impl Key {
    /// Raised from a program's own reports, so rate-limited.
    fn external(self) -> bool {
        matches!(self, Self::Terminal(_))
    }
}

/// What a chat or terminal is doing, as far as alerts care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Idle,
    Working,
    NeedsYou,
    /// Its last run failed.
    Failed,
    /// Its last run finished.
    Done,
}

/// One key's facts at a moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fact {
    pub(crate) phase: Phase,
    /// Names what the owner is asked: a different question while still asking raises again.
    pub(crate) need: Option<String>,
    /// Counts the runs that ended: one more ending raises again.
    pub(crate) ended: u64,
    /// Nobody has looked since its last run ended.
    pub(crate) unseen: bool,
    /// When anything last happened to it.
    pub(crate) active_at: u64,
    /// A side or archived chat: it never alerts.
    pub(crate) quiet: bool,
}

impl Fact {
    /// Nothing happening.
    #[cfg(test)]
    pub(crate) fn idle(now: u64) -> Self {
        Self {
            phase: Phase::Idle,
            need: None,
            ended: 0,
            unseen: false,
            active_at: now,
            quiet: false,
        }
    }
}

/// What a step changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// Show an alert of `kind` for `key`, replacing whatever shows for it.
    Raise { key: Key, kind: AlertKind },
    /// Take back what shows for `key`.
    Withdraw { key: Key },
}

#[derive(Clone, Copy, Debug)]
struct Held {
    kind: AlertKind,
    since: u64,
}

/// Whether a `kind` alert's cause still stands in `fact` at `now`.
pub(crate) fn stands(kind: AlertKind, fact: &Fact, now: u64) -> bool {
    match kind {
        AlertKind::NeedsYou => fact.phase == Phase::NeedsYou,
        AlertKind::Failed => fact.phase == Phase::Failed && fact.unseen,
        AlertKind::Finished => fact.phase == Phase::Done && fact.unseen,
        AlertKind::Stalled => stalled(fact, now),
    }
}

fn stalled(fact: &Fact, now: u64) -> bool {
    fact.phase == Phase::Working && now.saturating_sub(fact.active_at) >= STALL_MS
}

/// What moving from `prev` to `next` raises.
fn raised(prev: &Fact, next: &Fact) -> Option<AlertKind> {
    match next.phase {
        Phase::NeedsYou if prev.phase != Phase::NeedsYou || prev.need != next.need => {
            Some(AlertKind::NeedsYou)
        }
        Phase::Failed if prev.phase != Phase::Failed || prev.ended != next.ended => {
            Some(AlertKind::Failed)
        }
        Phase::Done if prev.phase != Phase::Done || prev.ended != next.ended => {
            Some(AlertKind::Finished)
        }
        _ => None,
    }
}

/// The engine's alert decisions. Feed it every fact, the keys on screen and every
/// [`Self::next_deadline`]; it shows nothing itself.
#[derive(Debug, Default)]
pub(crate) struct Alerter {
    facts: BTreeMap<Key, Fact>,
    held: BTreeMap<Key, Held>,
    live: BTreeMap<Key, AlertKind>,
    looking: BTreeSet<Key>,
    /// The `active_at` each running chat's stall was taken for, so one silence stalls once.
    stalls: BTreeMap<Key, u64>,
    /// When each terminal last raised.
    external: BTreeMap<Key, u64>,
    /// When terminals raised in the last window.
    burst: VecDeque<u64>,
}

impl Alerter {
    /// The alerts showing now.
    #[cfg(test)]
    pub(crate) fn live(&self) -> &BTreeMap<Key, AlertKind> {
        &self.live
    }

    /// The facts last taken in for `key`.
    #[cfg(test)]
    pub(crate) fn fact(&self, key: Key) -> Option<&Fact> {
        self.facts.get(&key)
    }

    /// Takes in `fact` without raising anything for it: what already stood when it was first
    /// seen, or after a resync.
    pub(crate) fn seed(&mut self, key: Key, fact: Fact) {
        self.facts.insert(key, fact);
    }

    /// Takes in `key`'s facts at `now`. A key seen for the first time seeds silently.
    pub(crate) fn observe(&mut self, key: Key, fact: Fact, now: u64) {
        let Some(prev) = self.facts.insert(key, fact.clone()) else {
            return;
        };
        if let Some(kind) = raised(&prev, &fact) {
            self.held.insert(key, Held { kind, since: now });
        }
        if self.looking.contains(&key) {
            self.held.remove(&key);
        }
    }

    /// `key` is gone; what shows for it is withdrawn at the next step.
    pub(crate) fn forget(&mut self, key: Key) {
        self.facts.remove(&key);
        self.held.remove(&key);
        self.stalls.remove(&key);
        self.external.remove(&key);
    }

    /// The keys on screen in a focused window now: they never alert, and what shows for them is
    /// withdrawn at the next step.
    pub(crate) fn look(&mut self, looking: BTreeSet<Key>) {
        for key in &looking {
            self.held.remove(key);
        }
        self.looking = looking;
    }

    /// When the next held cause is due, or a running chat would stall.
    pub(crate) fn next_deadline(&self) -> Option<u64> {
        let held = self
            .held
            .values()
            .map(|held| held.since.saturating_add(HOLD_MS));
        let stalls = self
            .facts
            .iter()
            .filter(|(key, fact)| {
                matches!(key, Key::Session(_))
                    && fact.phase == Phase::Working
                    && self.stalls.get(*key) != Some(&fact.active_at)
            })
            .map(|(_, fact)| fact.active_at.saturating_add(STALL_MS));
        held.chain(stalls).min()
    }

    /// Everything due at `now`: withdrawals first, then raises.
    pub(crate) fn step(&mut self, now: u64) -> Vec<Change> {
        let mut changes = Vec::new();
        let ended: Vec<Key> = self
            .live
            .iter()
            .filter(|(key, kind)| {
                !self.facts.get(key).is_some_and(|fact| {
                    !fact.quiet && !self.looking.contains(key) && stands(**kind, fact, now)
                })
            })
            .map(|(key, _)| *key)
            .collect();
        for key in ended {
            self.live.remove(&key);
            changes.push(Change::Withdraw { key });
        }
        let silent: Vec<(Key, u64)> = self
            .facts
            .iter()
            .filter(|(key, fact)| {
                matches!(key, Key::Session(_))
                    && stalled(fact, now)
                    && self.stalls.get(*key) != Some(&fact.active_at)
            })
            .map(|(key, fact)| (*key, fact.active_at))
            .collect();
        for (key, active_at) in silent {
            self.stalls.insert(key, active_at);
            if !self.looking.contains(&key) {
                self.held.insert(
                    key,
                    Held {
                        kind: AlertKind::Stalled,
                        since: now,
                    },
                );
            }
        }
        let due: Vec<Key> = self
            .held
            .iter()
            .filter(|(_, held)| now >= held.since.saturating_add(HOLD_MS))
            .map(|(key, _)| *key)
            .collect();
        for key in due {
            let Some(held) = self.held.remove(&key) else {
                continue;
            };
            let Some(fact) = self.facts.get(&key) else {
                continue;
            };
            if fact.quiet || self.looking.contains(&key) || !stands(held.kind, fact, now) {
                continue;
            }
            if key.external() && !self.admit(key, now) {
                continue;
            }
            self.live.insert(key, held.kind);
            changes.push(Change::Raise {
                key,
                kind: held.kind,
            });
        }
        changes
    }

    /// Whether a terminal may raise at `now`, counting it if so.
    fn admit(&mut self, key: Key, now: u64) -> bool {
        while self
            .burst
            .front()
            .is_some_and(|at| now.saturating_sub(*at) >= EXTERNAL_WINDOW_MS)
        {
            self.burst.pop_front();
        }
        let recent = self
            .external
            .get(&key)
            .is_some_and(|at| now.saturating_sub(*at) < EXTERNAL_GAP_MS);
        if recent || self.burst.len() >= EXTERNAL_BURST {
            return false;
        }
        self.external.insert(key, now);
        self.burst.push_back(now);
        true
    }
}
