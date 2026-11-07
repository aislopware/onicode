//! Whose size a terminal's PTY takes when several viewers watch it (a Mac and an iPhone): the
//! viewer that typed last. Until anyone has typed, or once every viewer that typed has stopped
//! watching, it is the viewer that sized it last. A viewer is known from its first resize, input
//! or watch until its last watch ends; a sole viewer's resizes all apply, as if it were alone.
use std::collections::HashMap;

use onicode_protocol::terminal::{TerminalSize, ViewerId};

/// Viewers one terminal remembers. One that never watched (a resize before its watch) is the
/// first to go past this.
const MAX_VIEWERS: usize = 16;

#[derive(Clone, Copy, Debug, Default)]
struct Viewer {
    size: Option<TerminalSize>,
    watches: usize,
    /// When it last typed and last sized, by the terminal's own clock; 0 is never.
    typed: u64,
    sized: u64,
}

/// One terminal's viewers.
#[derive(Debug, Default)]
pub(crate) struct Viewers {
    viewers: HashMap<ViewerId, Viewer>,
    clock: u64,
}

impl Viewers {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn entry(&mut self, viewer: ViewerId) -> &mut Viewer {
        if !self.viewers.contains_key(&viewer) && self.viewers.len() >= MAX_VIEWERS {
            let owner = self.owner();
            let stale = self
                .viewers
                .iter()
                .filter(|(id, _)| Some(**id) != owner)
                .min_by_key(|(_, v)| (v.watches > 0, v.typed.max(v.sized)))
                .map(|(id, _)| *id);
            if let Some(stale) = stale {
                self.viewers.remove(&stale);
            }
        }
        self.viewers.entry(viewer).or_default()
    }

    /// The viewer whose size the PTY follows: the one that typed last, else the one that sized
    /// it last.
    fn owner(&self) -> Option<ViewerId> {
        let typist = self
            .viewers
            .iter()
            .filter(|(_, v)| v.typed > 0)
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
        let now = self.tick();
        let entry = self.entry(viewer);
        entry.size = Some(size);
        entry.sized = now;
        (self.owner() == Some(viewer)).then_some(size)
    }

    /// `viewer` sent input: it becomes the typist, and the PTY takes its size.
    pub(crate) fn typed(&mut self, viewer: ViewerId) -> Option<TerminalSize> {
        let now = self.tick();
        self.entry(viewer).typed = now;
        self.size()
    }

    /// `viewer` started a watch.
    pub(crate) fn joined(&mut self, viewer: ViewerId) {
        self.entry(viewer).watches += 1;
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
    //! The typist rule against a model that replays the whole history of who did what.
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

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0usize..4, 20u16..30).prop_map(|(v, cols)| Op::Size(v, cols)),
            (0usize..4).prop_map(Op::Type),
            (0usize..4).prop_map(Op::Join),
            (0usize..4).prop_map(Op::Leave),
        ]
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
                Op::Type(v) if known(*v, at) => Some(*v),
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
