//! Whose size a terminal's PTY takes when several viewers watch it (a Mac and an iPhone): the
//! viewer that typed last, among those that gave a size. Until anyone has typed, or once every
//! viewer that typed has stopped watching, it is the viewer that sized it last. A viewer is known
//! from its first resize, input or watch until its last watch ends; a sole viewer's resizes all
//! apply, as if it were alone.
//!
//! The rule is deterministic: it reads only the order in which the host took the viewers' acts.
//! Every act (a resize, an input, a watch) takes the next tick of one clock, so no two acts tie,
//! and nothing depends on a viewer's id or on map order, not even which viewer is forgotten when
//! too many are known. Every viewer sees the outcome: the PTY's size is in the stream, and a
//! resize answers with it.
use std::collections::BTreeMap;

use onicode_protocol::terminal::{TerminalSize, ViewerId};

/// Viewers one terminal remembers. Past this, the viewer that acted longest ago among those not
/// watching (else among all, never the one the PTY follows) is forgotten.
const MAX_VIEWERS: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
struct Viewer {
    size: Option<TerminalSize>,
    watches: usize,
    /// When it last typed, last sized and last acted at all, by the terminal's own clock; 0 is
    /// never.
    typed: u64,
    sized: u64,
    acted: u64,
}

/// One terminal's viewers.
#[derive(Debug, Default)]
pub(crate) struct Viewers {
    viewers: BTreeMap<ViewerId, Viewer>,
    clock: u64,
}

impl Viewers {
    /// The next tick, and `viewer` known and marked as acting at it.
    fn act(&mut self, viewer: ViewerId) -> (u64, &mut Viewer) {
        self.clock += 1;
        let now = self.clock;
        if !self.viewers.contains_key(&viewer) && self.viewers.len() >= MAX_VIEWERS {
            let owner = self.owner();
            let stale = self
                .viewers
                .iter()
                .filter(|(id, _)| Some(**id) != owner)
                .min_by_key(|(_, v)| (v.watches > 0, v.acted))
                .map(|(id, _)| *id);
            if let Some(stale) = stale {
                self.viewers.remove(&stale);
            }
        }
        let entry = self.viewers.entry(viewer).or_default();
        entry.acted = now;
        (now, entry)
    }

    /// The viewer whose size the PTY follows: the one with a size that typed last, else the one
    /// that sized it last.
    fn owner(&self) -> Option<ViewerId> {
        let typist = self
            .viewers
            .iter()
            .filter(|(_, v)| v.typed > 0 && v.size.is_some())
            .max_by_key(|(_, v)| v.typed);
        let sizer = || {
            self.viewers
                .iter()
                .filter(|(_, v)| v.size.is_some())
                .max_by_key(|(_, v)| v.sized)
        };
        typist.or_else(sizer).map(|(id, _)| *id)
    }

    /// The size the PTY should have now, if its owner has one.
    fn size(&self) -> Option<TerminalSize> {
        self.viewers.get(&self.owner()?)?.size
    }

    /// `viewer`'s grid; the size to apply when the PTY follows it.
    pub(crate) fn sized(&mut self, viewer: ViewerId, size: TerminalSize) -> Option<TerminalSize> {
        let (now, entry) = self.act(viewer);
        entry.size = Some(size);
        entry.sized = now;
        (self.owner() == Some(viewer)).then_some(size)
    }

    /// `viewer` sent input: it becomes the typist, and the PTY takes its size.
    pub(crate) fn typed(&mut self, viewer: ViewerId) -> Option<TerminalSize> {
        let (now, entry) = self.act(viewer);
        entry.typed = now;
        self.size()
    }

    /// `viewer` started a watch.
    pub(crate) fn joined(&mut self, viewer: ViewerId) {
        self.act(viewer).1.watches += 1;
    }

    /// One of `viewer`'s watches ended; with its last, it is forgotten, and the size to apply
    /// is whoever owns the PTY next.
    pub(crate) fn left(&mut self, viewer: ViewerId) -> Option<TerminalSize> {
        let entry = self.viewers.get_mut(&viewer)?;
        entry.watches = entry.watches.saturating_sub(1);
        if entry.watches > 0 {
            return None;
        }
        self.viewers.remove(&viewer);
        self.size()
    }
}

#[cfg(test)]
mod tests {
    //! The typist rule against a model that replays the whole history of who did what, and
    //! metamorphically: renaming the viewers never changes a size the PTY takes.
    use proptest::prelude::*;
    use uuid::Uuid;

    use super::*;

    fn viewer(n: usize) -> ViewerId {
        ViewerId(Uuid::from_u128(n as u128 + 1))
    }

    fn size(cols: u16) -> TerminalSize {
        TerminalSize {
            cols,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        }
    }

    #[derive(Clone, Debug)]
    enum Op {
        Size(usize, u16),
        Type(usize),
        Join(usize),
        Leave(usize),
    }

    /// An act of one of `viewers` viewers.
    fn op_among(viewers: usize) -> impl Strategy<Value = Op> {
        prop_oneof![
            (0..viewers, 20u16..30).prop_map(|(v, cols)| Op::Size(v, cols)),
            (0..viewers).prop_map(Op::Type),
            (0..viewers).prop_map(Op::Join),
            (0..viewers).prop_map(Op::Leave),
        ]
    }

