//! Posts real events; needs post-event (Accessibility) access, so it is opt-in
//! (`ONICODE_INPUT_LIVE=1`) and skips itself when the process lacks the permission. Nothing here
//! asks for the permission.
//!
//! [`gestures`] posts only to an application the test starts itself
//! (`tests/support/gesture_app.rs`) and reads back what AppKit made of each event:
//! `ONICODE_INPUT_LIVE=1 cargo nextest run --test inject gestures --no-capture`.

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use std::time::Duration;

    use objc2_core_graphics::CGMainDisplayID;
    use onicode_desktop_input::Injector;
    use onicode_desktop_proto::DisplayId;
    use onicode_desktop_proto::screen::{CaptureTarget, ScreenInput};
    use onicode_desktop_testkit::live;

    /// Quarter of the way across and down the main display, in display points.
    const FRACTION: f64 = 0.25;

    #[expect(clippy::cast_possible_truncation, reason = "display points fit f32")]
    const fn to_f32(v: f64) -> f32 {
        v as f32
    }

    /// A display stream's move goes through the HID tap and moves the real pointer; the test
    /// puts it back where it was.
    #[tokio::test]
    async fn moves_the_real_pointer_on_a_display_stream() {
        if !live::opted_in(live::INPUT_LIVE) {
            return;
        }
        if !onicode_desktop_input::can_post() {
            live::skip("no post-event access");
            return;
        }
        let target = CaptureTarget::Display(DisplayId(CGMainDisplayID()));
        let bounds =
            onicode_desktop_capture::target_bounds(target).expect("main display has bounds");
        let (start_x, start_y) = onicode_desktop_capture::pointer_location();

        // A 1:1 stream, so stream pixels are display points.
        let mut injector = Injector::new(target, 1.0);
        let (x, y) = (bounds.w * FRACTION, bounds.h * FRACTION);
        injector
            .inject(&ScreenInput::Move {
                x: to_f32(x),
                y: to_f32(y),
            })
            .expect("post");
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (px, py) = onicode_desktop_capture::pointer_location();
        assert!(
            (px - (bounds.x + x)).abs() < 2.0 && (py - (bounds.y + y)).abs() < 2.0,
            "{px},{py}"
        );

        // Put it back.
        let back = ScreenInput::Move {
            x: to_f32(start_x - bounds.x),
            y: to_f32(start_y - bounds.y),
        };
        injector.inject(&back).expect("post");
    }
}

