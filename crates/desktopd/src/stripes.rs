//! Whether a stream is coded as two stripes on the two encode engines, or as one picture.
//!
//! Stripes halve the encode of a picture larger than one engine's pixel rate, and cost a scroll
//! twice the bits: scrolled text enters every coded region at its bottom edge, and two stripes
//! have two such edges (`docs/decisions/video.md`, "Two stripes halve the encode at 3K and
//! above"). So a stream is striped only while all of these hold:
//!
//! - this Mac's engines code the two stripes side by side at the stream's size, and the whole
//!   picture takes over `WHOLE_OVER` to code (timed once per size and process, `pays`, and never
//!   under `TIMED_FROM` pixels);
//! - the encoder spends well under half its target, so twice a scroll still fits: stripes go on
//!   under `ON_UNDER` of it and off over `OFF_OVER`, each after `HOLD` of it.
//!
//! Switching rebuilds the sessions (keyframes), so the gate holds for seconds, not report
//! windows. `ONICODE_STRIPES=on|off` overrides it: the measurements' and the tests' knob.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::time::{Duration, Instant};

use onicode_desktop_codec::VideoEncoder;
use onicode_desktop_proto::screen::Chroma;
use parking_lot::Mutex;

/// The whole picture's encode past which stripes pay: 3024 × 1968 codes in 15 ms whole, 1080p
/// in 6 (MEASUREMENTS.md, "stripes across the two encode engines").
pub(super) const WHOLE_OVER: Duration = Duration::from_millis(12);

/// Surfaces under this many pixels are never timed: two thirds of 3024 × 1968, the smallest
/// size whose whole picture takes over [`WHOLE_OVER`] (1080p codes in 6 ms).
const TIMED_FROM: u64 = 3024 * 1968 * 2 / 3;

/// Spend, as a share of the encoder's target, under which stripes go on.
pub(super) const ON_UNDER: f64 = 0.45;

/// Spend over which they go off.
pub(super) const OFF_OVER: f64 = 0.70;

/// How long the spend must stay past a bound before the stripes follow it.
pub(super) const HOLD: Duration = Duration::from_secs(3);

/// How much of each new spend sample the smoothed spend takes.
const SMOOTHING: f64 = 0.25;

/// What `ONICODE_STRIPES` asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Knob {
    /// The gate decides.
    Auto,
    /// Every stream tall enough to split is striped.
    On,
    /// No stream is.
    Off,
}

impl Knob {
    /// The knob as the environment sets it; anything but `on` or `off` is the gate.
    pub(super) fn from_env() -> Self {
        Self::parse(std::env::var("ONICODE_STRIPES").ok().as_deref())
    }

    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("on") => Self::On,
            Some("off") => Self::Off,
            _gate => Self::Auto,
        }
    }
}

/// The spend half of the gate: the encoder's output against its target, smoothed, with the
/// hold each way.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Gate {
    /// Whether the spend says stripes.
    on: bool,
    /// When the spend first crossed the bound the other way, while it stays across.
    crossed: Option<Instant>,
    /// The smoothed spend, a share of the target; `None` before two samples.
    spend: Option<f64>,
    /// The last sample: when, and the encoder's bytes so far.
    sample: Option<(Instant, u64)>,
}

impl Gate {
    /// A gate that starts where the stream does.
    pub(super) const fn new(on: bool) -> Self {
        Self {
            on,
            crossed: None,
            spend: None,
            sample: None,
        }
    }

    /// The smoothed spend as a share of the target.
    #[cfg(test)]
    pub(super) const fn spend(&self) -> Option<f64> {
        self.spend
    }

    /// The encoder has made `bytes` of video so far against a target of `target_bps`, at `now`:
    /// whether the spend says stripes, from here on.
    pub(super) fn observe(&mut self, now: Instant, bytes: u64, target_bps: u64) -> bool {
        if let Some((then, before)) = self.sample.replace((now, bytes)) {
            let secs = now.saturating_duration_since(then).as_secs_f64();
            if secs > 0.0 && target_bps > 0 {
                #[expect(clippy::cast_precision_loss, reason = "a rate, not an exact count")]
                let share = (bytes.saturating_sub(before) as f64) * 8.0 / secs / target_bps as f64;
                self.spend = Some(
                    self.spend
                        .map_or(share, |s| (share - s).mul_add(SMOOTHING, s)),
                );
            }
        }
        let Some(spend) = self.spend else {
            return self.on;
        };
        let across = if self.on {
            spend > OFF_OVER
        } else {
            spend < ON_UNDER
        };
        if !across {
            self.crossed = None;
            return self.on;
        }
        let since = *self.crossed.get_or_insert(now);
        if now.saturating_duration_since(since) >= HOLD {
            self.on = !self.on;
            self.crossed = None;
        }
        self.on
    }
}

