//! What a run fetches, and how much at once.
//!
//! * Files under [`Tuning::small`] are packed back to back: one stream for many of them.
//! * Every other file is one range per run of missing blocks, up to [`Tuning::files`] units in
//!   flight, each on its own stream.
//! * When one file over [`Tuning::split`] is all that is left to start, its ranges are cut into
//!   [`Tuning::range`] pieces, [`Tuning::ranges`] of them in flight, so a single big file still
//!   fills the link.

use std::collections::{HashMap, VecDeque};

use crate::Tuning;

/// One stream's worth of work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Bytes `offset..offset + len` of item `item`.
    Range {
        /// The item.
        item: u32,
        /// Where the range starts: on a block.
        offset: u64,
        /// Its length: whole blocks, or to the end of the file.
        len: u64,
    },
    /// Items `first..first + count`, whole.
    Pack {
        /// The first item.
        first: u32,
        /// How many.
        count: u32,
    },
}

impl Unit {
    /// The item a range is of; the first item of a pack.
    #[must_use]
    pub const fn item(&self) -> u32 {
        match *self {
            Self::Range { item, .. } => item,
            Self::Pack { first, .. } => first,
        }
    }
}

/// A file with bytes still to fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    /// The item.
    pub item: u32,
    /// Its size.
    pub size: u64,
    /// The byte spans still to fetch, each starting on a block and ending on one or at the end.
    pub missing: Vec<(u64, u64)>,
}

/// Hands out units, keeping within the limits.
#[derive(Debug)]
pub struct Scheduler {
    tuning: Tuning,
    queue: VecDeque<Unit>,
    /// Units of each item waiting in the queue.
    queued: HashMap<u32, usize>,
    /// Units of each item in flight.
    flying: HashMap<u32, usize>,
    in_flight: usize,
    sizes: HashMap<u32, u64>,
}

impl Scheduler {
    /// The units for `wants`, in item order. `cheap(i)` says item `i` costs nothing in a pack (a
    /// folder or a link), so a pack may run across it.
    #[must_use]
    pub fn new(wants: &[Want], cheap: &dyn Fn(u32) -> bool, tuning: Tuning) -> Self {
        let mut wants: Vec<&Want> = wants.iter().filter(|w| !w.missing.is_empty()).collect();
        wants.sort_by_key(|w| w.item);
        let mut queue = VecDeque::new();
        let mut i = 0;
        while let Some(want) = wants.get(i) {
            if is_small(want, &tuning) {
                // A pack: this file, then later small wanted files while every item between
                // is cheap, up to the pack's limits.
                let first = want.item;
                let mut last = want.item;
                let mut bytes = want.size;
                let mut j = i.saturating_add(1);
                while let Some(next) = wants.get(j) {
                    let count = next.item.saturating_sub(first).saturating_add(1);
                    let fits = is_small(next, &tuning)
                        && count <= tuning.pack_items
                        && bytes.saturating_add(next.size) <= tuning.pack_bytes
                        && (last.saturating_add(1)..next.item).all(cheap);
                    if !fits {
                        break;
                    }
                    bytes = bytes.saturating_add(next.size);
                    last = next.item;
                    j = j.saturating_add(1);
                }
                queue.push_back(Unit::Pack {
                    first,
                    count: last.saturating_sub(first).saturating_add(1),
                });
                i = j;
            } else {
                for &(start, end) in &want.missing {
                    queue.push_back(Unit::Range {
                        item: want.item,
                        offset: start,
                        len: end.saturating_sub(start),
                    });
                }
                i = i.saturating_add(1);
            }
        }
        let mut queued = HashMap::new();
        for unit in &queue {
            *queued.entry(unit.item()).or_insert(0_usize) += 1;
        }
        Self {
            tuning,
            queue,
            queued,
            flying: HashMap::new(),
            in_flight: 0,
            sizes: wants.iter().map(|w| (w.item, w.size)).collect(),
        }
    }