/// The trackpad gestures a stream posts, as an application receives them: the test's own app,
/// the only process these tests post to, whose window nobody at the Mac sees
/// (`tests/support/gesture_app.rs`, which says what it reads and why at the application's
/// queue). The injector maps into the app's window as it would into a display at that place,
/// and the test's backend posts each event built as the real backend builds it to the app's pid
/// instead of the HID tap, so nothing reaches the desktop of whoever is at the Mac.
#[cfg(test)]
#[cfg(target_os = "macos")]
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a live test printing what it saw and its timings"
)]
mod gestures {
    use std::io::{BufRead as _, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{CGEvent, CGEventFlags, CGEventType, CGMainDisplayID, CGMouseButton};
    use onicode_desktop_capture::Rect;
    use onicode_desktop_input::{Backend, Event, Injector, InputError, Post, Route};
    use onicode_desktop_proto::DisplayId;
    use onicode_desktop_proto::input::{Mods, MouseButton};
    use onicode_desktop_proto::screen::{CaptureTarget, ScreenInput, ScrollPhase, SwipeDirection};
    use onicode_desktop_testkit::live;

    const NONE: ScrollPhase = ScrollPhase::None;
    /// Where in the window the events go, in its points (a 1:1 stream).
    const X: f32 = 200.0;
    const Y: f32 = 150.0;

    /// The app's window as the stream's target, posting to the app alone.
    struct ToApp {
        pid: i32,
        window: Rect,
    }

    impl Backend for ToApp {
        fn bounds(&mut self, _target: CaptureTarget) -> Option<Rect> {
            Some(self.window)
        }

        fn post(&mut self, post: Post) -> Result<(), InputError> {
            let built = onicode_desktop_input::backend::build(&post, None)?;
            CGEvent::post_to_pid(self.pid, Some(&built));
            Ok(())
        }
    }

    /// The test's own application, and what it says.
    struct App {
        child: Child,
        lines: mpsc::Receiver<String>,
        pid: i32,
        /// Its window, in global display points.
        window: Rect,
        swipe_tracking: bool,
    }

    impl App {
        /// The app. Swipe tracking is turned on for it alone, in its own argument domain,
        /// whatever the Mac has.
        fn start() -> Self {
            // Where nextest put it, in an archive unpacked in a guest; else where cargo built it.
            let path = std::env::var_os("NEXTEST_BIN_EXE_onicode-gesture-app")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_onicode-gesture-app").into());
            let mut child = Command::new(path)
                .args(["-AppleEnableSwipeNavigateWithScrolls", "YES"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("the test's app starts");
            let stdout = child.stdout.take().expect("its stdout");
            let (tx, lines) = mpsc::channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let ready = lines
                .recv_timeout(Duration::from_secs(10))
                .expect("the app is ready");
            let field = |key: &str| -> f64 {
                ready
                    .split(' ')
                    .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('=')?.parse().ok())
                    .unwrap_or_else(|| panic!("{key} in {ready:?}"))
            };
            Self {
                pid: field("pid") as i32,
                window: Rect {
                    x: field("x"),
                    y: field("y"),
                    w: field("w"),
                    h: field("h"),
                },
                swipe_tracking: field("swipe_tracking") > 0.5,
                child,
                lines,
            }
        }

        /// An injector for the app's window, which posts to this app and nowhere else.
        fn injector(&self) -> Injector<ToApp> {
            let target = CaptureTarget::Display(DisplayId(CGMainDisplayID()));
            let injector = Injector::with_backend(
                target,
                1.0,
                ToApp {
                    pid: self.pid,
                    window: self.window,
                },
            );
            assert_eq!(injector.route(), Route::Hid, "built as the HID tap's");
            injector
        }

        /// What the app says until a line `last` accepts, that line included.
        fn until(&self, last: impl Fn(&str) -> bool) -> Vec<String> {
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(10))
                .expect("a deadline");
            let mut said = Vec::new();
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let Ok(line) = self.lines.recv_timeout(left) else {
                    panic!("the app never said it; it said {said:#?}");
                };
                let done = last(&line);
                said.push(line);
                if done {
                    return said;
                }
            }
        }

        /// Post `inputs`, then a pointer move, and what the app took off its queue up to that
        /// move: every event posted before it, in order.
        fn post<B: Backend>(
            &self,
            injector: &mut Injector<B>,
            inputs: &[ScreenInput],
        ) -> Vec<String> {
            for input in inputs {
                injector.inject(input).expect("posted");
            }
            injector
                .inject(&ScreenInput::Move { x: 1.0, y: 1.0 })
                .expect("posted");
            let mut said = self.until(|line| line.starts_with("app type=5"));
            said.pop();
            said.retain(|line| line.starts_with("app "));
            said
        }
    }

    impl Drop for App {
        fn drop(&mut self) {
            drop(self.child.stdin.take());
            let _killed = self.child.kill();
            let _waited = self.child.wait();
        }
    }

