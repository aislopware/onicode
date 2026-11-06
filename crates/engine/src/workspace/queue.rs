//! The workspace's ring of recent events and one watcher's queue, under the same rules as a
//! session's (engine.md "Backpressure"): the ring holds 2048 events or 4 MiB and is never
//! persisted; a watcher's queue holds 256 batches or 4 MiB, a pending event is replaced by a later
//! one that supersedes it (`reduce::supersedes`), and a watcher that still overflows is dropped to
//! `Resync`.
use std::collections::VecDeque;

use onicode_protocol::Item;
use onicode_protocol::workspace::{WorkspaceEvent, WorkspaceState};

use super::reduce::supersedes;

pub(crate) const RING_EVENTS: usize = 2048;
pub(crate) const RING_BYTES: usize = 4 << 20;
pub(crate) const QUEUE_BATCHES: usize = 256;
pub(crate) const QUEUE_BYTES: usize = 4 << 20;

/// About how many bytes `event` takes on the wire.
pub(crate) fn weight(event: &WorkspaceEvent) -> usize {
    match event {
        WorkspaceEvent::Removed(_) | WorkspaceEvent::Notes(_) => 16,
        WorkspaceEvent::AutomationTaken(run) => 24 + run.len(),
        WorkspaceEvent::Receipt { .. } => 64,
        other => serde_json::to_vec(other).map_or(256, |bytes| bytes.len()),
    }
}

pub(crate) struct Ring {
    events: VecDeque<(u64, WorkspaceEvent, usize)>,
    bytes: usize,
    max_events: usize,
    max_bytes: usize,
}

impl Ring {
    pub(crate) fn new((max_events, max_bytes): (usize, usize)) -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            max_events,
            max_bytes,
        }
    }

    /// Keeps `event`, numbered `seq`, dropping the oldest past either bound.
    pub(crate) fn push(&mut self, seq: u64, event: WorkspaceEvent) {
        let size = weight(&event);
        self.events.push_back((seq, event, size));
        self.bytes += size;
        while self.events.len() > self.max_events || self.bytes > self.max_bytes {
            let Some((_, _, size)) = self.events.pop_front() else {
                break;
            };
            self.bytes -= size;
        }
    }

    /// The events after `seq` up to `head`, the newest number, if the ring holds every one.
    pub(crate) fn since(&self, seq: u64, head: u64) -> Option<Vec<WorkspaceEvent>> {
        if seq > head {
            return None;
        }
        if seq == head {
            return Some(Vec::new());
        }
        let oldest = self.events.front()?.0;
        if seq.saturating_add(1) < oldest {
            return None;
        }
        Some(
            self.events
                .iter()
                .filter(|(number, ..)| *number > seq)
                .map(|(_, event, _)| event.clone())
                .collect(),
        )
    }
}

pub(crate) struct WatchQueue {
    head: Option<Item<WorkspaceState, WorkspaceEvent>>,
    pending: Vec<(WorkspaceEvent, usize)>,
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

impl WatchQueue {
    /// A watcher that starts from `snapshot`, taken when the last event was `seq`.
    pub(crate) fn snapshot(
        snapshot: Item<WorkspaceState, WorkspaceEvent>,
        seq: u64,
        limits: (usize, usize),
    ) -> Self {
        Self::new(Some(snapshot), seq, false, limits)
    }

    /// A watcher that resumes after `since`, missing `events` (numbered up to `head`).
    pub(crate) fn resume(
        since: u64,
        head: u64,
        events: Vec<WorkspaceEvent>,
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
        head: Option<Item<WorkspaceState, WorkspaceEvent>>,
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
    pub(crate) fn push(&mut self, first: u64, events: Vec<WorkspaceEvent>) {
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

    /// Replaces the pending event `event` supersedes, or appends it; `true` when appended.
    fn absorb(&mut self, event: WorkspaceEvent) -> bool {
        let size = weight(&event);
        if let Some(at) = self
            .pending
            .iter()
            .rposition(|(pending, _)| supersedes(&event, pending))
        {
            self.bytes = self.bytes - self.pending[at].1 + size;
            self.pending[at] = (event, size);
            return false;
        }
        self.pending.push((event, size));
        self.bytes += size;
        true
    }

    /// The next item for the watcher, if one is ready. After `Resync` the watcher is done.
    pub(crate) fn drain(&mut self) -> Option<Item<WorkspaceState, WorkspaceEvent>> {
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
