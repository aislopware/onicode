//! What one client input event costs the thread that hands it to the injector: the stream's
//! tokio task on the host. Real window-server reads (the main display's bounds) and no event
//! posted, so it needs no Accessibility grant and leaves the desktop alone. A measurement, run by
//! hand (slopty's MEASUREMENTS.md, "input injection off the runtime" and "moves queued behind a
//! stall").

#[cfg(test)]
#[expect(
    clippy::cast_precision_loss,
    reason = "measurement arithmetic on small counts"
)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CGPoint;
    use objc2_core_graphics::{CGEvent, CGEventType, CGMainDisplayID};
    use onicode_desktop_capture::Rect;
    use onicode_desktop_input::{
        Backend, Event, Injector, InputError, InputSink as _, InputThread, Post, System, to_point,
    };
    use onicode_desktop_proto::DisplayId;
    use onicode_desktop_proto::input::{KeyAction, KeyCode, Mods, MouseButton};
    use onicode_desktop_proto::screen::{CaptureTarget, ScreenInput};

    /// `System`'s reads, counted; posting does nothing but note when the post happened.
    #[derive(Clone, Debug, Default)]
    struct Dry {
        /// Bounds reads in front of an event.
        bounds_reads: Arc<AtomicUsize>,
        posted: Option<mpsc::Sender<(Instant, Post)>>,
    }

    impl Backend for Dry {
        fn bounds(&mut self, target: CaptureTarget) -> Option<Rect> {
            self.bounds_reads.fetch_add(1, Ordering::Relaxed);
            System.bounds(target)
        }

        fn post(&mut self, post: Post) -> Result<(), InputError> {
            if let Some(posted) = &self.posted {
                let _gone = posted.send((Instant::now(), post));
            }
            Ok(())
        }
    }

    const MOVES: usize = 1500;
    const MOVE_EVERY: Duration = Duration::from_millis(2);
    /// Moves between two geometry probes: 100 ms.
    const PROBE_EVERY: usize = 50;
    const KEYS: usize = 200;
    const KEY_EVERY: Duration = Duration::from_millis(5);

    fn pace(started: Instant, every: Duration) {
        std::thread::sleep(every.saturating_sub(started.elapsed()));
    }

    fn quantiles(label: &str, mut us: Vec<f64>) {
        us.sort_by(f64::total_cmp);
        let at = |q: f64| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "index"
            )]
            let i = ((us.len().saturating_sub(1)) as f64 * q).round() as usize;
            us.get(i).copied().unwrap_or(0.0)
        };
        eprintln!(
            "{label}: n {} p50 {:.1} / p99 {:.1} / max {:.1} µs",
            us.len(),
            at(0.5),
            at(0.99),
            us.last().copied().unwrap_or(0.0),
        );
    }

    /// One event handed to the sink: when, and where a pointer event should land (`None` for a
    /// key), which is how its post is found again.
    type Handed = (Instant, Option<CGPoint>);

    /// Each post matched to the event it came from, in order; an event with no post of its own
    /// (a move the thread passed over) is skipped. The delays of every post, the button posts'
    /// alone, and how many events went unposted.
    fn delays(handed: &[Handed], posts: impl Iterator<Item = (Instant, Post)>) -> Delays {
        let mut out = Delays::default();
        let mut next = handed.iter();
        for (posted, post) in posts {
            let (at, release) = match post.event {
                Event::Mouse { at, kind, .. } => (Some(at), kind == CGEventType::LeftMouseUp),
                Event::Key { .. }
                | Event::Scroll { .. }
                | Event::Media { .. }
                | Event::Gesture { .. } => (None, false),
            };
            let Some((handed_at, _)) = next.by_ref().find(|(_, want)| match (want, at) {
                (Some(want), Some(at)) => {
                    (want.x - at.x).abs() < 1e-6 && (want.y - at.y).abs() < 1e-6
                }
                (None, None) => true,
                _ => false,
            }) else {
                out.unmatched = out.unmatched.saturating_add(1);
                continue;
            };
            let us = posted.duration_since(*handed_at).as_secs_f64() * 1e6;
            out.all.push(us);
            if release {
                out.releases.push(us);
            }
        }
        out.skipped = handed.len().saturating_sub(out.all.len());
        out
    }

    #[derive(Debug, Default)]
    struct Delays {
        all: Vec<f64>,
        releases: Vec<f64>,
        skipped: usize,
        unmatched: usize,
    }

    /// What the stream's task hands the injector: an event, or the bounds its geometry probe
    /// read.
    enum Feed<'a> {
        Input(&'a ScreenInput),
        Bounds(Option<Rect>),
    }

    /// Moves at 500 Hz with the bounds read every 100 ms beside them, as the stream's geometry
    /// probe does, then key presses at 200 Hz, each timed on the calling thread, with the
    /// window-server reads made in front of an event. Returns when each event was handed over
    /// and where it should land, in order.
    fn drive(
        label: &str,
        target: CaptureTarget,
        scale: f64,
        mut feed: impl FnMut(Feed<'_>),
        dry: &Dry,
    ) -> Vec<Handed> {
        let mut handed = Vec::with_capacity(MOVES + 2 * KEYS);
        let mut moves = Vec::with_capacity(MOVES);
        let mut rect = None;
        for n in 0..MOVES {
            if n % PROBE_EVERY == 0 {
                rect = System.bounds(target);
                feed(Feed::Bounds(rect));
            }
            let started = Instant::now();
            // Every move lands somewhere of its own, so its post can be found again.
            let (x, y) = ((n % 400) as f32, (n / 400) as f32);
            handed.push((
                Instant::now(),
                rect.map(|r| to_point(r, scale, f64::from(x), f64::from(y))),
            ));
            feed(Feed::Input(&ScreenInput::Move { x, y }));
            moves.push(started.elapsed().as_secs_f64() * 1e6);
            pace(started, MOVE_EVERY);
        }
        let reads = dry.bounds_reads.swap(0, Ordering::Relaxed);
        quantiles(&format!("{label} move, caller"), moves);
        eprintln!("{label}: {reads} bounds reads in the event path, {MOVES} moves");
        let mut keys = Vec::with_capacity(KEYS);
        for _ in 0..KEYS {
            let started = Instant::now();
            handed.push((Instant::now(), None));
            feed(Feed::Input(&ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Press,
                mods: Mods::empty(),
            }));
            keys.push(started.elapsed().as_secs_f64() * 1e6);
            handed.push((Instant::now(), None));
            feed(Feed::Input(&ScreenInput::Key {
                code: KeyCode::A,
                action: KeyAction::Release,
                mods: Mods::empty(),
            }));
            pace(started, KEY_EVERY);
        }
        quantiles(&format!("{label} key press, caller"), keys);
        handed
    }

    #[test]
    #[ignore = "measurement"]
    fn injection_cost_on_the_callers_thread() {
        let target = CaptureTarget::Display(DisplayId(CGMainDisplayID()));
        let dry = Dry::default();
        // The injector as it was called before the input thread: on the caller, reading the
        // bounds itself.
        let mut inline = Injector::with_backend(target, 2.0, dry.clone());
        drive(
            "inline",
            target,
            2.0,
            |feed| {
                if let Feed::Input(input) = feed {
                    let _posted = inline.inject(input);
                }
            },
            &dry,
        );

        let (posted, posts) = mpsc::channel();
        let dry = Dry {
            posted: Some(posted),
            ..Dry::default()
        };
        let mut thread = InputThread::spawn(target, 2.0, dry.clone());
        let handed = drive(
            "thread",
            target,
            2.0,
            |feed| match feed {
                Feed::Input(input) => {
                    let _queued = thread.inject(input);
                }
                Feed::Bounds(bounds) => thread.set_bounds(bounds, Instant::now()),
            },
            &dry,
        );
        drop(thread);
        drop(dry);
        let delays = delays(&handed, posts.iter());
        eprintln!(
            "thread: {} events handed, {} passed over, {} posts unmatched",
            handed.len(),
            delays.skipped,
            delays.unmatched
        );
        quantiles("thread handed → posted", delays.all);
    }

    /// How long a stall in front of a button-down holds the input thread: 17 ms, the worst
    /// measured in front of a press (slopty's MEASUREMENTS.md, "input injection off the
    /// runtime").
    const STALL: Duration = Duration::from_millis(17);

    /// A display whose press posts stall like the slowest real stall, and whose posts build the
    /// `CGEvent` the real backend would post, without posting it.
    #[derive(Debug)]
    struct Stalling {
        posted: mpsc::Sender<(Instant, Post)>,
    }

    const RECT: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 2000.0,
        h: 2000.0,
    };

    impl Backend for Stalling {
        fn bounds(&mut self, _target: CaptureTarget) -> Option<Rect> {
            Some(RECT)
        }

        fn post(&mut self, post: Post) -> Result<(), InputError> {
            if let Event::Mouse {
                kind, at, button, ..
            } = post.event
            {
                if kind == CGEventType::LeftMouseDown {
                    std::thread::sleep(STALL);
                }
                let _built = CGEvent::new_mouse_event(None, kind, at, button);
            }
            let _gone = self.posted.send((Instant::now(), post));
            Ok(())
        }
    }

    /// Clicks in a stream of moves at 500 Hz, each press holding the input thread for
    /// [`STALL`]: every move and the release handed over meanwhile queue
    /// behind it. How long the release waits, and how many of the queued moves are posted.
    #[test]
    #[ignore = "measurement"]
    fn moves_queued_behind_a_stall() {
        const CLICKS: usize = 40;
        /// Moves per click: 300 ms.
        const ROUND: usize = 150;
        const PRESS_AT: usize = 50;
        /// The release 10 ms after the press, in the middle of the stall.
        const RELEASE_AT: usize = 55;
        // The first event a process builds connects to the window server, which takes seconds.
        let _warm = CGEvent::new_mouse_event(
            None,
            CGEventType::MouseMoved,
            CGPoint::new(0.0, 0.0),
            objc2_core_graphics::CGMouseButton::Left,
        );
        let (posted, posts) = mpsc::channel();
        let target = CaptureTarget::Display(DisplayId(1));
        let mut thread = InputThread::spawn(target, 1.0, Stalling { posted });
        let mut handed: Vec<Handed> = Vec::with_capacity(CLICKS * (ROUND + 2));
        let mut n = 0_u32;
        // Every event lands somewhere of its own, so its post can be found again.
        let mut hand = |thread: &mut InputThread, input: &dyn Fn(f32, f32) -> ScreenInput| {
            n = n.wrapping_add(1);
            let (x, y) = ((n % 1000) as f32, (n / 1000) as f32);
            handed.push((
                Instant::now(),
                Some(to_point(RECT, 1.0, f64::from(x), f64::from(y))),
            ));
            let _queued = thread.inject(&input(x, y));
        };
        for _ in 0..CLICKS {
            for m in 0..ROUND {
                let started = Instant::now();
                if m % PROBE_EVERY == 0 {
                    thread.set_bounds(Some(RECT), Instant::now());
                }
                if m == PRESS_AT || m == RELEASE_AT {
                    let down = m == PRESS_AT;
                    hand(&mut thread, &|x, y| ScreenInput::Button {
                        button: MouseButton::Left,
                        down,
                        clicks: 1,
                        x,
                        y,
                        mods: Mods::empty(),
                    });
                }
                hand(&mut thread, &|x, y| ScreenInput::Move { x, y });
                pace(started, MOVE_EVERY);
            }
        }
        drop(thread);
        let delays = delays(&handed, posts.iter());
        eprintln!(
            "stall {STALL:?}: {} events handed, {} passed over, {} posts unmatched",
            handed.len(),
            delays.skipped,
            delays.unmatched
        );
        quantiles("stalled handed → posted, every post", delays.all);
        quantiles("stalled handed → posted, releases", delays.releases);
    }

    /// A display's decisions with nothing posted, and the event clock as the real backend reads
    /// it or never read.
    #[derive(Debug)]
    struct Decide {
        clock: bool,
    }

    impl Backend for Decide {
        fn bounds(&mut self, _target: CaptureTarget) -> Option<Rect> {
            Some(RECT)
        }

        fn post(&mut self, post: Post) -> Result<(), InputError> {
            std::hint::black_box(post);
            Ok(())
        }

        fn event_clock(&mut self) -> u64 {
            if self.clock {
                onicode_desktop_capture::host_now_us().saturating_mul(1000)
            } else {
                0
            }
        }
    }

    /// What the injector's own decisions cost one input on the stream's input thread, with
    /// nothing built or posted: a trackpad scroll with its gesture, stamped at the client's
    /// spacing or left to the system's clock, and a move with no button held and with one
    /// (slopty's MEASUREMENTS.md, "a gesture's events keep the client's spacing").
    #[test]
    #[ignore = "measurement"]
    fn the_injector_s_own_cost_per_input() {
        const N: u32 = 200_000;
        let target = CaptureTarget::Display(DisplayId(1));
        let per = |label: &str, clock: bool, drag: bool, scroll: bool| {
            let mut injector = Injector::with_backend(target, 1.0, Decide { clock });
            injector
                .inject(&ScreenInput::Gestures { remote: true })
                .expect("taken");
            if drag {
                let press = ScreenInput::Button {
                    button: MouseButton::Left,
                    down: true,
                    clicks: 1,
                    x: 10.0,
                    y: 10.0,
                    mods: Mods::empty(),
                };
                injector.inject(&press).expect("pressed");
            }
            let started = Instant::now();
            for k in 0..N {
                #[expect(clippy::cast_precision_loss, reason = "a small count")]
                let x = (k % 1000) as f32;
                let input = if scroll {
                    ScreenInput::Scroll {
                        dx: -1.0,
                        dy: 0.0,
                        precise: true,
                        phase: if k == 0 {
                            onicode_desktop_proto::screen::ScrollPhase::Began
                        } else {
                            onicode_desktop_proto::screen::ScrollPhase::Changed
                        },
                        momentum: onicode_desktop_proto::screen::ScrollPhase::None,
                        x,
                        y: 10.0,
                        mods: Mods::empty(),
                        time_us: k.wrapping_mul(8_333),
                    }
                } else {
                    ScreenInput::Move { x, y: 10.0 }
                };
                injector
                    .inject(std::hint::black_box(&input))
                    .expect("taken");
            }
            let ns = started.elapsed().as_nanos() as f64 / f64::from(N);
            eprintln!("{label}: {ns:.0} ns an input");
        };
        for _ in 0..3 {
            per("scroll and its gesture, system's stamp", false, false, true);
            per(
                "scroll and its gesture, client's spacing",
                true,
                false,
                true,
            );
            per("move", false, false, false);
            per("move in a drag", false, true, false);
        }
    }
}
