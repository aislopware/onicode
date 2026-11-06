//! A still picture, coded again until the encoder stops gaining on it.
//!
//! A frame of a moving picture is coded at the quality its share of the rate buys. When the
//! picture stops, ScreenCaptureKit sends nothing more, so the last frame stays as it was coded,
//! text soft. Handed the same picture again, the low-latency encoder spends the next frames on
//! what the last one missed: on scrolling text at 8 Mbit/s, 1080p 4:2:0 went from 48.5 to 52.2 dB
//! of luma in eight frames, and 4:4:4 from 35.8 to 41.4 dB (MEASUREMENTS, "a still picture
//! refined"). Where the moving picture already had the rate it needed there is nothing to gain,
//! and the encoder's own error says so within a frame or two.
//!
//! [`Refine`] is that loop's policy. After a fresh picture goes still, one more frame of it at a
//! time while the encoder's error keeps falling, within a byte budget, spaced so the encoder
//! stays mostly idle for the next real change, and none once a new picture arrives. Only a fresh
//! capture starts a picture over: a keyframe or a refresh answered while the picture is still is
//! a frame of the same picture, and its error joins the record without resetting the count, so
//! refreshes cannot keep the loop alive.

/// Least gain over the last two refinement frames that is worth another, in hundredths of a dB:
/// a tenth of a dB a frame. Over two, so one frame the encoder spent little on does not end it.
const PLATEAU_CENTI_DB: u32 = 20;

/// Most refinement frames for one still picture: at a 60 Hz rung about a second of them, past
/// which a constrained rate still gains but a quarter of a dB a frame and less.
pub const MAX_REFINEMENTS: u32 = 32;

/// Refinement frames without the encoder's error to judge by: enough for most of the gain the
/// measured sessions got, never an open-ended loop.
const BLIND_REFINEMENTS: u32 = 4;

/// Share of the encoder's time refinement may take: a frame at most every four encode times.
///
/// It is the divisor of the gap between two refinement frames. The low-latency encoder codes
/// inside the submit, so a change that arrives while it refines waits for that frame; at a
/// quarter of the time busy, a change waits on average an eighth of an encode, and at most one.
pub const IDLE_SHARE: u64 = 4;

/// Rung periods between two refinement frames at least.
///
/// A refinement is stamped one period before it is sent ([`Refine::stamp`]), so its stamp stays
/// a period clear of the one before it as well as of the next capture.
pub const PERIODS_APART: u64 = 2;

/// Refinements tracked in flight at once; a session holds at most a frame or two.
const IN_FLIGHT: usize = 4;

/// The refinement state of one stream.
#[derive(Clone, Copy, Debug, Default)]
pub struct Refine {
    /// A still picture is being refined: a fresh picture was coded and nothing has stopped it.
    active: bool,
    /// Never refine: a measurement's baseline.
    disabled: bool,
    /// Refinement frames submitted for the current picture, whether or not they came back.
    sent: u32,
    /// Bytes the refinement frames of the current picture came back as.
    bytes: u64,
    /// The encoder's luma error on the picture's last three frames, newest first; `None` where
    /// it gave none or no frame came back yet.
    mse: [Option<f64>; 3],
    /// A mean of the encoder's time per frame, microseconds, weighted to the recent frames.
    encode_us: u64,
    /// When the last frame of any kind went to the encoder, microseconds.
    last_sent_us: u64,
    /// The presentation stamp of the fresh frame the picture started from.
    fresh_pts: Option<u64>,
    /// Refinements submitted and not back: `(encoder stamp, when it was sent)`.
    in_flight: [Option<(u64, u64)>; IN_FLIGHT],
}

impl Refine {
    /// A fresh capture went to the encoder at `now_us`, stamped `pts`: the picture starts over,
    /// and its own error is where refinement starts once it comes back.
    pub const fn fresh(&mut self, pts: u64, now_us: u64) {
        *self = Self {
            active: true,
            sent: 0,
            bytes: 0,
            mse: [None; 3],
            last_sent_us: now_us,
            fresh_pts: Some(pts),
            ..*self
        };
    }

    /// A frame that is neither fresh nor a refinement went to the encoder at `now_us` (the
    /// held picture sent late, a keyframe, a refresh): the next refinement is spaced from it.
    pub const fn other_sent(&mut self, now_us: u64) {
        self.last_sent_us = now_us;
    }

