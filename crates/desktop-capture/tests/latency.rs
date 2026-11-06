//! Capture against the live window server, gated by `ONICODE_DESKTOP_LIVE=1`: it captures this
//! Mac's main display, which needs the Screen Recording grant (a test never asks for it; one
//! without it fails to start and says so).
//!
//! Latency is the window server's display time of a frame (`SCStreamFrameInfoDisplayTime`) →
//! our callback. slopty measured these against a stand-in window of its own; onicode streams
//! whole displays, so these time the main display, and a display that does not change delivers
//! few frames: move something on it while they run.

#[cfg(test)]
#[cfg(target_os = "macos")]
#[expect(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "measurement arithmetic on small counts and microseconds"
)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use onicode_desktop_capture::{
        Capture, CaptureConfig, CapturedFrame, PixelFormat, Shareable, Target, enumerate,
        sck_defaults,
    };
    use onicode_desktop_proto::DisplayId;
    use onicode_desktop_proto::screen::CaptureTarget;

    /// Block this test thread while a stream settles.
    fn pause(d: Duration) {
        std::thread::sleep(d);
    }

    fn gated() -> bool {
        if std::env::var_os("ONICODE_DESKTOP_LIVE").is_none() {
            onicode_desktop_testkit::live::skip("set ONICODE_DESKTOP_LIVE=1");
            return false;
        }
        true
    }

    fn seconds() -> u64 {
        std::env::var("ONICODE_DESKTOP_SECONDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5)
    }

    fn shareable() -> Shareable {
        let (tx, rx) = mpsc::channel();
        enumerate(move |r| {
            let _gone = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("enumerate")
            .expect("content")
    }

    /// The main display, resolved against a fresh enumeration.
    fn main_display() -> Target {
        let main = objc2_core_graphics::CGMainDisplayID();
        Target::resolve(&shareable(), CaptureTarget::Display(DisplayId(main)))
            .expect("the main display")
    }

    /// One capture run: `(n, p50, p95, max)` of the capture latency in microseconds, the
    /// display-time → pts offset p50 and the number of inter-frame gaps over 25 ms.
    struct Run {
        n: usize,
        p50: u64,
        p95: u64,
        max: u64,
        offset_p50_us: i64,
        gaps: usize,
        fps: f64,
    }

    fn quantile(sorted: &[u64], q: f64) -> u64 {
        if sorted.is_empty() {
            return 0;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test"
        )]
        let i = ((sorted.len() - 1) as f64 * q).round() as usize;
        sorted[i.min(sorted.len() - 1)]
    }

    fn run(target: &Target, config: &CaptureConfig, seconds: u64) -> Run {
        let (tx, rx) = mpsc::channel::<(u64, i64, u64)>();
        let (started_tx, started_rx) = mpsc::channel();
        let capture = Capture::start(
            target,
            config,
            move |frame: CapturedFrame| {
                let offset = frame.display_ts_us.map_or(0, |d| {
                    i64::try_from(d).unwrap_or(0) - i64::try_from(frame.capture_ts_us).unwrap_or(0)
                });
                let _gone = tx.send((
                    frame.latency_us,
                    offset,
                    onicode_desktop_capture::host_now_us(),
                ));
            },
            |e| panic!("capture stopped: {e}"),
            move |r| {
                let _gone = started_tx.send(r);
            },
        )
        .expect("start");
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("start callback")
            .expect("started");
        // Skip the first half second: stream start-up and the encoder-less warm frames.
        let settle = Instant::now() + Duration::from_millis(500);
        while Instant::now() < settle {
            let _skipped = rx.recv_timeout(settle - Instant::now());
        }
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut latencies = Vec::new();
        let mut offsets = Vec::new();
        let mut gaps = 0;
        let mut last_at: Option<u64> = None;
        while let Ok(remaining) = deadline.checked_duration_since(Instant::now()).ok_or(()) {
            let Ok((latency, offset, at)) = rx.recv_timeout(remaining) else {
                break;
            };
            latencies.push(latency);
            offsets.push(offset);
            if let Some(prev) = last_at
                && at.saturating_sub(prev) > 25_000
            {
                gaps += 1;
            }
            last_at = Some(at);
        }
        let (tx, rx) = mpsc::channel();
        capture.stop(move |r| {
            let _gone = tx.send(r);
        });
        let _stopped = rx.recv_timeout(Duration::from_secs(5));
        latencies.sort_unstable();
        offsets.sort_unstable();
        let n = latencies.len();
        let fps = n as f64 / seconds as f64;
        Run {
            n,
            p50: quantile(&latencies, 0.5),
            p95: quantile(&latencies, 0.95),
            max: quantile(&latencies, 1.0),
            offset_p50_us: offsets.get(n / 2).copied().unwrap_or(0),
            gaps,
            fps,
        }
    }

    fn config(size: (u32, u32), depth: u8) -> CaptureConfig {
        CaptureConfig {
            width: size.0,
            height: size.1,
            align: 16,
            fps: 60,
            format: PixelFormat::Nv12Full,
            queue_depth: depth,
        }
    }

    fn row(label: &str, depth: u8, r: &Run) {
        eprintln!(
            "| {label:<14} | {depth} | {:>4} ({:.1} fps) | {:.2} / {:.2} / {:.2} ms | offset {:.2} ms | gaps>25ms {} |",
            r.n,
            r.fps,
            r.p50 as f64 / 1e3,
            r.p95 as f64 / 1e3,
            r.max as f64 / 1e3,
            r.offset_p50_us as f64 / 1e3,
            r.gaps
        );
    }

    /// The luma plane of a frame (NV12 plane 0), row by row.
    fn luma(frame: &CapturedFrame) -> Vec<u8> {
        use objc2_core_video::{
            CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
            CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane,
            CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        };
        let cv = frame.image.as_cv();
        // SAFETY: valid buffer; a read-only lock, unlocked below.
        let locked = unsafe { CVPixelBufferLockBaseAddress(cv, CVPixelBufferLockFlags::ReadOnly) };
        assert_eq!(locked, 0, "lock");
        let (w, h, stride) = (
            CVPixelBufferGetWidthOfPlane(cv, 0),
            CVPixelBufferGetHeightOfPlane(cv, 0),
            CVPixelBufferGetBytesPerRowOfPlane(cv, 0),
        );
        let base = CVPixelBufferGetBaseAddressOfPlane(cv, 0).cast::<u8>();
        let mut out = Vec::with_capacity(w * h);
        for row in 0..h {
            // SAFETY: row `row` starts `row * stride` bytes into the locked plane.
            let start = unsafe { base.add(row * stride) };
            // SAFETY: every row has at least `w` readable bytes while the buffer is locked.
            let line = unsafe { std::slice::from_raw_parts(start, w) };
            out.extend_from_slice(line);
        }
        // SAFETY: matches the lock above.
        let _unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(cv, CVPixelBufferLockFlags::ReadOnly) };
        out
    }

    /// One frame from `target`, luma only: the last of the first few, or of the one a display
    /// that does not change sends (an unchanged screen sends nothing more).
    fn one_frame(target: &Target, config: &CaptureConfig) -> (Vec<u8>, (usize, usize)) {
        let (tx, rx) = mpsc::channel::<CapturedFrame>();
        let (started_tx, started_rx) = mpsc::channel();
        let capture = Capture::start(
            target,
            config,
            move |frame| {
                let _gone = tx.send(frame);
            },
            |e| panic!("capture stopped: {e}"),
            move |r| {
                let _gone = started_tx.send(r);
            },
        )
        .expect("start");
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("start callback")
            .expect("started");
        // The first frames settle the stream's scaling; take the third if it comes.
        let mut frame = rx.recv_timeout(Duration::from_secs(5)).expect("a frame");
        for _ in 0..2 {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(next) => frame = next,
                Err(_) => break,
            }
        }
        let size = (frame.image.width(), frame.image.height());
        let luma = luma(&frame);
        let (tx, rx) = mpsc::channel();
        capture.stop(move |r| {
            let _gone = tx.send(r);
        });
        let _stopped = rx.recv_timeout(Duration::from_secs(5));
        (luma, size)
    }

    /// A padded surface carries the picture pixel for pixel, at its top-left, over black, on the
    /// Retina panel where a point is two pixels. `destinationRect` is documented in pixels; were
    /// it read in points the picture would cover the padding, and were it placed or scaled
    /// differently the picture would not match the unpadded capture's. The picture is sized even
    /// but off 16 on both sides, so each side gets padding.
    #[test]
    fn a_live_padded_capture_is_the_bare_picture_over_black() {
        if !gated() {
            return;
        }
        let display = main_display();
        // A hosted runner's display may be at one pixel a point, where the two cannot be told.
        if (display.point_scale() - 2.0).abs() >= f32::EPSILON {
            onicode_desktop_testkit::live::skip(&format!(
                "a Retina panel, where points and pixels differ: this one is at scale {}",
                display.point_scale()
            ));
            return;
        }
        let off_16 = |side: u32| {
            if side.is_multiple_of(16) {
                side - 2
            } else {
                side
            }
        };
        let (pw, ph) = display.pixel_size();
        let picture = (off_16(pw), off_16(ph));
        let at = |align: u32| CaptureConfig {
            align,
            ..config(picture, 2)
        };
        let (bare, bare_size) = one_frame(&display, &at(1));
        let (padded, padded_size) = one_frame(&display, &at(16));
        let (w, h) = (picture.0 as usize, picture.1 as usize);
        assert_eq!(bare_size, (w, h), "an align of 1 pads nothing");
        assert_eq!(
            padded_size,
            (w.next_multiple_of(16), h.next_multiple_of(16)),
            "padded to 16"
        );
        let surface_w = padded_size.0;

        // 16 px in from every edge is the picture proper.
        let (mut delta, mut lit, mut samples) = (0_u64, 0_u64, 0_u64);
        for y in 16..h - 16 {
            for x in 16..w - 16 {
                let (was, is) = (bare[y * w + x], padded[y * surface_w + x]);
                delta += u64::from(was.abs_diff(is));
                lit += u64::from(was);
                samples += 1;
            }
        }
        let (delta, lit) = (delta as f64 / samples as f64, lit as f64 / samples as f64);
        // The padding: every column right of the picture, and every row below it.
        let padding: Vec<u8> = (0..padded_size.1)
            .flat_map(|y| (0..surface_w).map(move |x| (x, y)))
            .filter(|&(x, y)| x >= w || y >= h)
            .map(|(x, y)| padded[y * surface_w + x])
            .collect();
        let brightest = padding.iter().copied().max().unwrap_or(0);
        let dark = padding.iter().map(|&l| u64::from(l)).sum::<u64>() as f64 / padding.len() as f64;
        eprintln!(
            "{w}×{h} in {}×{}: picture mean |Δluma| {delta:.3}, luma {lit:.1}; padding {} px, \
             mean luma {dark:.2}, max {brightest}",
            padded_size.0,
            padded_size.1,
            padding.len()
        );
        assert!(
            delta < 2.0,
            "the padded picture differs from the bare one: mean |Δ| {delta:.3}"
        );
        assert!(
            brightest <= 4,
            "the padding is black (full-range 0): brightest {brightest}"
        );
    }

    /// A stopped capture, once dropped, lets go of its sink and everything the sink holds: the
    /// host's sink holds its encoder, and a session ScreenCaptureKit kept alive would hold a
    /// hardware encoder for good. A runner's encoder stopped for good after a test that ran
    /// five captures, each with its own encoder (slopty's video decisions).
    #[test]
    fn a_dropped_capture_lets_go_of_its_sink() {
        if !gated() {
            return;
        }
        let display = main_display();
        let held = std::sync::Arc::new(());
        let in_sink = std::sync::Arc::clone(&held);
        let (frames_tx, frames_rx) = mpsc::channel();
        let (started_tx, started_rx) = mpsc::channel();
        let capture = Capture::start(
            &display,
            &config(display.pixel_size(), 2),
            move |_frame: CapturedFrame| {
                let _gone = frames_tx.send(std::sync::Arc::strong_count(&in_sink));
            },
            |e| panic!("capture stopped: {e}"),
            move |r| {
                let _gone = started_tx.send(r);
            },
        )
        .expect("start");
        started_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("start callback")
            .expect("started");
        frames_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a frame");
        let (tx, rx) = mpsc::channel();
        capture.stop(move |r| {
            let _gone = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("stop callback")
            .expect("stopped");
        drop(capture);
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::sync::Arc::strong_count(&held) > 1 && Instant::now() < deadline {
            pause(Duration::from_millis(50));
        }
        assert_eq!(
            std::sync::Arc::strong_count(&held),
            1,
            "the sink outlived its capture by 5 s: ScreenCaptureKit keeps the output"
        );
    }

    /// The ruled floor for a display's capture latency on this machine class (slopty's
    /// MEASUREMENTS, "capture floor": display filter p50 0.00 / p95 0.46 ms). The margin covers a
    /// loaded machine.
    const DISPLAY_P95_FLOOR_US: u64 = 1_000;
    const DISPLAY_P95_MARGIN_US: u64 = 3_000;
    /// Fewer frames than this in the run say the display hardly changed: nothing to time.
    const ENOUGH_FRAMES: usize = 60;

    /// Guard: the display filter's capture latency p95 stays under the ruled floor plus margin.
    #[test]
    fn display_capture_latency_p95_stays_under_the_floor() {
        if !gated() {
            return;
        }
        let display = main_display();
        let r = run(&display, &config(display.pixel_size(), 3), 3);
        row("display", 3, &r);
        if r.n < ENOUGH_FRAMES {
            onicode_desktop_testkit::live::skip(&format!(
                "the display hardly changed ({} frames in 3 s): move something on it",
                r.n
            ));
            return;
        }
        assert!(
            r.p95 <= DISPLAY_P95_FLOOR_US + DISPLAY_P95_MARGIN_US,
            "display capture latency p95 {} µs is over the {} µs floor + {} µs margin",
            r.p95,
            DISPLAY_P95_FLOOR_US,
            DISPLAY_P95_MARGIN_US
        );
    }

    /// The survey: `queueDepth` 2 / 3 / 5 / 8 on the main display. Prints one Markdown row per
    /// run.
    #[test]
    #[ignore = "a measurement; run with ONICODE_DESKTOP_LIVE=1 --run-ignored only --no-capture"]
    fn capture_latency_by_queue_depth() {
        if !gated() {
            return;
        }
        let defaults = sck_defaults();
        eprintln!("SCStreamConfiguration defaults: {defaults:?}");
        let display = main_display();
        let seconds = seconds();
        eprintln!("display {display:?}");
        eprintln!("| path | depth | frames | p50 / p95 / max | display→pts | gaps |");
        eprintln!("| --- | --- | --- | --- | --- | --- |");
        for depth in [2_u8, 3, 5, 8] {
            let r = run(&display, &config(display.pixel_size(), depth), seconds);
            row("display", depth, &r);
        }
    }
}