/// Whether a stream's open times the engines for a size not yet known, beside its own first
/// keyframe, as the open did before the timing waited for idle engines: `ONICODE_TIME_AT_OPEN=1`,
/// the measurements' knob for that schedule (MEASUREMENTS.md, "the stripe timing beside a new
/// stream"). Off otherwise.
pub(super) fn time_at_open() -> bool {
    static AT_OPEN: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        std::env::var("ONICODE_TIME_AT_OPEN").is_ok_and(|value| value == "1")
    });
    *AT_OPEN
}

/// A surface's width, height and chroma.
type Size = (u32, u32, Chroma);

/// What this process has timed of `side_by_side`, per size: `None` while the timing runs.
static TIMED: Mutex<Option<HashMap<Size, Option<bool>>>> = Mutex::new(None);

/// Whether stripes pay for a `width` × `height` surface carrying `chroma` on this Mac: the
/// engines code them side by side, and the whole picture takes over [`WHOLE_OVER`]. `None`
/// until it is known.
///
/// The timing ([`VideoEncoder::side_by_side`]) codes 26 frames on three sessions of its own,
/// about 300 ms of the engines at 2560 × 1600, so it starts only where `may_time` says the
/// engines are idle (`engines::Engines::quiet`), on a thread of its own, once per size. Asked
/// at a stream's open it would code beside the stream's first keyframe.
pub(super) fn pays<V: VideoEncoder>(
    width: u32,
    height: u32,
    chroma: Chroma,
    may_time: bool,
) -> Option<bool> {
    if u64::from(width).saturating_mul(u64::from(height)) < TIMED_FROM {
        return Some(false);
    }
    let key = (width, height, chroma);
    let mut timed = TIMED.lock();
    let asked = match timed.get_or_insert_with(HashMap::new).entry(key) {
        Entry::Occupied(known) => Some(*known.get()),
        Entry::Vacant(_) if !may_time => return None,
        Entry::Vacant(slot) => {
            slot.insert(None);
            None
        }
    };
    drop(timed);
    if let Some(known) = asked {
        return known;
    }
    // A thread of its own, never the runtime's blocking pool: a runtime waits for its blocking
    // tasks when it shuts down, and a VideoToolbox call that never returns (as one has on a
    // hosted virtual Mac) would hold desktopd's exit, or a test's end, for good.
    let spawned = std::thread::Builder::new().name("onicode-stripe-timing".to_owned()).spawn(
        move || {
            // The timing's own sessions count as calls under way, so no other size is timed
            // beside it and no stream's engines read as idle while it codes.
            let inside = crate::engines::ENGINES.enter();
            let verdict = match V::side_by_side(width, height, chroma) {
                Ok(Some(gate)) => {
                    let pays = gate.pays() && gate.whole > WHOLE_OVER;
                    tracing::info!(width, height, ?chroma, whole = ?gate.whole, striped = ?gate.striped, pays, "stripes timed");
                    pays
                }
                Ok(None) => false,
                Err(e) => {
                    tracing::warn!(width, height, error = %e, "stripes not timed: one picture");
                    false
                }
            };
            drop(inside);
            if let Some(timed) = TIMED.lock().as_mut() {
                timed.insert(key, Some(verdict));
            }
        },
    );
    if let Err(e) = spawned {
        tracing::warn!(width, height, error = %e, "no thread to time the stripes on: asked again later");
        if let Some(timed) = TIMED.lock().as_mut() {
            timed.remove(&key);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start
            .checked_add(Duration::from_millis(ms))
            .expect("inside the clock")
    }

    /// Stripes go on once the spend has stayed under the lower bound for the hold, off once it
    /// has stayed over the upper one for the hold, and a spend between the bounds, or one that
    /// crosses back within the hold, changes nothing.
    #[test]
    fn the_spend_moves_the_stripes_only_after_the_hold() {
        let start = Instant::now();
        let target = 10_000_000_u64;
        // Bytes a second for a share of the target.
        let per_second = |share: f64| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a test rate"
            )]
            let bytes = (share * 10_000_000.0 / 8.0) as u64;
            bytes
        };
        let mut gate = Gate::new(false);
        let mut bytes = 0;
        let mut ms = 0;
        let mut feed = |gate: &mut Gate, share: f64, seconds: u64| {
            let mut on = gate.on;
            for _ in 0..seconds * 4 {
                ms += 250;
                bytes += per_second(share) / 4;
                on = gate.observe(at(start, ms), bytes, target);
            }
            on
        };
        assert!(!feed(&mut gate, 0.6, 5), "between the bounds: as it was");
        assert!(!feed(&mut gate, 0.2, 2), "under, but not for the hold yet");
        assert!(
            !feed(&mut gate, 0.6, 2),
            "back between: the hold starts again"
        );
        assert!(feed(&mut gate, 0.2, 5), "under for the hold: on");
        assert!(feed(&mut gate, 0.6, 5), "between the bounds keeps them on");
        assert!(feed(&mut gate, 0.9, 2), "over, not for the hold yet");
        assert!(!feed(&mut gate, 0.9, 3), "over for the hold: off");
        assert!(gate.spend().is_some_and(|s| s > OFF_OVER));
    }

    /// How many times [`Timed`] was timed.
    static TIMINGS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// An encoder whose engines code two stripes in half the whole picture's 20 ms, and that
    /// counts its timings.
    struct Timed;

    impl VideoEncoder for Timed {
        type Image = ();

        fn new(
            _config: onicode_desktop_codec::EncoderConfig,
            _sink: impl Fn(onicode_desktop_codec::EncodedPacket) + Send + Sync + 'static,
        ) -> Result<Self, onicode_desktop_codec::CodecError> {
            Ok(Self)
        }

        fn side_by_side(
            _width: u32,
            _height: u32,
            _chroma: Chroma,
        ) -> Result<
            Option<onicode_desktop_codec::stripes::SideBySide>,
            onicode_desktop_codec::CodecError,
        > {
            TIMINGS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (whole, striped) = (Duration::from_millis(20), Duration::from_millis(10));
            Ok(Some(onicode_desktop_codec::stripes::SideBySide {
                whole,
                striped,
            }))
        }

        fn encode(
            &self,
            _image: &(),
            _pts_us: u64,
            _options: &onicode_desktop_codec::FrameOptions,
        ) -> Result<(), onicode_desktop_codec::CodecError> {
            Ok(())
        }

        fn set_bitrate(&self, _bps: u32) -> Result<(), onicode_desktop_codec::CodecError> {
            Ok(())
        }

        fn set_frame_rate(&self, _fps: u16) -> Result<(), onicode_desktop_codec::CodecError> {
            Ok(())
        }
    }

    /// A size not yet timed is timed only where the engines are said to be idle: asked
    /// otherwise, as a stream's open asks, it is not known and nothing is coded for it. Once
    /// asked where they are idle it is timed once, off the runtime, and known from then on,
    /// idle or not.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_engines_are_timed_only_where_they_are_idle() {
        let (width, height, chroma) = (5120, 2880, Chroma::Subsampled);
        for _ in 0..3 {
            assert_eq!(pays::<Timed>(width, height, chroma, false), None);
        }
        assert_eq!(
            TIMINGS.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "nothing timed"
        );

        assert_eq!(
            pays::<Timed>(width, height, chroma, true),
            None,
            "timing off the runtime"
        );
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("a deadline");
        while pays::<Timed>(width, height, chroma, false).is_none() {
            assert!(Instant::now() < deadline, "the timing never ended");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(pays::<Timed>(width, height, chroma, false), Some(true));
        assert_eq!(pays::<Timed>(width, height, chroma, true), Some(true));
        assert_eq!(
            TIMINGS.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "once per size"
        );
        assert_eq!(
            pays::<Timed>(1920, 1080, chroma, true),
            Some(false),
            "too small to time"
        );
        assert_eq!(TIMINGS.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    /// The knob reads `on` and `off`, and anything else leaves it to the gate.
    #[test]
    fn the_knob_forces_either_way() {
        assert_eq!(Knob::parse(Some("on")), Knob::On);
        assert_eq!(Knob::parse(Some("off")), Knob::Off);
        assert_eq!(Knob::parse(Some("auto")), Knob::Auto);
        assert_eq!(Knob::parse(None), Knob::Auto);
    }
}
