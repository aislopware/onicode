//! Temporal layers: when a stream should have frames nothing refers to, which a receiver may
//! skip.
//!
//! With layers on, the encoder writes every other frame as one no later frame is predicted from
//! (`IsDependedOnByOthers` false). A receiver that cannot repair such a frame skips it and shows
//! the next one, where any other lost frame costs a refresh, a round trip and a still picture
//! (video decisions, "Frames nothing refers to are skipped, not refreshed").
//!
//! The bits are [`crate::wire::flags::DISCARDABLE`] and [`crate::wire::flags::PREV_DISCARDABLE`].

/// Whether a stream's encoder should write temporal layers now.
///
/// What layers buy exists only on a link that loses: 60 % of the refreshes and 13–51 % of the
/// still picture under clumped loss (MEASUREMENTS, "temporal layers under clumped loss").
/// What they cost, switched on a session that has run a while as the host switches them, is
/// small: -9 % to +11 % of the bytes where the rate has room, and where it binds 0 to 0.6 dB of
/// luma on HEVC and 1.3–1.7 dB on H.264, with no frame dropped (MEASUREMENTS, "temporal
/// layers switched on a live session"). The encoder's spend is no guide to that: a layered session
/// spends under a binding target, so spend reads as room exactly while layers are on. So the gate
/// follows the link, with hysteresis, and one signal the layers themselves do not produce: no
/// layered phase measured dropped a frame, even where the rate bound, while a fraction the encoder
/// cannot code drops every one. A layered session that drops frames is in trouble, and layers go
/// off for [`LayerGate::HOLD_WINDOWS`].
///
/// Opening a session with layers, or switching them on within its first frames, is a different
/// encoder: 18–22 % more bytes, 2–6 dB less luma and dropped frames where the rate binds, and
/// switched on after the keyframe alone, 8–17 dB less. The host switches them on only once a
/// session has coded `LAYERS_AFTER_FRAMES`; this gate never sees a session.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayerGate {
    on: bool,
    /// Report windows in a row that said the link loses, or that it did not.
    lossy_run: u32,
    clean_run: u32,
    /// Report windows left before layers may go on again after the session dropped frames
    /// with them.
    held_off: u32,
}

impl LayerGate {
    /// Report windows layers stay off after a layered session dropped frames: ten seconds.
    pub const HOLD_WINDOWS: u32 = 200;
    /// Clean report windows in a row before layers go off: two seconds, so a link that loses in
    /// bursts a second or so apart keeps them.
    pub const OFF_WINDOWS: u32 = 40;
    /// Lossy report windows in a row before layers go on: two, about 100 ms at the client's
    /// 50 ms reports, so one stray loss does not switch the encoder.
    pub const ON_WINDOWS: u32 = 2;

    /// Whether layers are on.
    #[must_use]
    pub const fn on(&self) -> bool {
        self.on
    }

    /// One report window: whether the link `lossy`, and frames the encoder `dropped` in it.
    /// The new state when it changes.
    pub const fn update(&mut self, lossy: bool, dropped: u64) -> Option<bool> {
        if lossy {
            self.lossy_run = self.lossy_run.saturating_add(1);
            self.clean_run = 0;
        } else {
            self.clean_run = self.clean_run.saturating_add(1);
            self.lossy_run = 0;
        }
        self.held_off = self.held_off.saturating_sub(1);
        let on = if self.on {
            if dropped > 0 {
                self.held_off = Self::HOLD_WINDOWS;
                false
            } else {
                self.clean_run < Self::OFF_WINDOWS
            }
        } else {
            self.held_off == 0 && self.lossy_run >= Self::ON_WINDOWS
        };
        if on == self.on {
            return None;
        }
        self.on = on;
        Some(on)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed `windows` report windows of `lossy`, none dropping; the last change.
    fn windows(gate: &mut LayerGate, lossy: bool, windows: u32) -> Option<bool> {
        (0..windows).fold(None, |last, _| gate.update(lossy, 0).or(last))
    }

    /// Layers go on after two lossy windows in a row, never on one; they stay on through a
    /// clean second and go off after two clean seconds.
    #[test]
    fn layers_follow_the_link_with_hysteresis() {
        let mut gate = LayerGate::default();
        assert_eq!(gate.update(true, 0), None, "one lossy window");
        assert_eq!(gate.update(false, 0), None);
        assert_eq!(gate.update(true, 0), None);
        assert_eq!(gate.update(true, 0), Some(true), "two in a row");
        assert_eq!(
            windows(&mut gate, false, LayerGate::OFF_WINDOWS - 1),
            None,
            "a clean spell"
        );
        assert_eq!(gate.update(true, 0), None, "loss again: still on");
        assert_eq!(windows(&mut gate, false, LayerGate::OFF_WINDOWS - 1), None);
        assert_eq!(gate.update(false, 0), Some(false), "two clean seconds");
        assert!(!gate.on());
    }

    /// A layered session that drops frames, as one that has layers where the encoder cannot
    /// take them would drop every frame, loses them at once and for the hold, whatever the link.
    #[test]
    fn a_layered_session_that_drops_frames_loses_them_for_the_hold() {
        let mut gate = LayerGate::default();
        assert_eq!(windows(&mut gate, true, 2), Some(true));
        assert_eq!(gate.update(true, 3), Some(false), "dropped frames");
        assert_eq!(
            windows(&mut gate, true, LayerGate::HOLD_WINDOWS - 1),
            None,
            "held off"
        );
        assert_eq!(gate.update(true, 0), Some(true), "after the hold");
    }

    /// Frames dropped with layers off are the rate binding, which layers do not worsen: they
    /// neither hold layers off nor keep them from going on.
    #[test]
    fn drops_without_layers_change_nothing() {
        let mut gate = LayerGate::default();
        assert_eq!(gate.update(true, 5), None);
        assert_eq!(gate.update(true, 5), Some(true));
    }
}
