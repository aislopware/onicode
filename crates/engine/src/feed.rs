//! One watchable state for the Files and Git groups: a directory's listing or a checkout's
//! status, numbered by a `Cursor {epoch, seq}`, with a ring of recent events for watchers that
//! come back and a bounded queue per watcher, under the rules a session's and the workspace's
//! watches keep (engine.md "Backpressure"): the ring holds 2048 events or 4 MiB, a watcher's
//! queue 256 batches or 4 MiB, a pending event is replaced by a later one that supersedes it, and
//! a watcher that still overflows is dropped to `Resync`.
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
use onicode_protocol::{Cursor, Item};
use tokio::sync::Notify;

pub(crate) const RING_EVENTS: usize = 2048;
pub(crate) const RING_BYTES: usize = 4 << 20;
pub(crate) const QUEUE_BATCHES: usize = 256;
pub(crate) const QUEUE_BYTES: usize = 4 << 20;

/// What a topic holds and how its events change it.
pub(crate) trait Feed: 'static {
    type State: Clone + Send + 'static;
    type Event: Clone + Send + 'static;

    /// Applies `event` to `state`, on the host and in every viewer's mirror alike.
    fn apply(state: &mut Self::State, event: &Self::Event);

    /// Whether `later` supersedes `earlier` whole: applying both leaves what applying `later`
    /// alone does.
    fn supersedes(later: &Self::Event, earlier: &Self::Event) -> bool;

    /// About how many bytes `event` takes on the wire.
    fn weight(event: &Self::Event) -> usize;
}

/// A topic's bounds: the ring's and each watcher queue's, in events or batches and in bytes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) ring: (usize, usize),
    pub(crate) queue: (usize, usize),
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            ring: (RING_EVENTS, RING_BYTES),
            queue: (QUEUE_BATCHES, QUEUE_BYTES),
        }
    }
}

struct Ring<F: Feed> {
    events: VecDeque<(u64, F::Event, usize)>,
    bytes: usize,
    max_events: usize,
    max_bytes: usize,
}

