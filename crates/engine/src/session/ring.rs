//! A session's recent events, kept so a watcher that comes back with a cursor the ring still
//! covers gets only what it missed. Never persisted: settled rows are already in the store.
use std::collections::VecDeque;

use onicode_protocol::session::SessionEvent;

/// How many events a ring holds at most.
pub(crate) const RING_EVENTS: usize = 2048;
/// How many bytes of events (by [`weight`]) a ring holds at most.
pub(crate) const RING_BYTES: usize = 4 << 20;

/// About how many bytes `event` takes on the wire: exact enough to bound memory, cheap on the
/// streaming path.
pub(crate) fn weight(event: &SessionEvent) -> usize {
    match event {
        SessionEvent::Rows { rows, .. } => {
            16 + rows
                .iter()
                .map(|row| 48 + row.role.len() + row.text.len())
                .sum::<usize>()
        }
        SessionEvent::Append { text, .. } => 24 + text.len(),
        SessionEvent::Status { status, turn } => {
            64 + status.len()
                + turn
                    .as_ref()
                    .and_then(|turn| turn.provider_turn_id.as_ref())
                    .map_or(0, String::len)
        }
        other => serde_json::to_vec(other).map_or(256, |bytes| bytes.len()),
    }
}

pub(crate) struct Ring {
    events: VecDeque<(u64, SessionEvent, usize)>,
    bytes: usize,
    max_events: usize,
    max_bytes: usize,
}

impl Ring {
    pub(crate) fn new(max_events: usize, max_bytes: usize) -> Self {
        Self {
            events: VecDeque::new(),
            bytes: 0,
            max_events,
            max_bytes,
        }
    }

    /// Keeps `event`, numbered `seq`, dropping the oldest past either bound.
    pub(crate) fn push(&mut self, seq: u64, event: SessionEvent) {
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
    pub(crate) fn since(&self, seq: u64, head: u64) -> Option<Vec<SessionEvent>> {
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