    /// The next unit to start, if one may start now.
    pub fn start_next(&mut self) -> Option<Unit> {
        if self.in_flight >= self.tuning.files {
            return None;
        }
        self.split_a_lone_file();
        let at = self.queue.iter().position(|unit| {
            self.flying.get(&unit.item()).copied().unwrap_or(0) < self.tuning.ranges
        })?;
        let unit = self.queue.remove(at)?;
        self.take(&unit);
        Some(unit)
    }

    /// `unit` ended, done or not.
    pub fn finished(&mut self, unit: &Unit) {
        let item = unit.item();
        if let Some(n) = self.flying.get_mut(&item) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.flying.remove(&item);
            }
        }
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Puts `unit` back at the front, to be fetched again.
    pub fn requeue(&mut self, unit: Unit) {
        *self.queued.entry(unit.item()).or_insert(0) += 1;
        self.queue.push_front(unit);
    }

    /// Whether nothing is queued or in flight.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.queue.is_empty() && self.in_flight == 0
    }

    /// Units in flight.
    #[must_use]
    pub const fn in_flight(&self) -> usize {
        self.in_flight
    }

    fn take(&mut self, unit: &Unit) {
        let item = unit.item();
        if let Some(n) = self.queued.get_mut(&item) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.queued.remove(&item);
            }
        }
        *self.flying.entry(item).or_insert(0) += 1;
        self.in_flight = self.in_flight.saturating_add(1);
    }

    /// When the queue holds the ranges of one big file alone (what is in flight is finishing),
    /// cuts them into [`Tuning::range`] pieces.
    fn split_a_lone_file(&mut self) {
        if self.queued.len() != 1 {
            return;
        }
        let Some((&item, _)) = self.queued.iter().next() else {
            return;
        };
        if self
            .sizes
            .get(&item)
            .is_none_or(|&size| size <= self.tuning.split)
        {
            return;
        }
        let piece = self.tuning.range;
        if !self
            .queue
            .iter()
            .any(|unit| matches!(*unit, Unit::Range { len, .. } if len > piece))
        {
            return;
        }
        let mut cut = VecDeque::new();
        for unit in self.queue.drain(..) {
            match unit {
                Unit::Range { item, offset, len } if len > piece => {
                    let end = offset.saturating_add(len);
                    let mut at = offset;
                    while at < end {
                        let next = at.saturating_add(piece).min(end);
                        cut.push_back(Unit::Range {
                            item,
                            offset: at,
                            len: next.saturating_sub(at),
                        });
                        at = next;
                    }
                }
                other => cut.push_back(other),
            }
        }
        self.queue = cut;
        self.queued.insert(item, self.queue.len());
    }
}