    /// A drag: the press, each drag and the release reach the app as one press, under one event
    /// number, and the next press under a new one. Posted without it, CoreGraphics numbers them
    /// itself, as the lines it prints show.
    #[test]
    fn a_drag_reaches_the_app_under_its_press_number() {
        if !live() {
            return;
        }
        let app = App::start();
        let mut injector = app.injector();
        let button = |down| ScreenInput::Button {
            button: MouseButton::Left,
            down,
            clicks: 1,
            x: X,
            y: Y,
            mods: Mods::empty(),
        };
        let to = |x| ScreenInput::Move { x, y: Y };
        let said = app.post(
            &mut injector,
            &[
                button(true),
                to(210.0),
                to(220.0),
                to(230.0),
                button(false),
                button(true),
                button(false),
            ],
        );
        eprintln!("numbered: {said:#?}");
        let presses: Vec<(u32, i64)> = said
            .iter()
            .filter_map(|line| {
                let kind = line
                    .strip_prefix("app type=")?
                    .split(' ')
                    .next()?
                    .parse()
                    .ok()?;
                let number = line
                    .split(" number=")
                    .nth(1)?
                    .split(' ')
                    .next()?
                    .parse()
                    .ok()?;
                Some((kind, number))
            })
            .collect();
        // NSEventType: 1 left down, 6 left dragged, 2 left up. AppKit coalesces drags that
        // wait in its queue, so the three may come as fewer.
        let mut kinds: Vec<u32> = presses.iter().map(|(kind, _)| *kind).collect();
        kinds.dedup();
        assert_eq!(kinds, [1, 6, 2, 1, 2], "{said:#?}");
        let released = presses
            .iter()
            .position(|(kind, _)| *kind == 2)
            .expect("a release");
        let (dragged, next) = presses.split_at(released + 1);
        let (first, second) = (dragged[0].1, next[0].1);
        assert!(
            dragged.iter().all(|(_, n)| *n == first),
            "one number: {presses:?}"
        );
        assert!(next.iter().all(|(_, n)| *n == second), "{presses:?}");
        assert_ne!(first, second, "a new press, a new number");

        // The same drag with no number of the injector's own, for the record.
        let bounds = app.window;
        let at = |x: f32| CGPoint {
            x: bounds.x + f64::from(x),
            y: bounds.y + f64::from(Y),
        };
        let raw = |kind, x| Event::Mouse {
            kind,
            at: at(x),
            button: CGMouseButton::Left,
            number: 0,
            clicks: i64::from(kind != CGEventType::LeftMouseDragged),
            press: 0,
        };
        for event in [
            raw(CGEventType::LeftMouseDown, X),
            raw(CGEventType::LeftMouseDragged, 210.0),
            raw(CGEventType::LeftMouseDragged, 220.0),
            raw(CGEventType::LeftMouseUp, 220.0),
        ] {
            let post = Post {
                route: Route::Hid,
                flags: CGEventFlags::empty(),
                event,
            };
            let built = onicode_desktop_input::backend::build(&post, None).expect("built");
            CGEvent::post_to_pid(app.pid, Some(&built));
        }
        let said = app.post(&mut injector, &[]);
        eprintln!("unnumbered: {said:#?}");
    }

    fn live() -> bool {
        if !live::opted_in(live::INPUT_LIVE) {
            return false;
        }
        if !onicode_desktop_input::can_post() {
            live::skip("no post-event access");
            return false;
        }
        true
    }

    /// Wait `for_` between two posts, at the pace a client's input comes.
    fn pace(for_: Duration) {
        std::thread::sleep(for_);
    }

    /// A trackpad's report period: 120 a second.
    const REPORT_US: u32 = 8_333;

    /// A trackpad scroll's report, the client's `k`-th at its trackpad's pace.
    const fn scroll(
        k: u32,
        dx: f32,
        dy: f32,
        phase: ScrollPhase,
        momentum: ScrollPhase,
    ) -> ScreenInput {
        ScreenInput::Scroll {
            dx,
            dy,
            precise: true,
            phase,
            momentum,
            x: X,
            y: Y,
            mods: Mods::empty(),
            time_us: k.wrapping_mul(REPORT_US),
        }
    }