    fn op() -> impl Strategy<Value = Op> {
        op_among(4)
    }

    /// The size each act makes the PTY take, if any, with viewer `v` named `name(v)`.
    fn applied(ops: &[Op], name: impl Fn(usize) -> ViewerId) -> Vec<Option<TerminalSize>> {
        let mut viewers = Viewers::default();
        ops.iter()
            .map(|op| match op {
                Op::Size(v, cols) => viewers.sized(name(*v), size(*cols)),
                Op::Type(v) => viewers.typed(name(*v)),
                Op::Join(v) => {
                    viewers.joined(name(*v));
                    None
                }
                Op::Leave(v) => viewers.left(name(*v)),
            })
            .collect()
    }

    /// The PTY's size after `history`, read straight from it: the latest size of the viewer that
    /// typed last among those still known, else the latest size given by one still known.
    fn expected(history: &[Op], current: TerminalSize) -> TerminalSize {
        // A viewer is known from its first act until a leave that ends its last watch.
        let mut known_since = [None::<usize>; 4];
        let mut watches = [0usize; 4];
        for (at, op) in history.iter().enumerate() {
            let v = match op {
                Op::Size(v, _) | Op::Type(v) | Op::Join(v) | Op::Leave(v) => *v,
            };
            match op {
                Op::Leave(_) => {
                    if known_since[v].is_some() {
                        watches[v] = watches[v].saturating_sub(1);
                        if watches[v] == 0 {
                            known_since[v] = None;
                        }
                    }
                }
                _ => {
                    known_since[v].get_or_insert(at);
                    if matches!(op, Op::Join(_)) {
                        watches[v] += 1;
                    }
                }
            }
        }
        let known = |v: usize, at: usize| known_since[v].is_some_and(|since| at >= since);
        let last_size = |v: usize| {
            history
                .iter()
                .enumerate()
                .rev()
                .find_map(|(at, op)| match op {
                    Op::Size(w, cols) if *w == v && known(v, at) => Some(size(*cols)),
                    _ => None,
                })
        };
        let typist = history
            .iter()
            .enumerate()
            .rev()
            .find_map(|(at, op)| match op {
                Op::Type(v) if known(*v, at) && last_size(*v).is_some() => Some(*v),
                _ => None,
            });
        let owner = typist.or_else(|| {
            history
                .iter()
                .enumerate()
                .rev()
                .find_map(|(at, op)| match op {
                    Op::Size(v, _) if known(*v, at) => Some(*v),
                    _ => None,
                })
        });
        owner.and_then(last_size).unwrap_or(current)
    }

    proptest! {
        /// More viewers than a terminal remembers, so some are forgotten: the sizes the PTY
        /// takes are the same whatever the viewers' ids, since only the order of their acts
        /// counts.
        #[test]
        fn renaming_the_viewers_changes_no_size(
            ops in proptest::collection::vec(op_among(MAX_VIEWERS + 6), 0..120),
            names in Just((0..MAX_VIEWERS + 6).collect::<Vec<usize>>()).prop_shuffle(),
        ) {
            prop_assert_eq!(applied(&ops, viewer), applied(&ops, |v| viewer(names[v])));
        }

        /// Whatever viewers do, in any order, the PTY ends at the size the history says.
        #[test]
        fn the_pty_follows_the_typist(ops in proptest::collection::vec(op(), 0..60)) {
            let mut viewers = Viewers::default();
            let mut pty = size(80);
            for (at, op) in ops.iter().enumerate() {
                let apply = match op {
                    Op::Size(v, cols) => viewers.sized(viewer(*v), size(*cols)),
                    Op::Type(v) => viewers.typed(viewer(*v)),
                    Op::Join(v) => {
                        viewers.joined(viewer(*v));
                        None
                    }
                    Op::Leave(v) => viewers.left(viewer(*v)),
                };
                let before = pty;
                if let Some(size) = apply {
                    pty = size;
                }
                prop_assert_eq!(pty, expected(&ops[..=at], before), "after {:?}", op);
            }
        }
    }

    #[test]
    fn a_second_viewer_takes_the_pty_only_by_typing() {
        let (mac, phone) = (viewer(0), viewer(1));
        let mut viewers = Viewers::default();
        viewers.joined(mac);
        assert_eq!(
            viewers.sized(mac, size(100)),
            Some(size(100)),
            "nobody typed: any size"
        );
        assert_eq!(viewers.typed(mac), Some(size(100)));
        viewers.joined(phone);
        assert_eq!(
            viewers.sized(phone, size(40)),
            None,
            "the phone has not typed"
        );
        assert_eq!(viewers.typed(phone), Some(size(40)), "it typed: its size");
        assert_eq!(viewers.sized(mac, size(120)), None);
        assert_eq!(
            viewers.left(phone),
            Some(size(120)),
            "the typist left: the Mac's again"
        );
        assert_eq!(viewers.left(mac), None, "nobody is left");
    }
}
