//! Host side: turn receiver reports into a parity ratio.
//!
//! Reed–Solomon parity is the only repair that costs no round trip, which is what makes it the
//! right first line on a lossy path: a NACK answered one RTT later is a frame the presenter has
//! already given up on. The ratio has to track the loss, though — parity the link does not need
//! is bitrate the picture does not get.
//!
//! The controller is deliberately asymmetric. Loss arrives in bursts, so it **rises on the first
//! bad report and falls slowly**: MEASUREMENTS ("Wi-Fi/mesh window stream") shows loss
//! coming in clumps between clean seconds, and a symmetric filter spends every clump under-
//! protected and every gap over-protected. It also refuses to move for small changes
//! ([`Redundancy::STEP`]), because every change re-cuts the frame layout at the packetizer and a
//! ratio that jitters by a fragment per frame buys nothing.
//!
//! Pure: the caller passes the report and how many datagrams went out since the last one.

use crate::packetize::DEFAULT_PARITY_PERMILLE;
use crate::wire::ReceiverReport;

/// Adaptive parity ratio.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Redundancy {
    permille: u16,
    loss_permille: u32,
}

impl Default for Redundancy {
    fn default() -> Self {
        Self::new()
    }
}

impl Redundancy {
    /// Weight of a new, lower loss sample: an eighth, so a single clean report does not undo the
    /// protection a burst just earned. At the 50 ms report cadence that is ~0.4 s to decay.
    pub const FALL_SHIFT: u32 = 3;
    /// Headroom over the measured loss. A frame is lost unless *every* missing data fragment is
    /// covered, so the ratio has to beat the mean loss by enough to absorb its variance; twice
    /// is the standard rule of thumb for bursty channels and matches what the 20 ‰ and 50 ‰
    /// runs needed (MEASUREMENTS, "parity, NACK and refresh under injected loss").
    pub const HEADROOM: u32 = 2;
    /// Most parity ever sent (50 %); beyond this the bitrate should drop instead. Past a half,
    /// parity is buying less picture per byte than simply encoding a smaller one.
    pub const MAX: u16 = 500;
    /// Least parity ever sent (5 %): one fragment per small frame costs little and covers the
    /// single-packet loss that dominates on good links.
    pub const MIN: u16 = 50;
    /// Weight of a new, higher loss sample: half, so one bad report moves the estimate most of
    /// the way. Bursts must not be averaged away before the parity that covers them goes out.
    pub const RISE_SHIFT: u32 = 1;
    /// Smallest change worth re-cutting the frame layout for, in thousandths.
    pub const STEP: u16 = 20;

    /// Start at the default ratio.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            permille: DEFAULT_PARITY_PERMILLE,
            loss_permille: 0,
        }
    }

    /// Current parity ratio in thousandths.
    #[must_use]
    pub const fn permille(self) -> u16 {
        self.permille
    }

    /// Smoothed datagram loss in thousandths.
    #[must_use]
    pub const fn loss_permille(self) -> u32 {
        self.loss_permille
    }

    /// Fold in a report. `datagrams_sent` is how many datagrams went out since the previous
    /// report. Returns the new ratio.
    ///
    /// A window the receiver spent stalled is not evidence about loss: the link was holding
    /// packets, not dropping them, and the missing fragments it reports usually arrive with the
    /// release. Those windows leave the estimate alone — both halves of it, exactly as the
    /// bitrate controller's `Stall` verdict does: `stalls` counts the stalls that *released* in
    /// the window, and a stall that releases just after the previous report charged its duration
    /// reports `stalls > 0` with `stalled_ms == 0`.
    pub fn on_report(&mut self, report: &ReceiverReport, datagrams_sent: u32) -> u16 {
        if report.stalled_ms == 0 && report.stalls == 0 {
            let sent = u64::from(datagrams_sent.max(report.datagrams_lost).max(1));
            // In `u64`: `lost * 1000` overflows `u32` above ~4.3 million datagrams, and a
            // saturated numerator would read as *less* loss the worse the window was.
            let loss = u64::from(report.datagrams_lost)
                .saturating_mul(1000)
                .checked_div(sent)
                .unwrap_or(0);
            self.loss_permille = smooth(self.loss_permille, u32::try_from(loss).unwrap_or(1000));
        }
        let mut target = self
            .loss_permille
            .saturating_mul(Self::HEADROOM)
            .saturating_add(u32::from(Self::MIN));
        if report.frames_lost > 0 {
            // Parity was not enough for a frame the receiver then had to refresh from, which
            // costs a keyframe and a visible gap. Buy protection ahead of the estimate.
            target = target.max(u32::from(self.permille).saturating_mul(3) / 2);
        }
        let target = u16::try_from(target.clamp(u32::from(Self::MIN), u32::from(Self::MAX)))
            .unwrap_or(Self::MAX);
        // Deadband: only re-cut the layout for a change worth the trouble, but never sit one
        // step away from a bound.
        let far_enough = target.abs_diff(self.permille) >= Self::STEP;
        if far_enough || target == Self::MIN || target == Self::MAX {
            self.permille = target;
        }
        self.permille
    }
}

