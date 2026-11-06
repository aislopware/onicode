//! How a stream is doing, in one plain line over the picture (slopty-ui's `screen/health.rs`):
//! the figures a person reads, as Parsec's overlay leads with them. A figure past its threshold
//! wears the warning tone; a figure not yet known is left out rather than drawn as a dash.
//!
//! The frame rate is never flagged: a still screen sends no frames, so a low rate is no fault.
//! Frames that miss the display are, and so is a picture slow to reach the glass.

use std::time::Duration;

use onicode_desktop_client::ScreenStats;
use onicode_desktop_media::{PacingStats, PaintRate, Spread};
use onicode_desktop_proto::screen::Chroma;

/// A round trip slow enough to wear the warning tone: typing lags behind the fingers.
pub(crate) const RTT_WARN_FROM: Duration = Duration::from_millis(150);

/// One figure of the line, and whether it is past its threshold.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Figure {
    /// The figure as said: "60 fps", "12 ms to glass".
    pub text: String,
    /// Past its threshold: drawn in the warning tone.
    pub warn: bool,
}

/// Frames the stream lost and repaired, as its counters say.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Loss {
    /// Frames rebuilt from parity or a retransmission.
    pub repaired: u64,
    /// Frames given up on.
    pub lost: u64,
}

impl Loss {
    /// What the stream's counters say.
    #[must_use]
    pub(crate) fn of(stats: &ScreenStats) -> Self {
        Self {
            repaired: stats.frames_fec.saturating_add(stats.frames_retransmit),
            lost: stats.frames_lost,
        }
    }
}

/// What the line is made of.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HudInput {
    /// The frames a second the stream asked for.
    pub target_fps: u16,
    /// Painted in the last second, and missed.
    pub paint: PaintRate,
    /// Arrival → glass and the counters, from the view's pacer.
    pub pacing: PacingStats,
    /// Capture on the Mac → glass, once the stream's clock probes placed the Mac's clock.
    pub capture: Spread,
    /// Pictures the layer's mailbox replaced before they were shown.
    pub replaced: u64,
    /// Megabits a second the Mac sends.
    pub mbps: f64,
    /// The colour of the picture now; `None` before the first.
    pub chroma: Option<Chroma>,
    /// Frames repaired and lost.
    pub loss: Loss,
    /// The link's round trip; `None` before the first is measured.
    pub rtt: Option<Duration>,
}

/// The plain line: the painted rate; frames that missed the display, while there are any;
/// capture → glass (p50, p95), or arrival → glass until the Mac's clock is placed; decode; the
/// bitrate; the chroma; frames repaired and lost; the round trip.
///
/// "To glass" is flagged when its p95 passes three display periods and half the round trip:
/// one period each for the capture, the codec and the display, and the path's one way. From
/// arrival it is flagged past two periods.
#[must_use]
pub(crate) fn summary(input: &HudInput) -> Vec<Figure> {
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let period = Duration::from_secs(1).checked_div(u32::from(input.target_fps.max(1)));
    let one_way = input
        .rtt
        .and_then(|rtt| rtt.checked_div(2))
        .unwrap_or_default();
    let captured = input.capture.count > 0;
    let (p50, p95, slow, from) = if captured {
        let limit = period.map(|p| p.saturating_mul(3).saturating_add(one_way));
        let slow = limit.is_some_and(|limit| input.capture.p95 > limit);
        (input.capture.p50, input.capture.p95, slow, "")
    } else {
        let limit = period.map(|p| p.saturating_mul(2));
        let slow = limit.is_some_and(|limit| input.pacing.latency_p95 > limit);
        (
            input.pacing.latency_p50,
            input.pacing.latency_p95,
            slow,
            " from arrival",
        )
    };
    let mut figures = vec![Figure {
        text: format!("{} fps", input.paint.painted),
        warn: false,
    }];
    let missed = u64::from(input.paint.missed);
    if missed > 0 {
        figures.push(Figure {
            text: format!("{missed} late"),
            warn: true,
        });
    }
    if input.pacing.presented > 0 {
        figures.push(Figure {
            text: format!("{:.0}/{:.0} ms to glass{from}", ms(p50), ms(p95)),
            warn: slow,
        });
        figures.push(Figure {
            text: format!("decode {:.1} ms", ms(input.pacing.decode_p50)),
            warn: false,
        });
    }
    let skipped = input.pacing.skipped.saturating_add(input.replaced);
    if skipped > 0 || input.pacing.late > 0 {
        figures.push(Figure {
            text: format!("{skipped} skipped · {} late in all", input.pacing.late),
            warn: false,
        });
    }
    figures.push(Figure {
        text: if input.mbps < 10.0 {
            format!("{:.1} Mb/s", input.mbps)
        } else {
            format!("{:.0} Mb/s", input.mbps)
        },
        warn: false,
    });
    if let Some(chroma) = input.chroma {
        figures.push(Figure {
            text: chroma_label(chroma).to_owned(),
            warn: false,
        });
    }
    if input.loss.repaired > 0 || input.loss.lost > 0 {
        figures.push(Figure {
            text: format!(
                "{} repaired · {} lost",
                input.loss.repaired, input.loss.lost
            ),
            warn: input.loss.lost > 0,
        });
    }
    if let Some(rtt) = input.rtt {
        figures.push(Figure {
            text: format!("RTT {:.1} ms", ms(rtt)),
            warn: rtt >= RTT_WARN_FROM,
        });
    }
    figures
}

