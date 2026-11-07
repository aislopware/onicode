//! One watchable state, numbered by a `Cursor {epoch, seq}`, with a ring of recent events for
//! watchers that come back and a bounded queue per watcher, under engine.md "Backpressure": the
//! ring holds 2048 events or 4 MiB, a watcher's queue 256 batches or 4 MiB, a slow watcher's
//! pending events merge as its [`Feed`] allows, and a watcher that still overflows is dropped to
//! `Resync`. A watcher that comes back with a backlog bigger than its queue gets a snapshot
//! instead, so a rejoin never meets `Resync` before its first item. Every watch the engine serves
//! is one: a session, the workspace, a directory's listing and a checkout's status.
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
pub(crate) trait Feed: Sized + 'static {
    type State: Clone + Send + 'static;
    type Event: Clone + Send + 'static;

    /// Applies `event` to `state`, on the host and in every viewer's mirror alike.
    fn apply(state: &mut Self::State, event: &Self::Event);

    /// About how many bytes `event` takes on the wire.
    fn weight(event: &Self::Event) -> usize;

    /// Merges `event` into what a slow watcher holds `pending`, or appends it; `true` when
    /// appended. A merge must leave the state the drained events build exactly as the unmerged
    /// ones would.
    fn absorb(pending: &mut Pending<Self>, event: Self::Event) -> bool;
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

/// The events a watcher has not drained yet, with their weights, for a [`Feed`] to merge into.
pub(crate) struct Pending<F: Feed> {
    events: Vec<(F::Event, usize)>,
    bytes: usize,
}

impl<F: Feed> Pending<F> {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            bytes: 0,
        }
    }

    /// Appends `event`; always `true`, as [`Feed::absorb`] answers for an append.
    pub(crate) fn append(&mut self, event: F::Event) -> bool {
        let size = F::weight(&event);
        self.events.push((event, size));
        self.bytes += size;
        true
    }

    /// Replaces the newest pending event `event` supersedes (applying both leaves what applying
    /// `event` alone does), or appends it; `true` when appended.
    pub(crate) fn replace_or_append(
        &mut self,
        event: F::Event,
        supersedes: impl Fn(&F::Event, &F::Event) -> bool,
    ) -> bool {
        match self.rposition(|pending| supersedes(&event, pending)) {
            Some(at) => {
                self.events[at].0 = event;
                self.reweigh(at);
                false
            }
            None => self.append(event),
        }
    }

    /// The newest pending event that matches.
    pub(crate) fn rposition(&self, matches: impl Fn(&F::Event) -> bool) -> Option<usize> {
        self.events.iter().rposition(|(event, _)| matches(event))
    }

    /// The pending event at `at`, to merge into; [`Self::reweigh`] it after.
    pub(crate) fn event_mut(&mut self, at: usize) -> &mut F::Event {
        &mut self.events[at].0
    }

    /// Weighs the event at `at` again after a merge.
    pub(crate) fn reweigh(&mut self, at: usize) {
        let size = F::weight(&self.events[at].0);
        self.bytes = self.bytes - self.events[at].1 + size;
        self.events[at].1 = size;
    }

    /// Keeps only the pending events that match.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&F::Event) -> bool) {
        let mut freed = 0;
        self.events.retain(|(event, size)| {
            let kept = keep(event);
            if !kept {
                freed += size;
            }
            kept
        });
        self.bytes -= freed;
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    fn take(&mut self) -> Vec<F::Event> {
        self.bytes = 0;
        self.events.drain(..).map(|(event, _)| event).collect()
    }

    fn clear(&mut self) {
        self.events.clear();
        self.bytes = 0;
    }
}

/// One watcher's queue: the snapshot it starts from, then its pending events, bounded.
pub(crate) struct Queue<F: Feed> {
    head: Option<Item<F::State, F::Event>>,
    pending: Pending<F>,
    /// The number of the first event not yet drained.
    next: u64,
    /// The number of the last event pushed.
    last: u64,
    /// Drain once even with nothing pending: a resumed watcher learns it missed nothing.
    ack: bool,
    batches: usize,
    resync: bool,
    max_batches: usize,
    max_bytes: usize,
}

impl<F: Feed> Queue<F> {
    /// A watcher that starts from `snapshot`, taken when the last event was `seq`.
    pub(crate) fn snapshot(
        snapshot: Item<F::State, F::Event>,
        seq: u64,
        limits: (usize, usize),
    ) -> Self {
        Self::new(Some(snapshot), seq, false, limits)
    }