    /// A refinement frame stamped `pts` went to the encoder at `now_us`. Counted here, not when
    /// it comes back, so an encoder that drops them cannot keep the loop going.
    pub fn refinement_sent(&mut self, pts: u64, now_us: u64) {
        self.sent = self.sent.saturating_add(1);
        self.last_sent_us = now_us;
        let slot = self.in_flight.iter().position(Option::is_none).unwrap_or(0);
        if let Some(entry) = self.in_flight.get_mut(slot) {
            *entry = Some((pts, now_us));
        }
    }

    /// A frame stamped `pts` came back as `bytes` with the encoder's `mse`. For a refinement,
    /// when it was sent, which is the time it goes out under; `None` for any other frame.
    pub fn returned(&mut self, pts: u64, mse: Option<f64>, bytes: u64) -> Option<u64> {
        let refinement = self
            .in_flight
            .iter_mut()
            .find(|entry| entry.is_some_and(|(stamp, _)| stamp == pts))
            .and_then(Option::take);
        if let Some((_, sent_us)) = refinement {
            self.bytes = self.bytes.saturating_add(bytes);
            self.mse = [mse, self.mse[0], self.mse[1]];
            return Some(sent_us);
        }
        // The fresh frame, or a keyframe or refresh of the same still picture: where the error
        // stands now, and what the next refinement's gain is judged against.
        if self.fresh_pts == Some(pts) || self.active {
            self.mse = [mse, None, None];
        }
        None
    }

    /// The encoder took `us` for a frame.
    pub const fn took(&mut self, us: u64) {
        self.encode_us = if self.encode_us == 0 {
            us
        } else {
            self.encode_us
                .saturating_sub(self.encode_us / 8)
                .saturating_add(us / 8)
        };
    }

    /// Nothing more to refine: the picture is not the target's any more.
    pub const fn stop(&mut self) {
        self.active = false;
    }

    /// A new encoder session: its encode time is not the old one's, and nothing it codes is a
    /// refinement of what the old one coded.
    pub const fn rebuilt(&mut self) {
        *self = Self {
            disabled: self.disabled,
            ..Self::new()
        };
    }

    /// Never refine again; for a measurement's baseline.
    pub const fn disable(&mut self) {
        self.disabled = true;
    }

    const fn new() -> Self {
        Self {
            active: false,
            disabled: false,
            sent: 0,
            bytes: 0,
            mse: [None; 3],
            encode_us: 0,
            last_sent_us: 0,
            fresh_pts: None,
            in_flight: [None; IN_FLIGHT],
        }
    }

    /// Refinement frames coded for the current picture.
    #[must_use]
    pub const fn sent(&self) -> u32 {
        self.sent
    }

    /// Whether another refinement frame of the current picture is worth coding, with
    /// `budget_bytes` the most its refinement frames may take together.
    #[must_use]
    pub fn wanted(&self, budget_bytes: u64) -> bool {
        if !self.active || self.disabled || self.sent >= MAX_REFINEMENTS {
            return false;
        }
        if self.bytes >= budget_bytes {
            return false;
        }
        match self.mse {
            [Some(newest), ..] if newest <= 0.0 => false,
            [Some(newest), _, Some(two_back)] => {
                gain_centi_db(two_back, newest) >= PLATEAU_CENTI_DB
            }
            [Some(newest), Some(one_back), None] => {
                gain_centi_db(one_back, newest) >= PLATEAU_CENTI_DB / 2
            }
            [Some(_), None, _] => true,
            [None, ..] => self.sent < BLIND_REFINEMENTS,
        }
    }

    /// When the next refinement frame may go at a rung of `period_us`, if one is wanted within
    /// `budget_bytes`.
    #[must_use]
    pub fn due_us(&self, period_us: u64, budget_bytes: u64) -> Option<u64> {
        self.wanted(budget_bytes)
            .then(|| self.last_sent_us.saturating_add(self.spacing_us(period_us)))
    }

    /// The least time between two refinement frames at a rung of `period_us`: [`PERIODS_APART`]
    /// periods, and never less than [`IDLE_SHARE`] encode times, so the encoder is idle for most
    /// of any moment a change can arrive in.
    #[must_use]
    pub const fn spacing_us(&self, period_us: u64) -> u64 {
        let idle = self.encode_us.saturating_mul(IDLE_SHARE);
        let apart = period_us.saturating_mul(PERIODS_APART);
        if idle > apart { idle } else { apart }
    }