/// How the line names a chroma.
#[must_use]
pub(crate) const fn chroma_label(chroma: Chroma) -> &'static str {
    match chroma {
        Chroma::Subsampled => "4:2:0",
        Chroma::Full => "4:4:4",
    }
}

/// Bits a second as megabits.
#[must_use]
pub(crate) fn mbps(bits_per_second: u64) -> f64 {
    #[expect(clippy::cast_precision_loss, reason = "a bitrate, read by a person")]
    let bits = bits_per_second as f64;
    bits / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> HudInput {
        HudInput {
            target_fps: 60,
            paint: PaintRate {
                painted: 60,
                missed: 0,
            },
            mbps: 18.25,
            rtt: Some(Duration::from_micros(1_700)),
            ..HudInput::default()
        }
    }

    /// Before the first picture the line says the rate, the bitrate and the round trip, and
    /// nothing it cannot know yet.
    #[test]
    fn a_line_leaves_out_what_is_not_known() {
        let texts: Vec<String> = summary(&input()).into_iter().map(|f| f.text).collect();
        assert_eq!(texts, ["60 fps", "18 Mb/s", "RTT 1.7 ms"]);
    }

    /// Capture → glass past three periods and the one way is flagged; a round trip from
    /// 150 ms is; frames missed in the last second are a flagged figure of their own, never in
    /// the rate; a frame lost is flagged beside those repaired.
    #[test]
    fn figures_past_their_thresholds_wear_the_warning() {
        let mut slow = input();
        slow.pacing.presented = 10;
        slow.capture = Spread {
            p50: Duration::from_millis(30),
            p95: Duration::from_millis(200),
            max: Duration::from_millis(240),
            count: 10,
        };
        slow.paint.missed = 3;
        slow.rtt = Some(Duration::from_millis(200));
        slow.loss = Loss {
            repaired: 4,
            lost: 1,
        };
        slow.chroma = Some(Chroma::Full);
        let figures = summary(&slow);
        let warned: Vec<&str> = figures
            .iter()
            .filter(|f| f.warn)
            .map(|f| f.text.as_str())
            .collect();
        assert_eq!(
            warned,
            [
                "3 late",
                "30/200 ms to glass",
                "4 repaired · 1 lost",
                "RTT 200.0 ms"
            ]
        );
        assert!(figures.iter().any(|f| f.text == "4:4:4"));
        assert_eq!(figures.first().map(|f| f.text.as_str()), Some("60 fps"));
    }

    /// Until the Mac's clock is placed, "to glass" is from arrival, flagged past two periods.
    #[test]
    fn to_glass_falls_back_to_arrival() {
        let mut early = input();
        early.pacing.presented = 1;
        early.pacing.latency_p50 = Duration::from_millis(10);
        early.pacing.latency_p95 = Duration::from_millis(40);
        let figure = summary(&early)
            .into_iter()
            .find(|f| f.text.contains("to glass"))
            .expect("a time to glass");
        assert_eq!(figure.text, "10/40 ms to glass from arrival");
        assert!(figure.warn, "40 ms is past two 60 Hz periods");
        assert!((mbps(18_250_000) - 18.25).abs() < 1e-9);
    }
}
