//! What a stream may send now: a frame against the congestion window, a picture that stands on
//! its own against what the link can drain, and when the repair loop codes the held capture
//! again. Pure, measured in slopty (MEASUREMENTS.md, "the frame guard", "keyframes the link
//! cannot drain").

/// Frames' worth of bytes QUIC may hold before a new frame is refused: two, so one is on the
/// wire while the next queues. The congestion window is the floor, so a link whose window is
/// larger than two frames is never refused below it.
pub const HELD_FRAMES: u64 = 2;

/// Frames coded at the ceiling before temporal layers may turn on.
pub const LAYERS_AFTER_FRAMES: u64 = 300;

/// What refining a still picture may spend: half a second of the encoder's rate.
pub const REFINE_BUDGET_MS: u64 = 500;

/// A picture that stands on its own (a keyframe, or a refresh with no usable reference) is
/// coded only when the link drains it and what it holds within this long.
pub const KEYFRAME_DRAIN_MS: u64 = 400;

/// How long a deferred standalone picture waits before it goes anyway.
pub const KEYFRAME_VALVE_US: u64 = 1_000_000;

/// How long a keyframe in the encoder answers every refresh asked meanwhile.
pub const KEYFRAME_IN_FLIGHT_US: u64 = 400_000;

/// Whether a frame fits: QUIC holds no more than [`HELD_FRAMES`] frames at `target_bps` and
/// `fps`, or the congestion window, whichever is larger.
#[must_use]
pub const fn frame_fits(held: usize, cwnd: u64, target_bps: u64, fps: u16) -> bool {
    let fps = if fps == 0 { 1 } else { fps as u64 };
    let per_frame = match (target_bps / 8).checked_div(fps) {
        Some(bytes) => bytes,
        None => 0,
    };
    let frames = per_frame.saturating_mul(HELD_FRAMES);
    let limit = if frames < cwnd { cwnd } else { frames };
    (held as u64) <= limit
}

/// Whether a standalone picture of about `estimate` bytes fits behind `held` bytes at
/// `target_bps`: the link drains both within [`KEYFRAME_DRAIN_MS`]. No estimate yet fits.
#[must_use]
pub const fn keyframe_fits(estimate: u64, held: usize, target_bps: u64) -> bool {
    if estimate == 0 {
        return true;
    }
    let drainable = (target_bps / 8).saturating_mul(KEYFRAME_DRAIN_MS) / 1000;
    estimate.saturating_add(held as u64) <= drainable
}

/// The next keyframe's size estimate: a larger one is taken at once, a smaller one an eighth
/// at a time.
#[must_use]
pub const fn keyframe_estimate(previous: u64, observed: u64) -> u64 {
    if observed >= previous {
        return observed;
    }
    previous
        .saturating_sub(previous / 8)
        .saturating_add(observed / 8)
}

/// The encoder's share of `target_bps` when parity adds `parity_permille` on the wire.
#[must_use]
pub fn encoder_bps(target_bps: u32, parity_permille: u16) -> u32 {
    let whole = 1000_u64.saturating_add(u64::from(parity_permille));
    let share = u64::from(target_bps)
        .saturating_mul(1000)
        .checked_div(whole)
        .unwrap_or(0);
    u32::try_from(share).unwrap_or(u32::MAX)
}

/// How long after the held capture a repair waits for a newer one to come instead: a
/// period and a half at the ceiling, never under 60 frames a second's.
#[must_use]
pub const fn quiet_after_us(ceiling_fps: u16) -> u64 {
    let fps = if ceiling_fps < 60 { ceiling_fps } else { 60 };
    match 1_500_000_u64.checked_div(fps as u64) {
        Some(us) => us,
        None => 1_500_000,
    }
}

/// One frame period at `fps`.
#[must_use]
pub const fn period_us(fps: u16) -> u64 {
    match 1_000_000_u64.checked_div(fps as u64) {
        Some(us) => us,
        None => 1_000_000,
    }
}

/// What the held capture is still owed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Asks {
    /// It was never coded: the pacer or the guard held it back.
    pub owed: bool,
    /// A refresh from a long-term reference is asked for.
    pub refresh: bool,
    /// A keyframe is asked for.
    pub keyframe: bool,
    /// When a refinement of the still picture is due.
    pub refine: Option<u64>,
}