    /// Each gesture reaches the app as the event a trackpad's makes, with its phase and its
    /// amount: a scroll through its phases and its coast, a pinch and a rotation through theirs,
    /// one smart zoom, and exactly one swipe for each swipe, its direction as AppKit reads it.
    #[test]
    fn each_gesture_reaches_the_app_as_a_trackpad_s_does() {
        if !live() {
            return;
        }
        let app = App::start();
        let mut injector = app.injector();
        let (x, y) = (X, Y);
        let said = app.post(
            &mut injector,
            &[
                ScreenInput::Gestures { remote: true },
                scroll(0, 0.0, 2.0, ScrollPhase::Began, NONE),
                scroll(1, 0.0, 6.0, ScrollPhase::Changed, NONE),
                scroll(2, 0.0, 0.0, ScrollPhase::Ended, NONE),
                scroll(3, 0.0, 5.0, NONE, ScrollPhase::Began),
                scroll(4, 0.0, 3.0, NONE, ScrollPhase::Changed),
                scroll(5, 0.0, 0.0, NONE, ScrollPhase::Ended),
                ScreenInput::Magnify {
                    delta: 0.0,
                    phase: ScrollPhase::Began,
                    x,
                    y,
                    time_us: 0,
                },
                ScreenInput::Magnify {
                    delta: 0.25,
                    phase: ScrollPhase::Changed,
                    x,
                    y,
                    time_us: 1,
                },
                ScreenInput::Magnify {
                    delta: -0.5,
                    phase: ScrollPhase::Ended,
                    x,
                    y,
                    time_us: 2,
                },
                ScreenInput::Rotate {
                    degrees: 0.0,
                    phase: ScrollPhase::Began,
                    x,
                    y,
                    time_us: 0,
                },
                ScreenInput::Rotate {
                    degrees: -12.5,
                    phase: ScrollPhase::Changed,
                    x,
                    y,
                    time_us: 1,
                },
                ScreenInput::Rotate {
                    degrees: 0.0,
                    phase: ScrollPhase::Cancelled,
                    x,
                    y,
                    time_us: 2,
                },
                ScreenInput::SmartMagnify { x, y },
                ScreenInput::Swipe {
                    direction: SwipeDirection::Left,
                    x,
                    y,
                },
                ScreenInput::Swipe {
                    direction: SwipeDirection::Right,
                    x,
                    y,
                },
                ScreenInput::Swipe {
                    direction: SwipeDirection::Up,
                    x,
                    y,
                },
                ScreenInput::Swipe {
                    direction: SwipeDirection::Down,
                    x,
                    y,
                },
            ],
        );
        eprintln!("{said:#?}");
        // An accessory app's first event is its own launch (`NSEventTypeAppKitDefined`).
        let said: Vec<&str> = said
            .iter()
            .map(String::as_str)
            .filter(|l| *l != "app type=13")
            .collect();
        assert_eq!(
            said,
            [
                "app type=22 phase=1 momentum=0 dx=0 dy=2 precise=1",
                "app type=22 phase=4 momentum=0 dx=0 dy=6 precise=1",
                "app type=22 phase=8 momentum=0 dx=0 dy=0 precise=1",
                "app type=22 phase=0 momentum=1 dx=0 dy=5 precise=1",
                "app type=22 phase=0 momentum=4 dx=0 dy=3 precise=1",
                "app type=22 phase=0 momentum=8 dx=0 dy=0 precise=1",
                "app type=30 phase=1 amount=0",
                "app type=30 phase=4 amount=0.25",
                "app type=30 phase=8 amount=-0.5",
                "app type=18 phase=1 amount=0",
                "app type=18 phase=4 amount=-12.5",
                "app type=18 phase=16 amount=0",
                "app type=32",
                "app type=31 dx=1 dy=0",
                "app type=31 dx=-1 dy=0",
                "app type=31 dx=0 dy=1",
                "app type=31 dx=0 dy=-1",
            ],
            "a scroll's gesture is AppKit's to read, and no app sees it as an event"
        );
    }

    /// The steps of AppKit's swipe tracking the app reports, until it completes: (amount,
    /// phase) of each.
    fn tracked(app: &App) -> Vec<(f64, u64)> {
        app.until(|line| line.ends_with("complete=1"))
            .iter()
            .filter_map(|line| {
                let rest = line.strip_prefix("track amount=")?;
                let (amount, rest) = rest.split_once(" phase=")?;
                let (phase, _complete) = rest.split_once(' ')?;
                Some((amount.parse().ok()?, phase.parse().ok()?))
            })
            .collect()
    }