    /// The encoder's stamp for a refinement sent at `now_us` after a frame stamped `last_pts`:
    /// a period before it is sent, and after the last stamp. Stamps only go forward, so a
    /// refinement stamped when it is sent would push a capture taken just before, and handed
    /// over just after, to a stamp past its capture time, which is also its capture time on the
    /// wire. Stamped a period early, the next capture keeps its own. The encoder was not seen
    /// to size the change by the gap either way (MEASUREMENTS, "a still picture refined").
    #[must_use]
    pub const fn stamp(now_us: u64, last_pts: u64, period_us: u64) -> u64 {
        let early = now_us.saturating_sub(period_us);
        let next = last_pts.saturating_add(1);
        if early > next { early } else { next }
    }
}

/// How much less `after` is than `before`, in hundredths of a dB: `10 log10(before / after)`,
/// zero when it did not fall. The ratio makes it the same for 8- and 10-bit errors.
fn gain_centi_db(before: f64, after: f64) -> u32 {
    if after <= 0.0 || before <= after {
        return 0;
    }
    let db = 10.0 * (before / after).log10();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=100 000 first"
    )]
    let centi_db = (db * 100.0).clamp(0.0, 100_000.0) as u32;
    centi_db
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A generous budget: the tests that are not about it.
    const ROOM: u64 = u64::MAX;

    /// An error in the units the encoder reports it, for a luma PSNR of `db` on 8-bit samples.
    fn mse(db: f64) -> f64 {
        255.0 * 255.0 / 10_f64.powf(db / 10.0)
    }

    /// Send a refinement stamped `pts` at `pts` and have it come back at `db`.
    fn refine_once(refine: &mut Refine, pts: u64, db: f64, bytes: u64) {
        refine.refinement_sent(pts, pts);
        assert_eq!(refine.returned(pts, Some(mse(db)), bytes), Some(pts));
    }

    /// The encoder's own figures from the measured runs (MEASUREMENTS, "a still picture
    /// refined"): at 8 Mbit/s it gains a third of a dB a frame and more, and refinement goes on;
    /// at 16 Mbit/s the gain dies within three frames, and it stops.
    #[test]
    fn refinement_goes_on_while_the_encoder_gains_and_stops_at_the_plateau() {
        let mut refine = Refine::default();
        assert!(!refine.wanted(ROOM), "nothing coded yet");
        refine.fresh(1, 1);
        assert_eq!(refine.returned(1, Some(mse(48.7)), 1_000), None);
        assert!(refine.wanted(ROOM));
        for (k, db) in [49.3, 50.7, 51.7, 52.1, 52.5, 53.0, 53.4]
            .into_iter()
            .enumerate()
        {
            refine_once(&mut refine, 10 + k as u64, db, 1_000);
            assert!(refine.wanted(ROOM), "still gaining at {db} dB");
        }
        refine.fresh(100, 100);
        assert_eq!(refine.returned(100, Some(mse(55.7)), 1_000), None);
        for (k, (db, more)) in [(55.9, true), (56.1, true), (56.1, true), (56.2, false)]
            .into_iter()
            .enumerate()
        {
            refine_once(&mut refine, 110 + k as u64, db, 1_000);
            assert_eq!(refine.wanted(ROOM), more, "at {db} dB");
        }
    }

    /// A lossless frame, a stop, the cap, the byte budget and an encoder that reports no error
    /// each end it; a fresh picture starts it over.
    #[test]
    fn every_bound_holds_and_a_fresh_picture_restarts() {
        let mut refine = Refine::default();
        refine.fresh(1, 1);
        refine.returned(1, Some(mse(30.0)), 0);
        refine_once(&mut refine, 2, 90.0, 0);
        refine.refinement_sent(3, 3);
        refine.returned(3, Some(0.0), 0);
        assert!(!refine.wanted(ROOM), "lossless");
        refine.fresh(4, 4);
        refine.stop();
        assert!(!refine.wanted(ROOM), "stopped");
        refine.fresh(5, 5);
        for step in 0..MAX_REFINEMENTS {
            assert!(refine.wanted(ROOM), "{step}");
            refine_once(
                &mut refine,
                10 + u64::from(step),
                20.0 + f64::from(step + 1),
                0,
            );
        }
        assert!(!refine.wanted(ROOM), "the cap");
        refine.fresh(100, 100);
        refine.returned(100, Some(mse(30.0)), 0);
        refine_once(&mut refine, 101, 31.0, 6_000);
        assert!(refine.wanted(10_000));
        refine_once(&mut refine, 102, 32.0, 6_000);
        assert!(!refine.wanted(10_000), "the budget");
        refine.fresh(200, 200);
        for k in 0..BLIND_REFINEMENTS {
            assert!(refine.wanted(ROOM));
            refine.refinement_sent(201 + u64::from(k), 201);
            refine.returned(201 + u64::from(k), None, 0);
        }
        assert!(!refine.wanted(ROOM), "a blind encoder gets a few");
        refine.fresh(300, 300);
        for k in 0..MAX_REFINEMENTS {
            refine.refinement_sent(301 + u64::from(k), 301);
        }
        assert!(
            !refine.wanted(ROOM),
            "an encoder that drops every one cannot keep it going"
        );
        refine.fresh(400, 400);
        refine.disable();
        assert!(!refine.wanted(ROOM), "disabled");
    }

    /// Keyframes and refreshes answered while the picture is still are frames of it: they
    /// neither restart the count nor are taken for refinements, so a refresh each frame cannot
    /// keep refinement going past its cap.
    #[test]
    fn refreshes_of_a_still_picture_do_not_restart_it() {
        let mut refine = Refine::default();
        refine.fresh(1, 1);
        refine.returned(1, Some(mse(30.0)), 0);
        let (mut pts, mut db) = (2, 30.0);
        while refine.wanted(ROOM) {
            db += 2.0;
            refine_once(&mut refine, pts, db, 0);
            pts += 1;
            refine.other_sent(pts);
            assert_eq!(refine.returned(pts, Some(mse(29.0)), 0), None, "a refresh");
            pts += 1;
            assert!(pts < 200, "refreshes kept it going");
        }
        assert_eq!(refine.sent(), MAX_REFINEMENTS);
    }

    /// Refinements are known by their stamps however many are in flight, and a frame that is
    /// not one is never taken for one.
    #[test]
    fn refinements_are_known_by_their_stamps() {
        let mut refine = Refine::default();
        refine.fresh(1, 1);
        refine.refinement_sent(10, 1_010);
        refine.refinement_sent(11, 1_011);
        assert_eq!(
            refine.returned(1, Some(mse(30.0)), 0),
            None,
            "the fresh frame"
        );
        assert_eq!(refine.returned(12, Some(mse(30.0)), 0), None, "a refresh");
        assert_eq!(refine.returned(11, Some(mse(31.0)), 0), Some(1_011));
        assert_eq!(refine.returned(10, Some(mse(31.0)), 0), Some(1_010));
        assert_eq!(refine.returned(10, Some(mse(31.0)), 0), None, "once");
    }

    /// Refinement never takes more than a quarter of the encoder, never comes closer than two
    /// periods, and its stamp leaves the next capture a whole period.
    #[test]
    fn refinement_leaves_the_encoder_idle_and_the_next_change_a_period() {
        let mut refine = Refine::default();
        assert_eq!(refine.spacing_us(16_667), 33_334, "no encode time yet");
        refine.took(15_000);
        assert_eq!(refine.spacing_us(16_667), 60_000, "3024 × 1968 at 60");
        refine.took(15_000);
        assert_eq!(refine.spacing_us(100_000), 200_000, "a slow rung");
        refine.fresh(1_000, 1_000);
        refine.returned(1_000, Some(mse(30.0)), 0);
        assert_eq!(refine.due_us(16_667, ROOM), Some(61_000));
        refine.rebuilt();
        assert_eq!(
            refine.spacing_us(16_667),
            33_334,
            "a new session's time is its own"
        );
        assert!(
            !refine.wanted(ROOM),
            "and nothing of the old picture is refined"
        );
        let stamp = Refine::stamp(100_000, 50_000, 16_667);
        assert_eq!(stamp, 83_333);
        assert!(
            100_000 + 1_000 - stamp > 16_667,
            "a capture a moment later is a period on"
        );
        assert_eq!(
            Refine::stamp(100_000, 90_000, 16_667),
            90_001,
            "stamps only go forward"
        );
    }
}
