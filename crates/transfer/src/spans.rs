//! Sets of numbers kept as sorted, disjoint half-open spans: the blocks of a file on disk, the
//! items of a job that are done. A finished tree of a hundred thousand files is one span.

use serde::{Deserialize, Serialize};

/// Sorted, disjoint, non-adjacent `[start, end)` spans.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Spans(Vec<(u64, u64)>);

impl Spans {
    /// Adds `at`.
    pub fn insert(&mut self, at: u64) {
        self.insert_range(at, at.saturating_add(1));
    }

    /// Adds every number in `start..end`.
    pub fn insert_range(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        // The first span that ends at or after `start` (touching counts, so they merge).
        let first = self.0.partition_point(|&(_, e)| e < start);
        // The spans from `first` that start at or before `end` all merge with the new one.
        let mut last = first;
        let (mut lo, mut hi) = (start, end);
        while let Some(&(s, e)) = self.0.get(last) {
            if s > end {
                break;
            }
            lo = lo.min(s);
            hi = hi.max(e);
            last = last.saturating_add(1);
        }
        self.0.splice(first..last, std::iter::once((lo, hi)));
    }

    /// Whether `at` is in the set.
    #[must_use]
    pub fn contains(&self, at: u64) -> bool {
        let i = self.0.partition_point(|&(_, e)| e <= at);
        self.0.get(i).is_some_and(|&(s, _)| s <= at)
    }

    /// How many numbers are in the set.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.0
            .iter()
            .fold(0_u64, |n, &(s, e)| n.saturating_add(e.saturating_sub(s)))
    }

    /// Whether the set is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The spans themselves.
    #[must_use]
    pub fn spans(&self) -> &[(u64, u64)] {
        &self.0
    }

    /// The spans of `0..total` not in the set.
    #[must_use]
    pub fn missing(&self, total: u64) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut at = 0_u64;
        for &(s, e) in &self.0 {
            if s >= total {
                break;
            }
            if s > at {
                out.push((at, s));
            }
            at = at.max(e);
        }
        if at < total {
            out.push((at, total));
        }
        out
    }

    /// How many of the first numbers from `start` are in the set, up to `end`.
    #[must_use]
    pub fn run_from(&self, start: u64, end: u64) -> u64 {
        let i = self.0.partition_point(|&(_, e)| e <= start);
        match self.0.get(i) {
            Some(&(s, e)) if s <= start => e.min(end).saturating_sub(start),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use proptest::prelude::*;

    use super::*;

    proptest! {
        /// The spans agree with a plain set after any inserts, and stay sorted, disjoint and
        /// non-adjacent.
        #[test]
        fn spans_are_a_set(ranges in proptest::collection::vec((0_u64..200, 0_u64..20), 0..40)) {
            let mut spans = Spans::default();
            let mut set = BTreeSet::new();
            for (start, len) in ranges {
                spans.insert_range(start, start + len);
                set.extend(start..start + len);
            }
            for at in 0..240 {
                prop_assert_eq!(spans.contains(at), set.contains(&at), "at {}", at);
            }
            prop_assert_eq!(spans.count(), set.len() as u64);
            for pair in spans.spans().windows(2) {
                prop_assert!(pair[0].1 < pair[1].0, "{:?}", spans);
            }
            let missing: BTreeSet<u64> = spans
                .missing(240)
                .into_iter()
                .flat_map(|(s, e)| s..e)
                .collect();
            let expected: BTreeSet<u64> = (0..240).filter(|n| !set.contains(n)).collect();
            prop_assert_eq!(missing, expected);
            for start in 0..240 {
                let run = (start..240).take_while(|n| set.contains(n)).count() as u64;
                prop_assert_eq!(spans.run_from(start, 240), run);
            }
        }
    }
}