    /// How a client's reports reach the host.
    #[derive(Clone, Copy, Debug)]
    enum Arrivals {
        /// Every 8.3 ms, a trackpad's 120 a second.
        Steady,
        /// Each report 0 to 12 ms late, in order: a network's jitter, the same 120 a second.
        Jittered,
        /// Two at once every 16.7 ms, as a link that batches its packets delivers them.
        Pairs,
    }

    impl Arrivals {
        /// When each of `n` reports arrives, from the first.
        fn times(self, n: u32) -> Vec<Duration> {
            let tick = Duration::from_nanos(8_333_333);
            // xorshift64, seeded, so a run is repeatable.
            let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
            let mut late = move || {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                Duration::from_micros(seed % 12_000)
            };
            let mut at = Duration::ZERO;
            (0..n)
                .map(|k| {
                    let nominal = tick.saturating_mul(k);
                    at = match self {
                        Self::Steady => nominal,
                        Self::Jittered => at.max(nominal.saturating_add(late())),
                        Self::Pairs => tick.saturating_mul(k & !1),
                    };
                    at
                })
                .collect()
        }
    }

    /// A two-finger swipe to the side, the way Safari goes back a page: an app following it as
    /// it moves (`trackSwipeEventWithOptions:`) sees it move and turns the page only when the
    /// scroll comes with its gesture. Without (how a stream starts), the tracking stays at its
    /// first step and is cancelled when the fingers lift. A trackpad's swipe, 10 points a report
    /// at 120 reports a second, stamped by the client at that pace, whether they arrive steadily,
    /// jittered or in pairs: the host posts each at the client's spacing, which is what the
    /// lift's verdict reads.
    #[test]
    fn a_swipe_between_pages_follows_the_fingers_only_with_their_gesture() {
        const REPORTS: u32 = 12;
        if !live() {
            return;
        }
        // Stamped by the client at its trackpad's pace, whatever the path then does to them.
        let swipe: Vec<ScreenInput> =
            std::iter::once(scroll(0, -10.0, 0.0, ScrollPhase::Began, NONE))
                .chain((1..REPORTS).map(|k| scroll(k, -10.0, 0.0, ScrollPhase::Changed, NONE)))
                .chain(std::iter::once(scroll(
                    REPORTS,
                    0.0,
                    0.0,
                    ScrollPhase::Ended,
                    NONE,
                )))
                .collect();
        let lifted = |steps: &[(f64, u64)]| {
            let at = steps
                .iter()
                .position(|(_, phase)| *phase != 1 && *phase != 4);
            let (following, after) = steps.split_at(at.unwrap_or(steps.len()));
            (
                following
                    .iter()
                    .map(|(amount, _)| *amount)
                    .collect::<Vec<_>>(),
                after.to_vec(),
            )
        };

        // Each in an app of its own: one whose tracking was cancelled takes no more events.
        let swiped = |remote: bool, arrivals: Arrivals| {
            let app = App::start();
            if !app.swipe_tracking {
                live::skip("two-finger swipe between pages is off");
                return None;
            }
            let mut injector = app.injector();
            injector
                .inject(&ScreenInput::Gestures { remote })
                .expect("posted");
            let times = arrivals.times(REPORTS + 1);
            let start = Instant::now();
            for (input, at) in swipe.iter().zip(&times) {
                pace(at.saturating_sub(start.elapsed()));
                injector.inject(input).expect("posted");
            }
            Some(tracked(&app))
        };
        for arrivals in [Arrivals::Steady, Arrivals::Jittered, Arrivals::Pairs] {
            let Some(alone) = swiped(false, arrivals) else {
                return;
            };
            eprintln!("{arrivals:?}, scroll alone: {alone:?}");
            let (following, after) = lifted(&alone);
            let first = following.first().copied().unwrap_or_default();
            assert!(first.abs() < 0.02, "one report's worth: {following:?}");
            assert!(
                following.iter().all(|a| (a - first).abs() < 1e-3),
                "stayed: {following:?}"
            );
            assert_eq!(
                after.first().map(|(_, phase)| *phase),
                Some(16),
                "cancelled on lifting"
            );
            assert!(
                after.last().is_some_and(|(amount, _)| amount.abs() < 1e-3),
                "{after:?}"
            );

            // How far the tracking gets before the lift is the display's to say: it reports
            // once a frame, and a guest's frames come unevenly. The lift's verdict is the
            // swipe's own, from its events' spacing. A guest's scheduler can still hold a
            // whole swipe up, so it gets a second go (slopty's MEASUREMENTS.md, "a gesture's
            // events keep the client's spacing": 34 of 36 turned at the first).
            let turned = (0..2).find_map(|attempt| {
                let paired = swiped(true, arrivals)?;
                eprintln!("{arrivals:?}, with its gesture (try {attempt}): {paired:?}");
                let (following, after) = lifted(&paired);
                assert!(
                    following.windows(2).all(|w| w[1] <= w[0]),
                    "it follows: {following:?}"
                );
                let ended = after.first().map(|(_, phase)| *phase) == Some(8);
                ended
                    .then(|| after.last().map(|(amount, _)| *amount))
                    .flatten()
            });
            assert!(
                turned.is_some_and(|amount| amount <= -0.999),
                "{arrivals:?}: never turned"
            );
        }
    }