fn is_small(want: &Want, tuning: &Tuning) -> bool {
    want.size < tuning.small && want.missing == [(0, want.size)]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use proptest::prelude::*;

    use super::*;

    fn tuning() -> Tuning {
        Tuning {
            block: 16,
            range: 32,
            small: 8,
            split: 64,
            files: 3,
            ranges: 2,
            pack_bytes: 20,
            pack_items: 4,
        }
    }

    /// A want for each size; `resumed` marks blocks already on disk.
    fn wants(sizes: &[u64], resumed: &[Vec<bool>], block: u64) -> Vec<Want> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| {
                let blocks = size.div_ceil(block).max(1);
                let mut missing: Vec<(u64, u64)> = Vec::new();
                for b in 0..blocks {
                    let done = resumed
                        .get(i)
                        .and_then(|r| r.get(usize::try_from(b).unwrap()))
                        .copied()
                        .unwrap_or(false);
                    if done && size >= 8 {
                        continue;
                    }
                    let (start, end) = (b * block, ((b + 1) * block).min(size));
                    match missing.last_mut() {
                        Some(last) if last.1 == start => last.1 = end,
                        _ => missing.push((start, end)),
                    }
                }
                Want {
                    item: u32::try_from(i).unwrap() * 2,
                    size,
                    missing,
                }
            })
            .collect()
    }

    proptest! {
        /// Run to the end in any finishing order: every missing byte is fetched exactly once,
        /// ranges start on blocks and end on one or at the end, packs carry only small files
        /// and cheap items, and the limits always hold.
        #[test]
        fn every_missing_byte_once_within_the_limits(
            sizes in proptest::collection::vec(0_u64..200, 1..12),
            resumed in proptest::collection::vec(proptest::collection::vec(any::<bool>(), 0..13), 0..12),
            order in proptest::collection::vec(any::<prop::sample::Index>(), 0..200),
        ) {
            let t = tuning();
            let wants = wants(&sizes, &resumed, t.block);
            // Odd items are folders between the files: cheap.
            let mut scheduler = Scheduler::new(&wants, &|i| i % 2 == 1, t);
            let mut flying: Vec<Unit> = Vec::new();
            let mut fetched: BTreeMap<u32, Vec<(u64, u64)>> = BTreeMap::new();
            let mut order = order.into_iter();
            loop {
                while let Some(unit) = scheduler.start_next() {
                    flying.push(unit);
                    prop_assert!(flying.len() <= t.files);
                    let per_item = flying.iter().filter(|u| u.item() == unit.item()).count();
                    prop_assert!(per_item <= t.ranges);
                }
                if flying.is_empty() {
                    break;
                }
                let at = order.next().map_or(0, |i| i.index(flying.len()));
                let unit = flying.remove(at);
                scheduler.finished(&unit);
                match unit {
                    Unit::Range { item, offset, len } => {
                        let size = wants.iter().find(|w| w.item == item).unwrap().size;
                        prop_assert_eq!(offset % t.block, 0);
                        let end = offset + len;
                        prop_assert!(end % t.block == 0 || end == size);
                        prop_assert!(end <= size);
                        fetched.entry(item).or_default().push((offset, end));
                    }
                    Unit::Pack { first, count } => {
                        prop_assert!(count <= t.pack_items);
                        let mut bytes = 0;
                        for i in first..first + count {
                            match wants.iter().find(|w| w.item == i) {
                                Some(w) => {
                                    prop_assert!(w.size < t.small);
                                    bytes += w.size;
                                    fetched.entry(i).or_default().push((0, w.size));
                                }
                                None => prop_assert!(i % 2 == 1, "item {} is not cheap", i),
                            }
                        }
                        prop_assert!(bytes <= t.pack_bytes.max(t.small));
                    }
                }
            }
            prop_assert!(scheduler.is_done());
            for want in &wants {
                let mut got = fetched.remove(&want.item).unwrap_or_default();
                got.sort_unstable();
                let mut merged: Vec<(u64, u64)> = Vec::new();
                for (s, e) in got {
                    if let Some(last) = merged.last_mut() {
                        prop_assert!(s >= last.1, "item {} fetched twice", want.item);
                        if s == last.1 {
                            last.1 = e;
                            continue;
                        }
                    }
                    merged.push((s, e));
                }
                prop_assert_eq!(&merged, &want.missing, "item {}", want.item);
            }
        }

        /// A big file alone goes out as pieces of one range each, several at once.
        #[test]
        fn a_lone_big_file_is_split(size in 65_u64..400) {
            let t = tuning();
            let wants = wants(&[size], &[], t.block);
            let mut scheduler = Scheduler::new(&wants, &|_| false, t);
            let mut flying = Vec::new();
            while let Some(unit) = scheduler.start_next() {
                if let Unit::Range { len, .. } = unit {
                    prop_assert!(len <= t.range);
                }
                flying.push(unit);
            }
            prop_assert_eq!(flying.len(), t.ranges.min(usize::try_from(size.div_ceil(t.range)).unwrap()));
        }
    }
}
