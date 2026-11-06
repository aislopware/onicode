//! Numbered input: each copy applied once, in order, whichever arrives first.
//!
//! Input goes twice (slopty's ruling for keystrokes and window input: keys over 50 ms fell from
//! 13.5 % to 0.5 % with a datagram copy): on the control stream, which is reliable and ordered,
//! and a moment later as a datagram ([`crate::copy`]), which a lost stream packet does not hold
//! back. The client numbers every input and quality change on the link ([`Numberer`]); desktopd
//! keeps one [`InputOrder`] per link and takes a copy only when it is due:
//!
//! - an event that applies in its turn (a key, a button, a scroll delta, a quality change: see
//!   `ScreenRequest::numbered`) is due when the in-order event before it, which it names
//!   ([`Numbered::after`]), is the last one applied, so none is applied twice or out of order;
//! - a move sets where the pointer is, so it is due whenever it is newer than everything applied: a
//!   newer move overtakes an older one, and a move older than an applied click never lands after
//!   it.

use onicode_desktop_proto::screen::ScreenRequest;
use serde::{Deserialize, Serialize};

/// A numbered screen request.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Numbered {
    /// Its number on the link, from 1.
    pub seq: u32,
    /// The number of the last in-order request sent before it, 0 for none.
    pub after: u32,
    /// The request: one whose `numbered()` is `Some`.
    pub request: ScreenRequest,
}

impl Numbered {
    /// Whether it applies only in its turn.
    #[must_use]
    pub fn in_order(&self) -> bool {
        self.request.numbered().is_none_or(|(_, in_order)| in_order)
    }
}

/// The client's numbering of one link.
#[derive(Clone, Copy, Debug, Default)]
pub struct Numberer {
    last: u32,
    last_ordered: u32,
}

impl Numberer {
    /// `request` numbered; `None` for a request that is not numbered.
    pub fn number(&mut self, request: ScreenRequest) -> Option<Numbered> {
        let (_, in_order) = request.numbered()?;
        self.last = self.last.wrapping_add(1);
        let numbered = Numbered {
            seq: self.last,
            after: self.last_ordered,
            request,
        };
        if in_order {
            self.last_ordered = self.last;
        }
        Some(numbered)
    }
}

/// desktopd's view of one link's numbered requests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputOrder {
    /// The last in-order request applied.
    last_ordered: u32,
    /// The newest move applied.
    last_move: u32,
}

impl InputOrder {
    /// Whether `numbered`, from either copy, is due now. A `true` marks it applied: the caller
    /// must apply it.
    pub fn take(&mut self, numbered: &Numbered) -> bool {
        if numbered.in_order() {
            if numbered.seq <= self.last_ordered || numbered.after != self.last_ordered {
                return false;
            }
            self.last_ordered = numbered.seq;
            true
        } else {
            if numbered.seq <= self.last_move.max(self.last_ordered) {
                return false;
            }
            self.last_move = numbered.seq;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use onicode_desktop_proto::StreamId;
    use onicode_desktop_proto::screen::ScreenInput;
    use proptest::prelude::*;

    use super::*;

    fn input(ordered: bool, i: u32) -> ScreenRequest {
        #[expect(clippy::cast_precision_loss, reason = "small test numbers")]
        let x = i as f32;
        let input = if ordered {
            ScreenInput::Text {
                text: i.to_string(),
            }
        } else {
            ScreenInput::Move { x, y: x }
        };
        ScreenRequest::Input {
            stream: StreamId(1),
            input,
        }
    }

    proptest! {
        /// Whatever datagram copies arrive, and whenever, every in-order event is applied once
        /// and in order, and no move lands after a newer event.
        #[test]
        fn each_copy_is_applied_once_in_order(
            kinds in proptest::collection::vec(any::<bool>(), 1..64),
            copies in proptest::collection::vec((any::<u16>(), any::<bool>()), 0..128),
        ) {
            let mut numberer = Numberer::default();
            let sent: Vec<Numbered> = kinds
                .iter()
                .zip(0_u32..)
                .map(|(&ordered, i)| numberer.number(input(ordered, i)).unwrap())
                .collect();
            // Interleave: the stream delivers everything in order; datagram copies of any
            // event arrive at any point, possibly twice, possibly early.
            let mut order = InputOrder::default();
            let mut applied: Vec<u32> = Vec::new();
            let mut copies = copies.into_iter().peekable();
            for (at, numbered) in sent.iter().enumerate() {
                while let Some(&(pick, early)) = copies.peek() {
                    if !early && usize::from(pick) % (at + 1) == 0 {
                        break;
                    }
                    copies.next();
                    let copy = &sent[usize::from(pick) % sent.len()];
                    if order.take(copy) {
                        applied.push(copy.seq);
                    }
                    if !early {
                        break;
                    }
                }
                if order.take(numbered) {
                    applied.push(numbered.seq);
                }
            }
            let ordered: Vec<u32> = sent.iter().filter(|n| n.in_order()).map(|n| n.seq).collect();
            let applied_ordered: Vec<u32> = applied
                .iter()
                .copied()
                .filter(|seq| sent[usize::try_from(seq - 1).unwrap()].in_order())
                .collect();
            prop_assert_eq!(applied_ordered, ordered);
            let mut newest = 0;
            for seq in applied {
                let numbered = &sent[usize::try_from(seq - 1).unwrap()];
                if !numbered.in_order() {
                    prop_assert!(seq > newest, "a move older than something applied");
                }
                newest = newest.max(seq);
            }
        }
    }

    #[test]
    fn a_move_after_a_newer_click_is_dropped() {
        let mut numberer = Numberer::default();
        let mv = numberer.number(input(false, 1)).unwrap();
        let click = numberer.number(input(true, 2)).unwrap();
        let mut order = InputOrder::default();
        assert!(order.take(&click));
        assert!(!order.take(&mv));
        assert!(!order.take(&click), "a second copy");
    }
}