/// Exponential smoothing with a heavier weight on a rise than on a fall.
fn smooth(current: u32, sample: u32) -> u32 {
    let shift = if sample > current {
        Redundancy::RISE_SHIFT
    } else {
        Redundancy::FALL_SHIFT
    };
    let weight = 1_u32 << shift;
    let keep = weight.saturating_sub(1);
    current
        .saturating_mul(keep)
        .saturating_add(sample)
        .checked_div(weight)
        .unwrap_or(sample)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic loss model: `permille` loss applied to `sent` datagrams per report, with
    /// the losses clumped rather than spread, which is how a Wi-Fi path actually loses them.
    /// The sequence is a fixed LCG, so a run repeats exactly.
    struct Channel {
        seed: u64,
        permille: u32,
    }

    impl Channel {
        const fn new(permille: u32) -> Self {
            Self {
                seed: 0x1234_5678_9abc_def0,
                permille,
            }
        }

        fn next(&mut self) -> u32 {
            self.seed = self
                .seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            u32::try_from((self.seed >> 33) % 1_000_000).unwrap_or(0)
        }

        /// Datagrams lost out of `sent`, in bursts of one to four.
        fn lose(&mut self, sent: u32) -> u32 {
            let expected = sent.saturating_mul(self.permille) / 1000;
            if expected == 0 {
                return u32::from(self.next() % 1000 < self.permille);
            }
            // Same mean, clumped: a burst of 1–4 with probability tuned to keep the mean.
            let burst = (self.next() % 4).saturating_add(1);
            let draws = expected.saturating_mul(2).checked_div(burst).unwrap_or(0);
            (0..draws)
                .filter(|_| self.next().is_multiple_of(2))
                .map(|_| burst)
                .sum()
        }
    }

    fn report(lost: u32, frames_lost: u32) -> ReceiverReport {
        ReceiverReport {
            datagrams_lost: lost,
            frames_lost,
            ..ReceiverReport::default()
        }
    }

    /// Where the ratio settles on a channel of a given loss, and how many times it moved on the
    /// way: the second number is the one the deadband exists for.
    fn settle(permille: u32, reports: usize) -> (u16, usize) {
        let mut r = Redundancy::new();
        let mut channel = Channel::new(permille);
        let mut changes: usize = 0;
        let sent = 300;
        for _ in 0..reports {
            let before = r.permille();
            r.on_report(&report(channel.lose(sent), 0), sent);
            if r.permille() != before {
                changes = changes.saturating_add(1);
            }
        }
        (r.permille(), changes)
    }

    /// The ratio tracks the channel: a clean path decays to the floor, a lossy one settles near
    /// twice the loss, and a hopeless one sits at the ceiling rather than spending the whole
    /// bitrate on parity.
    #[test]
    fn the_ratio_settles_around_twice_the_measured_loss() {
        let (clean, _changes) = settle(0, 60);
        assert_eq!(clean, Redundancy::MIN, "a clean path decays to the floor");

        let (twenty, _changes) = settle(20, 200);
        assert!(
            (80..=160).contains(&twenty),
            "20 permille loss settled at {twenty}"
        );

        let (fifty, _changes) = settle(50, 200);
        assert!(
            (140..=260).contains(&fifty),
            "50 permille loss settled at {fifty}"
        );
        assert!(fifty > twenty, "more loss must buy more parity");

        let (hopeless, _changes) = settle(400, 200);
        assert_eq!(
            hopeless,
            Redundancy::MAX,
            "past the ceiling the bitrate should drop instead"
        );
    }

    /// The deadband keeps the frame layout still: a steady channel re-cuts it a handful of
    /// times, not on every report.
    #[test]
    fn a_steady_channel_does_not_re_cut_the_layout_every_report() {
        let (_settled, changes) = settle(20, 200);
        assert!(
            changes <= 20,
            "{changes} layout changes in 200 reports is churn"
        );
        let (_settled, clean_changes) = settle(0, 200);
        assert!(
            clean_changes <= 6,
            "{clean_changes} changes on a clean path"
        );
    }

    /// Loss rises faster than it falls: one bad report buys protection at once, one good report
    /// does not give it back. The asymmetry is the point — loss comes in bursts.
    #[test]
    fn parity_rises_on_the_first_bad_report_and_decays_slowly() {
        let mut r = Redundancy::new();
        for _ in 0..40 {
            r.on_report(&report(0, 0), 300);
        }
        assert_eq!(r.permille(), Redundancy::MIN);

        // One report at 10 % loss.
        let after_one = r.on_report(&report(30, 0), 300);
        assert!(
            after_one > Redundancy::MIN,
            "a burst must move the ratio at once"
        );
        assert!(
            r.loss_permille() >= 45,
            "half the sample: {}",
            r.loss_permille()
        );

        // One clean report gives back much less than the burst bought.
        let peak = r.loss_permille();
        r.on_report(&report(0, 0), 300);
        assert!(
            r.loss_permille() > peak.saturating_mul(3) / 4,
            "decayed too fast: {peak} → {}",
            r.loss_permille()
        );

        // It does get back to the floor eventually.
        for _ in 0..60 {
            r.on_report(&report(0, 0), 300);
        }
        assert_eq!(r.permille(), Redundancy::MIN);
    }

    /// A lost frame means parity was not enough for a frame that then cost a refresh; buy ahead
    /// of the estimate rather than waiting for the next report to agree.
    #[test]
    fn a_lost_frame_buys_parity_ahead_of_the_estimate() {
        let mut r = Redundancy::new();
        for _ in 0..40 {
            r.on_report(&report(6, 0), 300);
        }
        let before = r.permille();
        let after = r.on_report(&report(6, 2), 300);
        assert!(after > before, "{before} → {after}");
        assert_eq!(
            after,
            before * 3 / 2,
            "one and a half times the ratio in force"
        );
        assert!(after <= Redundancy::MAX);
    }

    /// A stalled window is the link holding packets, not dropping them: the fragments it reports
    /// missing usually arrive with the release, so they must not buy parity. Both halves of the
    /// signal count — a stall that releases just after the previous report charged its duration
    /// leaves `stalled_ms` at zero and only the `stalls` counter to go on.
    #[test]
    fn a_stalled_window_does_not_move_the_estimate() {
        let mut r = Redundancy::new();
        for _ in 0..40 {
            r.on_report(&report(0, 0), 300);
        }
        let quiet = r.loss_permille();
        let stalled = ReceiverReport {
            stalled_ms: 180,
            ..report(120, 0)
        };
        for _ in 0..10 {
            r.on_report(&stalled, 300);
        }
        assert_eq!(r.loss_permille(), quiet, "a stall is not loss");
        assert_eq!(r.permille(), Redundancy::MIN);

        // The stall released a hair after the last report: its duration was charged there, so
        // this window carries the delayed fragments and nothing but the counter to explain them.
        let released = ReceiverReport {
            stalled_ms: 0,
            stalls: 1,
            ..report(120, 0)
        };
        for _ in 0..10 {
            r.on_report(&released, 300);
        }
        assert_eq!(
            r.loss_permille(),
            quiet,
            "a released stall is not loss either"
        );
        assert_eq!(r.permille(), Redundancy::MIN);
    }

    /// Degenerate inputs: no datagrams sent, and more lost than sent.
    #[test]
    fn impossible_reports_do_not_divide_by_zero_or_overflow() {
        let mut r = Redundancy::new();
        assert!(r.on_report(&report(300, 0), 0) <= Redundancy::MAX);
        assert!(r.on_report(&report(u32::MAX, u32::MAX), 1) <= Redundancy::MAX);
        for _ in 0..40 {
            r.on_report(&report(u32::MAX, 0), u32::MAX);
        }
        assert_eq!(r.permille(), Redundancy::MAX);
    }
}
