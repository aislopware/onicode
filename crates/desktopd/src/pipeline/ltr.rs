//! The long-term references a coder offered and which one the client holds.
//!
//! The encoder marks a frame as a long-term reference with a token; the client acknowledges the
//! tokens it decoded (`ReceiverReport::acked_ltr`). A refresh predicts from the newest
//! acknowledged one, a few hundred bytes where a keyframe costs tens of kilobytes. A keyframe
//! starts a new epoch: what was offered before it is gone from the encoder, so an
//! acknowledgement of it never makes it usable again.

use std::collections::VecDeque;

/// How many offered tokens are remembered.
pub const LTR_OFFERED_KEEP: usize = 64;

/// One coder's references.
#[derive(Debug, Default)]
pub struct LtrBook {
    /// Tokens offered: each with its epoch and when.
    offered: VecDeque<(u64, u64, u64)>,
    epoch: u64,
    /// The reference a refresh predicts from: its token and when it was offered.
    usable: Option<(u64, u64)>,
}

impl LtrBook {
    /// A packet came back from the encoder at `now`; whether it offered a reference.
    pub fn on_packet(&mut self, keyframe: bool, token: Option<u64>, now: u64) -> bool {
        if keyframe {
            self.epoch = self.epoch.wrapping_add(1);
            self.usable = None;
        }
        let Some(token) = token else { return false };
        if self.offered.len() >= LTR_OFFERED_KEEP {
            self.offered.pop_front();
        }
        self.offered.push_back((token, self.epoch, now));
        true
    }

    /// The client acknowledged `token`; whether it was one offered.
    pub fn on_ack(&mut self, token: u64) -> bool {
        let Some(&(_token, epoch, at)) = self.offered.iter().rev().find(|(t, ..)| *t == token)
        else {
            return false;
        };
        if epoch == self.epoch && self.usable.is_none_or(|(_t, newest)| at >= newest) {
            self.usable = Some((token, at));
        }
        true
    }

    /// New sessions: nothing offered before is held by the encoder.
    pub fn reset(&mut self) {
        self.offered.clear();
        self.epoch = self.epoch.wrapping_add(1);
        self.usable = None;
    }

    /// The client lost its references (it asked for a keyframe).
    pub const fn client_lost(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.usable = None;
    }

    /// Whether a refresh has a reference to predict from.
    #[must_use]
    pub const fn usable(&self) -> bool {
        self.usable.is_some()
    }

    /// How old the usable reference is at `now`.
    #[must_use]
    pub fn usable_age(&self, now: u64) -> Option<u64> {
        self.usable.map(|(_token, at)| now.saturating_sub(at))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ack_makes_the_newest_reference_usable_until_a_keyframe() {
        let mut book = LtrBook::default();
        assert!(!book.on_packet(true, None, 0));
        assert!(book.on_packet(false, Some(7), 10));
        assert!(book.on_packet(false, Some(8), 20));
        assert!(book.on_ack(8));
        assert_eq!(book.usable_age(25), Some(5));
        assert!(book.on_ack(7), "an older one is known");
        assert_eq!(
            book.usable_age(25),
            Some(5),
            "but does not replace the newer"
        );
        assert!(!book.on_ack(99));
        book.on_packet(true, None, 30);
        assert!(!book.usable(), "a keyframe ends the epoch");
        assert!(book.on_ack(8));
        assert!(!book.usable(), "an ack from before it is no reference");
    }

    #[test]
    fn a_reset_or_a_lost_client_forgets_the_reference() {
        let mut book = LtrBook::default();
        book.on_packet(false, Some(1), 0);
        book.on_ack(1);
        book.client_lost();
        assert!(!book.usable());
        book.on_packet(false, Some(2), 1);
        book.on_ack(2);
        assert!(book.usable());
        book.reset();
        assert!(!book.usable());
        assert!(!book.on_ack(2), "a reset forgets what was offered");
        for token in 0..100 {
            book.on_packet(false, Some(token), token);
        }
        assert!(!book.on_ack(0), "only the newest are kept");
        assert!(book.on_ack(99));
    }
}