    /// What a trackpad scroll costs the input thread to post, alone and with its gesture
    /// (slopty's MEASUREMENTS.md, "a trackpad scroll's gesture"). Every scroll's travel must
    /// reach the app, though AppKit may coalesce two that wait in its queue into one.
    #[test]
    fn a_trackpad_scroll_s_post_cost() {
        const SCROLLS: usize = 600;
        if !live() {
            return;
        }
        let app = App::start();
        let mut injector = app.injector();
        let mut run = |remote: bool| {
            injector
                .inject(&ScreenInput::Gestures { remote })
                .expect("posted");
            injector
                .inject(&scroll(0, 0.0, 1.0, ScrollPhase::Began, NONE))
                .expect("posted");
            let mut k = 0;
            let mut took: Vec<f64> = std::iter::repeat_with(|| {
                k += 1;
                let from = Instant::now();
                injector
                    .inject(&scroll(k, 0.0, 1.0, ScrollPhase::Changed, NONE))
                    .expect("posted");
                let spent = from.elapsed();
                // A trackpad's pace, 120 a second.
                pace(Duration::from_micros(8_333).saturating_sub(spent));
                spent.as_secs_f64() * 1e6
            })
            .take(SCROLLS)
            .collect();
            injector
                .inject(&scroll(k + 1, 0.0, 0.0, ScrollPhase::Ended, NONE))
                .expect("posted");
            took.sort_by(f64::total_cmp);
            let at = |q: f64| took[((took.len() - 1) as f64 * q).round() as usize];
            (at(0.5), at(0.99), at(1.0))
        };
        let alone = run(false);
        let paired = run(true);
        let said = app.post(&mut injector, &[]);
        let travel: f64 = said
            .iter()
            .filter(|l| l.starts_with("app type=22"))
            .filter_map(|l| {
                l.split(" dy=")
                    .nth(1)?
                    .split(' ')
                    .next()?
                    .parse::<f64>()
                    .ok()
            })
            .sum();
        let scrolls = said.iter().filter(|l| l.starts_with("app type=22")).count();
        eprintln!("{scrolls} scroll events of {} posted", 2 * (SCROLLS + 2));
        let posted = 2.0 * (SCROLLS as f64 + 1.0);
        assert!(
            (travel - posted).abs() < 1e-6,
            "all of it arrived: {travel} of {posted}"
        );
        eprintln!(
            "trackpad scroll post, µs p50 / p99 / max: alone {:.1} / {:.1} / {:.1}, with its \
             gesture {:.1} / {:.1} / {:.1}",
            alone.0, alone.1, alone.2, paired.0, paired.1, paired.2
        );
    }
}