    fn new(
        head: Option<Item<F::State, F::Event>>,
        seq: u64,
        ack: bool,
        (max_batches, max_bytes): (usize, usize),
    ) -> Self {
        Self {
            head,
            pending: Pending::new(),
            next: seq + 1,
            last: seq,
            ack,
            batches: 0,
            resync: false,
            max_batches,
            max_bytes,
        }
    }

    /// Queues `events`, numbered from `first`.
    pub(crate) fn push(&mut self, first: u64, events: &[F::Event]) {
        if self.resync || events.is_empty() {
            return;
        }
        self.last = first + events.len() as u64 - 1;
        let mut added = false;
        for event in events {
            added |= F::absorb(&mut self.pending, event.clone());
        }
        if added {
            self.batches += 1;
        }
        if self.batches > self.max_batches || self.pending.bytes > self.max_bytes {
            self.resync = true;
            self.head = None;
            self.pending.clear();
        }
    }

    /// The next item for the watcher, if one is ready. After `Resync` the watcher is done.
    pub(crate) fn drain(&mut self) -> Option<Item<F::State, F::Event>> {
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
        let item = Item::Events {
            first: self.next,
            last: self.last,
            events: self.pending.take(),
        };
        self.next = self.last + 1;
        self.batches = 0;
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

    /// Replaces the state a topic starts from, before any event is numbered: a hub that reads
    /// its state lazily.
    pub(crate) fn load(&mut self, state: F::State) {
        debug_assert_eq!(self.seq, 0, "a topic loads before its first event");
        self.state = state;
    }

    pub(crate) fn cursor(&self) -> Cursor {
        Cursor {
            epoch: self.epoch,
            seq: self.seq,
        }
    }

    /// Whether any watcher holds the topic.
    pub(crate) fn is_watched(&self) -> bool {
        !self.watchers.is_empty()
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

    /// A new watcher that starts from the whole state; see [`Self::join_with`].
    pub(crate) fn join(&mut self, since: Option<Cursor>) -> (u64, Arc<Notify>) {
        self.join_with(since, <F::State as Clone>::clone)
    }

    /// A new watcher: a snapshot (the state as `snapshot` cuts it for this watcher), or with a
    /// `since` the ring still covers, only the events after it. A backlog bigger than a watcher's
    /// queue is a snapshot too, so a watcher that comes back never meets `Resync` before its
    /// first item. Its id, and what wakes it when something is queued for it.
    pub(crate) fn join_with(
        &mut self,
        since: Option<Cursor>,
        snapshot: impl FnOnce(&F::State) -> F::State,
    ) -> (u64, Arc<Notify>) {
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
            None => Queue::snapshot(
                Item::Snapshot {
                    at: self.cursor(),
                    state: snapshot(&self.state),
                },
                self.seq,
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
    // After `Resync` the stream ends, but the watcher is let go only when the stream is dropped:
    // a viewer resubscribes before it lets the old stream go, so the topic is never left unwatched
    // between them (an idle session would unload and reload from the store).
    let held = (Leave(Some(leave)), notify, drain);
    futures::stream::unfold((held, false), |(held, ended)| async move {
        if ended {
            return None;
        }
        let (leave, notify, mut drain) = held;
        loop {
            match drain()? {
                Some(Item::Resync) => return Some((Item::Resync, ((leave, notify, drain), true))),
                Some(item) => return Some((item, ((leave, notify, drain), false))),
                None => notify.notified().await,
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    //! The topic every watch rides, against a list that only grows: viewers fall behind, leave
    //! and come back at cursors they held, under rings bigger than their queues, and each one,
    //! caught up, holds the host's list.
    use futures::FutureExt;
    use proptest::prelude::*;

    use super::*;

    /// A list of numbers, each event one more number; a later event never replaces an earlier.
    struct Numbers;

    impl Feed for Numbers {
        type Event = u32;
        type State = Vec<u32>;

        fn apply(state: &mut Vec<u32>, event: &u32) {
            state.push(*event);
        }

        fn weight(_: &u32) -> usize {
            8
        }

        fn absorb(pending: &mut Pending<Self>, event: u32) -> bool {
            pending.append(event)
        }
    }

    struct Viewer {
        id: Option<u64>,
        held: Option<(Cursor, Vec<u32>)>,
        cursors: Vec<Cursor>,
    }

    impl Viewer {
        /// Folds whatever is ready; after `Resync` the viewer is out until it rejoins.
        fn pump(&mut self, topic: &mut Topic<Numbers>) {
            while let Some(id) = self.id {
                let Some(Some(item)) = topic.drain(id) else {
                    return;
                };
                match item {
                    Item::Snapshot { at, state } => self.held = Some((at, state)),
                    Item::Events {
                        first,
                        last,
                        events,
                    } => {
                        let (cursor, state) = self.held.as_mut().expect("a snapshot first");
                        assert_eq!(first, cursor.seq + 1, "no gap and no repeat");
                        state.extend(events);
                        cursor.seq = last;
                    }
                    Item::Resync => {
                        topic.leave(id);
                        self.id = None;
                    }
                }
                if let Some((cursor, _)) = &self.held {
                    self.cursors.push(*cursor);
                }
            }
        }

        /// Comes back at a cursor it held (with what it held there), or at its last one.
        fn rejoin(&mut self, topic: &mut Topic<Numbers>, pick: Option<usize>) {
            if let Some(id) = self.id.take() {
                topic.leave(id);
            }
            if let Some(pick) = pick
                && !self.cursors.is_empty()
            {
                let at = self.cursors[pick % self.cursors.len()];
                let mut state = topic.state().clone();
                state.truncate(usize::try_from(at.seq).unwrap());
                self.held = Some((at, state));
            }
            let since = self.held.as_ref().map(|(cursor, _)| *cursor);
            let (id, _) = topic.join(since);
            self.id = Some(id);
            let Some(Some(first)) = topic.drain(id) else {
                return;
            };
            assert!(
                !matches!(first, Item::Resync),
                "a viewer that comes back never meets Resync first"
            );
            match first {
                Item::Snapshot { at, state } => self.held = Some((at, state)),
                Item::Events {
                    first,
                    last,
                    events,
                } => {
                    let (cursor, state) = self.held.as_mut().expect("a resume has a cursor");
                    assert_eq!(first, cursor.seq + 1);
                    state.extend(events);
                    cursor.seq = last;
                }
                Item::Resync => unreachable!(),
            }
        }
    }

    #[derive(Clone, Debug)]
    enum Step {
        Publish(u8),
        Pump(usize),
        Rejoin(usize, Option<usize>),
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            3 => (1u8..12).prop_map(Step::Publish),
            2 => any::<usize>().prop_map(Step::Pump),
            2 => (any::<usize>(), proptest::option::of(any::<usize>()))
                .prop_map(|(at, pick)| Step::Rejoin(at, pick)),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        /// Rings much bigger than the queues: a viewer whose backlog alone is past its queue
        /// comes back to a snapshot, never to `Resync` again, and every viewer converges.
        #[test]
        fn a_rejoin_past_the_queue_size_converges(
            ring in 4usize..64,
            batches in 1usize..3,
            bytes in 8usize..48,
            steps in proptest::collection::vec(step(), 1..80),
        ) {
            let mut topic = Topic::<Numbers>::new(
                7,
                Vec::new(),
                Limits { ring: (ring, usize::MAX), queue: (batches, bytes) },
            );
            let mut viewers: Vec<Viewer> = (0..3)
                .map(|_| {
                    let mut viewer = Viewer { id: None, held: None, cursors: Vec::new() };
                    viewer.rejoin(&mut topic, None);
                    viewer
                })
                .collect();
            let mut next = 0;
            for step in &steps {
                match step {
                    Step::Publish(count) => {
                        let events: Vec<u32> = (0..u32::from(*count)).map(|n| next + n).collect();
                        next += u32::from(*count);
                        topic.publish(events);
                    }
                    Step::Pump(at) => viewers[at % 3].pump(&mut topic),
                    Step::Rejoin(at, pick) => viewers[at % 3].rejoin(&mut topic, *pick),
                }
            }
            for viewer in &mut viewers {
                viewer.pump(&mut topic);
                if viewer.id.is_none() {
                    viewer.rejoin(&mut topic, None);
                    viewer.pump(&mut topic);
                }
                let (cursor, state) = viewer.held.as_ref().expect("every viewer holds the list");
                prop_assert_eq!(*cursor, topic.cursor());
                prop_assert_eq!(state, topic.state());
            }
        }
    }

    #[test]
    fn a_stream_lets_its_topic_go_when_dropped() {
        let topic = Arc::new(std::sync::Mutex::new(Topic::<Numbers>::new(
            1,
            Vec::new(),
            Limits::default(),
        )));
        let (id, notify) = topic.lock().unwrap().join(None);
        let (draining, leaving) = (topic.clone(), topic.clone());
        let mut items = stream(
            notify,
            move || draining.lock().unwrap().drain(id),
            move || {
                leaving.lock().unwrap().leave(id);
            },
        );
        assert!(matches!(
            items.next().now_or_never(),
            Some(Some(Item::Snapshot { .. }))
        ));
        assert!(topic.lock().unwrap().is_watched());
        drop(items);
        assert!(!topic.lock().unwrap().is_watched());
    }
}