/// When the repair loop codes the held capture (captured at `captured_us`) again: `None` when
/// nothing is owed. A keyframe goes once no newer capture came for [`quiet_after_us`]; anything
/// else also waits for the pacer's slot (`due_us`), and a refinement for its own time.
#[must_use]
pub const fn repair_at(asks: Asks, captured_us: u64, due_us: u64, ceiling_fps: u16) -> Option<u64> {
    let refine_us = match asks.refine {
        Some(at) => at,
        None if !asks.owed && !asks.refresh && !asks.keyframe => return None,
        None => 0,
    };
    let quiet = captured_us.saturating_add(quiet_after_us(ceiling_fps));
    if asks.keyframe {
        return Some(quiet);
    }
    let at = if due_us > quiet { due_us } else { quiet };
    let refining = !asks.owed && !asks.refresh;
    Some(if refining && refine_us > at {
        refine_us
    } else {
        at
    })
}

/// The low byte of the send time in milliseconds, which every media datagram carries.
#[must_use]
pub const fn send_ms_lo(now_us: u64) -> u8 {
    #[expect(clippy::cast_possible_truncation, reason = "the low byte by design")]
    let lo = (now_us / 1000) as u8;
    lo
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_frame_fits_two_frames_or_the_window() {
        // 8 Mbps at 50 fps: 20 kB a frame, 40 kB held.
        assert!(frame_fits(40_000, 0, 8_000_000, 50));
        assert!(!frame_fits(40_001, 0, 8_000_000, 50));
        assert!(
            frame_fits(60_000, 64_000, 8_000_000, 50),
            "the window is the floor"
        );
        assert!(frame_fits(0, 0, 0, 0), "nothing held always fits");
    }

    #[test]
    fn a_keyframe_waits_for_a_link_that_drains_it() {
        // 10 Mbps drains 500 kB in 400 ms.
        assert!(keyframe_fits(400_000, 100_000, 10_000_000));
        assert!(!keyframe_fits(400_000, 100_001, 10_000_000));
        assert!(keyframe_fits(0, usize::MAX, 0), "no estimate yet");
        assert_eq!(keyframe_estimate(100, 200), 200);
        assert_eq!(keyframe_estimate(800, 0), 700);
    }

    #[test]
    fn the_encoder_leaves_room_for_parity() {
        assert_eq!(encoder_bps(11_000_000, 100), 10_000_000);
        assert_eq!(encoder_bps(10_000_000, 0), 10_000_000);
    }

    #[test]
    fn a_keyframe_waits_for_quiet_and_a_refinement_for_its_time() {
        let none = Asks::default();
        assert_eq!(repair_at(none, 1_000, 0, 60), None);
        let quiet = 1_000 + quiet_after_us(60);
        let keyframe = Asks {
            keyframe: true,
            ..none
        };
        assert_eq!(repair_at(keyframe, 1_000, u64::MAX, 60), Some(quiet));
        let owed = Asks { owed: true, ..none };
        assert_eq!(repair_at(owed, 1_000, quiet + 7, 60), Some(quiet + 7));
        let refine = Asks {
            refine: Some(quiet + 100),
            ..none
        };
        assert_eq!(repair_at(refine, 1_000, 0, 60), Some(quiet + 100));
        let owed_refine = Asks {
            owed: true,
            refine: Some(quiet + 100),
            ..none
        };
        assert_eq!(
            repair_at(owed_refine, 1_000, 0, 60),
            Some(quiet),
            "owed goes first"
        );
    }

    proptest! {
        #[test]
        fn more_held_never_fits_where_less_did_not(
            held in 0_usize..1 << 24,
            more in 0_usize..1 << 20,
            cwnd in 0_u64..1 << 24,
            bps in 0_u64..100_000_000,
            fps in 0_u16..241,
        ) {
            if !frame_fits(held, cwnd, bps, fps) {
                prop_assert!(!frame_fits(held + more, cwnd, bps, fps));
            }
            if !keyframe_fits(u64::try_from(held).unwrap(), more, bps) {
                prop_assert!(!keyframe_fits(u64::try_from(held).unwrap(), more + 1, bps));
            }
        }

        #[test]
        fn a_repair_is_never_before_its_quiet(
            owed: bool, refresh: bool, keyframe: bool,
            refine in proptest::option::of(0_u64..1 << 40),
            captured in 0_u64..1 << 40,
            due in 0_u64..1 << 40,
            fps in 1_u16..241,
        ) {
            let asks = Asks { owed, refresh, keyframe, refine };
            if let Some(at) = repair_at(asks, captured, due, fps) {
                prop_assert!(at >= captured + quiet_after_us(fps));
            } else {
                prop_assert!(!owed && !refresh && !keyframe && refine.is_none());
            }
        }
    }
}