impl<F: Feed> Ring<F> {
    fn new((max_events, max_bytes): (usize, usize)) -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            max_events,
            max_bytes,
        }
    }

    /// Keeps `event`, numbered `seq`, dropping the oldest past either bound.
    fn push(&mut self, seq: u64, event: F::Event) {
        let size = F::weight(&event);
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
    fn since(&self, seq: u64, head: u64) -> Option<Vec<F::Event>> {
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

struct Queue<F: Feed> {
    head: Option<Item<F::State, F::Event>>,
    pending: Vec<(F::Event, usize)>,
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

impl<F: Feed> Queue<F> {
    fn new(
        head: Option<Item<F::State, F::Event>>,
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
    fn push(&mut self, first: u64, events: &[F::Event]) {
        if self.resync || events.is_empty() {
            return;
        }
        self.last = first + events.len() as u64 - 1;
        let mut added = false;
        for event in events {
            added |= self.absorb(event.clone());
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
    fn absorb(&mut self, event: F::Event) -> bool {
        let size = F::weight(&event);
        if let Some(at) = self
            .pending
            .iter()
            .rposition(|(pending, _)| F::supersedes(&event, pending))
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
    fn drain(&mut self) -> Option<Item<F::State, F::Event>> {
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

struct Watcher<F: Feed> {
    queue: Queue<F>,
    notify: Arc<Notify>,
}

/// One state, its numbering, its ring and its watchers. Not locked itself: the hub that holds it
/// changes it under its own lock.
pub(crate) struct Topic<F: Feed> {
    epoch: u64,
    seq: u64,
    state: F::State,
    ring: Ring<F>,
    watchers: HashMap<u64, Watcher<F>>,
    next_watcher: u64,
    limits: Limits,
}

impl<F: Feed> Topic<F> {
    /// A topic starting from `state`, numbered under `epoch`, which must be one no earlier topic
    /// of the same key used, so a cursor from before never matches.
    pub(crate) fn new(epoch: u64, state: F::State, limits: Limits) -> Self {
        Self {
            epoch,
            seq: 0,
            state,
            ring: Ring::new(limits.ring),
            watchers: HashMap::new(),
            next_watcher: 0,
            limits,
        }
    }

    pub(crate) fn state(&self) -> &F::State {
        &self.state
    }

    pub(crate) fn cursor(&self) -> Cursor {
        Cursor {
            epoch: self.epoch,
            seq: self.seq,
        }
    }

    /// Numbers `events`, applies them to the state and the ring, and queues them for every
    /// watcher.
    pub(crate) fn publish(&mut self, events: Vec<F::Event>) {
        if events.is_empty() {
            return;
        }
        let first = self.seq + 1;
        for event in &events {
            self.seq += 1;
            F::apply(&mut self.state, event);
            self.ring.push(self.seq, event.clone());
        }
        for watcher in self.watchers.values_mut() {
            watcher.queue.push(first, &events);
            watcher.notify.notify_one();
        }
    }

    /// A new watcher: a snapshot, or with a `since` the ring still covers, only the events after
    /// it. A backlog bigger than a watcher's queue is a snapshot too, so a watcher that comes
    /// back never meets `Resync` before its first item. Its id, and what wakes it when something
    /// is queued for it.
    pub(crate) fn join(&mut self, since: Option<Cursor>) -> (u64, Arc<Notify>) {
        let resumed = since
            .filter(|since| since.epoch == self.epoch)
            .and_then(|since| {
                self.ring
                    .since(since.seq, self.seq)
                    .map(|events| (since.seq, events))
            })
            .map(|(since, events)| {
                let mut queue = Queue::new(None, since, true, self.limits.queue);
                if !events.is_empty() {
                    queue.push(since + 1, &events);
                }
                queue.last = self.seq;
                queue
            })
            .filter(|queue| !queue.resync);
        let queue = match resumed {
            Some(queue) => queue,
            None => Queue::new(
                Some(Item::Snapshot {
                    at: self.cursor(),
                    state: self.state.clone(),
                }),
                self.seq,
                false,
                self.limits.queue,
            ),
        };
        self.next_watcher += 1;
        let id = self.next_watcher;
        let notify = Arc::new(Notify::new());
        self.watchers.insert(
            id,
            Watcher {
                queue,
                notify: notify.clone(),
            },
        );
        (id, notify)
    }

    /// The watcher's next item: `None` when it is gone, `Some(None)` when nothing is ready.
    pub(crate) fn drain(&mut self, id: u64) -> Option<Option<Item<F::State, F::Event>>> {
        Some(self.watchers.get_mut(&id)?.queue.drain())
    }

    /// Lets the watcher go; `true` when no watcher is left.
    pub(crate) fn leave(&mut self, id: u64) -> bool {
        self.watchers.remove(&id);
        self.watchers.is_empty()
    }
}

/// Runs `leave` when dropped: a watcher's stream lets its topic go however it ends.
struct Leave<L: FnOnce()>(Option<L>);

impl<L: FnOnce()> Drop for Leave<L> {
    fn drop(&mut self) {
        if let Some(leave) = self.0.take() {
            leave();
        }
    }
}

/// A watcher's stream: what `drain` yields, waiting on `notify` while nothing is ready. Ends
/// after `Resync` or when `drain` says the watcher is gone; `leave` runs once either way, or when
/// the viewer drops the stream.
pub(crate) fn stream<S, E, D, L>(
    notify: Arc<Notify>,
    drain: D,
    leave: L,
) -> BoxStream<'static, Item<S, E>>
where
    S: Send + 'static,
    E: Send + 'static,
    D: FnMut() -> Option<Option<Item<S, E>>> + Send + 'static,
    L: FnOnce() + Send + 'static,
{
    let held = (Leave(Some(leave)), notify, drain);
    futures::stream::unfold(Some(held), |held| async move {
        let (leave, notify, mut drain) = held?;
        loop {
            match drain()? {
                Some(Item::Resync) => return Some((Item::Resync, None)),
                Some(item) => return Some((item, Some((leave, notify, drain)))),
                None => notify.notified().await,
            }
        }
    })
    .boxed()
}
