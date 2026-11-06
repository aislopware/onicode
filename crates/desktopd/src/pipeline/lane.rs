//! The lane that keeps sound ahead of video.
//!
//! While the stream's sound is live, a frame's datagrams do not go into QUIC's datagram queue
//! at once: a keyframe of 60 kB queued in front of a 10 ms sound packet holds it back for as
//! long as the link takes to drain the keyframe. The lane meters video into the queue a
//! [`LANE_SLICE_US`] slice at a time, at the rate the queue has been draining, so the queue
//! never holds more than a slice of video ahead of the next sound packet. Without sound the
//! frame goes straight in.

use std::collections::VecDeque;
use std::time::Duration;

use bytes::Bytes;

/// How much video the queue may hold ahead of sound: 5 ms of the drain rate.
pub const LANE_SLICE_US: u64 = 5_000;
/// How often the lane tops the queue up while it holds video.
pub const LANE_TICK: Duration = Duration::from_millis(1);
/// The shortest span a drain rate is measured over.
pub const LANE_SAMPLE_US: u64 = 1_000;
/// How long after the last sound packet the lane still meters video.
pub const LANE_AUDIO_HOLD_US: u64 = 200_000;

/// The video waiting to go into the datagram queue, and the queue's drain rate.
#[derive(Debug, Default)]
pub struct Lane {
    /// Datagrams waiting.
    pub queue: VecDeque<Bytes>,
    /// Their bytes.
    pub bytes: usize,
    /// What the datagram queue held after the lane last put in.
    pub expected: usize,
    /// Bytes the queue drained since the last sample.
    drained: usize,
    /// When the last sample was taken.
    mark_us: u64,
    /// The queue ran dry in this sample: the rate is a floor, not a measure.
    dry: bool,
    /// The drain rate, bytes a second.
    rate: u64,
}

impl Lane {
    /// The queue holds `held` bytes at `now`: count what it drained since the lane looked.
    pub fn observe(&mut self, held: usize, now: u64) {
        self.drained = self
            .drained
            .saturating_add(self.expected.saturating_sub(held));
        self.expected = held;
        let span = now.saturating_sub(self.mark_us);
        if span < LANE_SAMPLE_US {
            self.dry |= held == 0;
            return;
        }
        let drained = u64::try_from(self.drained).unwrap_or(u64::MAX);
        let sample = drained
            .saturating_mul(1_000_000)
            .checked_div(span)
            .unwrap_or(0);
        self.rate = if sample >= self.rate || self.dry || held == 0 {
            self.rate.max(sample)
        } else {
            self.rate
                .saturating_sub(self.rate / 8)
                .saturating_add(sample / 8)
        };
        self.drained = 0;
        self.mark_us = now;
        self.dry = held == 0;
    }

    /// Bytes the queue may hold: a slice at the drain rate, never under `floor_rate`.
    #[must_use]
    pub fn budget(&self, floor_rate: u64) -> usize {
        let rate = self.rate.max(floor_rate);
        usize::try_from(rate.saturating_mul(LANE_SLICE_US) / 1_000_000).unwrap_or(usize::MAX)
    }

    /// Put `datagrams` at the back.
    pub fn push(&mut self, datagrams: &[Bytes]) {
        self.bytes = datagrams
            .iter()
            .fold(self.bytes, |sum, d| sum.saturating_add(d.len()));
        self.queue.extend(datagrams.iter().cloned());
    }

    /// What fits in the queue holding `held` with `budget`: always one into an empty queue.
    pub fn take(&mut self, held: usize, budget: usize) -> Vec<Bytes> {
        let mut room = budget.saturating_sub(held);
        let mut out = Vec::new();
        while let Some(front) = self.queue.front() {
            let first_into_empty = out.is_empty() && held == 0;
            if front.len() > room && !first_into_empty {
                break;
            }
            room = room.saturating_sub(front.len());
            self.bytes = self.bytes.saturating_sub(front.len());
            out.extend(self.queue.pop_front());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn the_rate_follows_the_drain() {
        let mut lane = Lane {
            expected: 10_000,
            ..Lane::default()
        };
        lane.observe(0, 1_000);
        assert_eq!(
            lane.budget(0),
            50_000,
            "10 kB in a millisecond: 10 MB/s, 50 kB a slice"
        );
        assert_eq!(lane.budget(100_000_000), 500_000, "the floor");
    }

    proptest! {
        #[test]
        fn the_lane_keeps_order_and_bytes(
            sizes in proptest::collection::vec(1_usize..1300, 0..64),
            takes in proptest::collection::vec((0_usize..4000, 0_usize..8000), 0..64),
        ) {
            let mut lane = Lane::default();
            let datagrams: Vec<Bytes> = sizes.iter().map(|&n| Bytes::from(vec![0_u8; n])).collect();
            lane.push(&datagrams);
            prop_assert_eq!(lane.bytes, sizes.iter().sum::<usize>());
            let mut out = Vec::new();
            for (held, budget) in takes {
                let taken = lane.take(held, budget);
                let bytes: usize = taken.iter().map(Bytes::len).sum();
                if held > 0 || taken.len() > 1 {
                    prop_assert!(bytes <= budget.saturating_sub(held) || taken.len() == 1 && held == 0);
                }
                out.extend(taken);
            }
            out.extend(lane.take(0, usize::MAX));
            prop_assert_eq!(lane.bytes, 0);
            prop_assert_eq!(out, datagrams);
        }
    }
}
