//! One watcher's queue (engine.md "Backpressure"): bounded at 256 batches or 4 MiB, and merging
//! what it holds while the watcher is slow. An `Append` grows the pending `Append` of the same row
//! (or the pending `Rows` holding that row), a `Rows` patch absorbs the older patches of the rows
//! it covers, and a `Status` folds into the pending `Status`. Row events and the other events
//! commute under `reduce::apply`, so each merge leaves the state the drained events build exactly
//! as the unmerged ones would. A watcher that still overflows is dropped to `Resync`.
use onicode_protocol::Item;
use onicode_protocol::session::{SessionEvent, SessionState};

use super::reduce::merge_turns;
use super::ring::weight;

/// How many batches a watcher's queue holds before it is dropped to `Resync`.
pub(crate) const QUEUE_BATCHES: usize = 256;
/// How many bytes (by `ring::weight`) a watcher's queue holds before it is dropped to `Resync`.
pub(crate) const QUEUE_BYTES: usize = 4 << 20;

pub(crate) struct WatchQueue {
    /// The snapshot the watcher starts from, until it is drained.
    head: Option<Item<SessionState, SessionEvent>>,
    pending: Vec<(SessionEvent, usize)>,
    /// The number of the first event not yet drained.
    next: u64,
    /// The number of the last event pushed.
    last: u64,
    /// Drain once even with nothing pending: a resumed watcher learns it missed nothing.
    ack: bool,
    batches: usize,
    bytes: usize,
    resync: bool,
    max_batches: usize,
    max_bytes: usize,
}

fn is_row_event(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::Rows { .. } | SessionEvent::Append { .. }
    )
}

impl WatchQueue {
    /// A watcher that starts from `snapshot`, taken when the session's last event was `seq`.
    pub(crate) fn snapshot(
        snapshot: Item<SessionState, SessionEvent>,
        seq: u64,
        limits: (usize, usize),
    ) -> Self {
        Self::new(Some(snapshot), seq, false, limits)
    }

    /// A watcher that resumes after `since`, missing `events` (numbered up to `head`).
    pub(crate) fn resume(
        since: u64,
        head: u64,
        events: Vec<SessionEvent>,
        limits: (usize, usize),
    ) -> Self {
        let mut queue = Self::new(None, since, true, limits);
        if !events.is_empty() {
            queue.push(since + 1, events);
        }
        queue.last = head;
        queue
    }

    fn new(
        head: Option<Item<SessionState, SessionEvent>>,
        seq: u64,
        ack: bool,
        (max_batches, max_bytes): (usize, usize),
    ) -> Self {
        Self {
            head,
            pending: Vec::new(),
            next: seq + 1,
            last: seq,
            ack,
            batches: 0,
            bytes: 0,
            resync: false,
            max_batches,
            max_bytes,
        }
    }

    /// Queues `events`, numbered from `first`.
    pub(crate) fn push(&mut self, first: u64, events: Vec<SessionEvent>) {
        if self.resync || events.is_empty() {
            return;
        }
        self.last = first + events.len() as u64 - 1;
        let mut added = false;
        for event in events {
            added |= self.absorb(event);
        }
        if added {
            self.batches += 1;
        }
        if self.batches > self.max_batches || self.bytes > self.max_bytes {
            self.resync = true;
            self.head = None;
            self.pending.clear();
            self.bytes = 0;
        }
    }

    /// Merges `event` into what is pending, or appends it; `true` when appended.
    fn absorb(&mut self, event: SessionEvent) -> bool {
        match event {
            SessionEvent::Append { row, text } => {
                if let Some(at) = self.last_row_event() {
                    let merged = match &mut self.pending[at].0 {
                        SessionEvent::Append {
                            row: pending,
                            text: grown,
                        } if *pending == row => {
                            grown.push_str(&text);
                            true
                        }
                        SessionEvent::Rows { start, rows }
                            if row >= *start && row - *start < rows.len() =>
                        {
                            rows[row - *start].text.push_str(&text);
                            true
                        }
                        _ => false,
                    };
                    if merged {
                        self.reweigh(at);
                        return false;
                    }
                }
                self.append(SessionEvent::Append { row, text })
            }
            SessionEvent::Rows { start, rows } => {
                // Everything pending from `start` on is replaced.
                let mut freed = 0;
                self.pending.retain(|(pending, size)| {
                    let covered = match pending {
                        SessionEvent::Rows { start: from, .. } => *from >= start,
                        SessionEvent::Append { row, .. } => *row >= start,
                        _ => false,
                    };
                    if covered {
                        freed += size;
                    }
                    !covered
                });
                self.bytes -= freed;
                if let Some(at) = self.last_row_event()
                    && let SessionEvent::Rows {
                        start: from,
                        rows: held,
                    } = &mut self.pending[at].0
                    && *from + held.len() >= start
                {
                    held.truncate(start - *from);
                    held.extend(rows);
                    self.reweigh(at);
                    return false;
                }
                self.append(SessionEvent::Rows { start, rows })
            }
            SessionEvent::Status { status, turn } => {
                if let Some(at) = self.pending.iter().rposition(|(e, _)| !is_row_event(e))
                    && let SessionEvent::Status {
                        status: held,
                        turn: held_turn,
                    } = &mut self.pending[at].0
                    && let Some(merged) = merge_turns(held_turn.as_ref(), turn.as_ref())
                {
                    *held = status;
                    *held_turn = merged;
                    self.reweigh(at);
                    return false;
                }
                self.append(SessionEvent::Status { status, turn })
            }
            other => self.append(other),
        }
    }

    fn last_row_event(&self) -> Option<usize> {
        self.pending.iter().rposition(|(e, _)| is_row_event(e))
    }

    fn append(&mut self, event: SessionEvent) -> bool {
        let size = weight(&event);
        self.pending.push((event, size));
        self.bytes += size;
        true
    }

    fn reweigh(&mut self, at: usize) {
        let size = weight(&self.pending[at].0);
        self.bytes = self.bytes - self.pending[at].1 + size;
        self.pending[at].1 = size;
    }

    /// The next item for the watcher, if one is ready. After `Resync` the watcher is done.
    pub(crate) fn drain(&mut self) -> Option<Item<SessionState, SessionEvent>> {
        if self.resync {
            return Some(Item::Resync);
        }
        if let Some(head) = self.head.take() {
            return Some(head);
        }
        if self.pending.is_empty() && !self.ack && self.next > self.last {
            return None;
        }
        self.ack = false;
        let events = self.pending.drain(..).map(|(event, _)| event).collect();
        let item = Item::Events {
            first: self.next,
            last: self.last,
            events,
        };
        self.next = self.last + 1;
        self.batches = 0;
        self.bytes = 0;
        Some(item)
    }
}
