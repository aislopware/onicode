//! What a key costs on its way from the keyboard to a remote tile's view while the host types
//! under the client's input source, and what the host's answer costs when the source is the
//! one it already has (slopty's MEASUREMENTS.md, "a taken key's hop and the input-source
//! answer").
//!
//! A real `NSApplication` runs on this binary's main thread (no window, no Dock icon). Key
//! events made here are posted into this process's own event queue (`-[NSApplication
//! postEvent:atStart:]`, never `CGEventPost`, so nothing reaches another app), where the real
//! monitor (`onicode_desktop_input::keyboard::take_keys`) takes them off `sendEvent:`. Its wake
//! hops to the main queue, as GPUI's foreground executor does when the view's task is woken, and
//! the drain there is where the view sends the key. The answer is asked of the real input sources
//! (`Sources::system`) for the source this Mac is under, which selects nothing.
//!
//! Opt-in, run by hand:
//! `ONICODE_MEASURE=1 cargo test --release --test key_path`. Without the variable, and when a
//! runner lists tests, it does nothing.
//!
//! `ONICODE_MEASURE_SWITCH=<TIS id>` (with `ONICODE_MEASURE=1`) also times a real switch: this Mac
//! is put under that source and back, 20 times, and each answer is timed from the ask to the
//! host hearing the switch (`Sources::heard`, as the host's main run loop hears it). It
//! switches the input source of whoever uses this Mac, so it is for a Mac nobody is typing on,
//! and it also needs `ONICODE_INPUT_LIVE=1`: `ONICODE_MEASURE=1 ONICODE_INPUT_LIVE=1
//! ONICODE_MEASURE_SWITCH=com.apple.keylayout.French cargo test --release --test key_path`.

#[cfg(target_os = "macos")]
#[expect(
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a measurement printing its quantiles"
)]
mod mac {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    use dispatch2::DispatchQueue;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
    };
    use objc2_foundation::{NSPoint, NSString};
    use onicode_desktop_input::input_source;
    use onicode_desktop_input::keyboard::{Taken, take_keys};
    use onicode_desktop_input::sources::{Claimant, Sources};

    /// Key presses, each with its release.
    const KEYS: usize = 1000;
    /// Between two presses.
    const EVERY: Duration = Duration::from_millis(4);
    /// Answers asked for.
    const ASKS: usize = 300;

    #[derive(Default)]
    struct Timings {
        posted: VecDeque<Instant>,
        taken: VecDeque<Instant>,
        to_monitor: Vec<f64>,
        to_drain: Vec<f64>,
    }

    thread_local! {
        static TAKEN: RefCell<Option<Taken>> = const { RefCell::new(None) };
        static TIMINGS: RefCell<Timings> = RefCell::new(Timings::default());
        /// The switch watch, on the main thread, for the rest of the run.
        static WATCH: RefCell<Option<input_source::Watch>> =
            const { RefCell::new(None) };
    }

    fn quantiles(label: &str, mut us: Vec<f64>) {
        us.sort_by(f64::total_cmp);
        let at = |q: f64| {
            let i = ((us.len().saturating_sub(1)) as f64 * q).round() as usize;
            us.get(i).copied().unwrap_or(0.0)
        };
        eprintln!(
            "{label}: n {} p50 {:.1} / p90 {:.1} / p99 {:.1} / max {:.1} µs",
            us.len(),
            at(0.5),
            at(0.9),
            at(0.99),
            us.last().copied().unwrap_or(0.0),
        );
    }

    /// The view's drain: each key taken since the last, timed from its monitor call.
    fn drain() {
        let now = Instant::now();
        let keys = TAKEN.with(|t| t.borrow().as_ref().map(Taken::drain).unwrap_or_default());
        TIMINGS.with(|t| {
            let mut t = t.borrow_mut();
            for _ in keys {
                if let Some(at) = t.taken.pop_front() {
                    t.to_drain.push(now.duration_since(at).as_secs_f64() * 1e6);
                }
            }
        });
    }

    /// The monitor took a key: the view's task is woken on the main queue.
    fn woken() {
        let now = Instant::now();
        TIMINGS.with(|t| {
            let mut t = t.borrow_mut();
            t.taken.push_back(now);
            if let Some(at) = t.posted.pop_front() {
                t.to_monitor
                    .push(now.duration_since(at).as_secs_f64() * 1e6);
            }
        });
        DispatchQueue::main().exec_async(drain);
    }

    /// Post the A key's press or release into this process's own event queue.
    fn post(down: bool) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let kind = if down {
            NSEventType::KeyDown
        } else {
            NSEventType::KeyUp
        };
        let a = NSString::from_str("a");
        let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            kind,
            NSPoint::ZERO,
            NSEventModifierFlags::empty(),
            0.0,
            0,
            None,
            &a,
            &a,
            false,
            0x00,
        );
        let Some(event) = event else { return };
        TIMINGS.with(|t| t.borrow_mut().posted.push_back(Instant::now()));
        app.postEvent_atStart(&event, false);
    }

    /// Run `f` on the main queue and wait for its answer.
    fn on_main<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
        let (answer, answered) = std::sync::mpsc::channel();
        DispatchQueue::main().exec_async(move || {
            let _gone = answer.send(f());
        });
        answered.recv().ok()
    }

    /// Ask for the source this Mac is under, `ASKS` times: the answer's round trip through the
    /// main queue. Selects nothing.
    fn asks() -> Vec<f64> {
        let current = on_main(input_source::current).flatten();
        let Some(current) = current else {
            eprintln!("no input source to ask for");
            return Vec::new();
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            return Vec::new();
        };
        // Nothing is kept on disk and nothing released: the claim only ever held the source
        // that was current, and dropping it calls nothing.
        let sources = Sources::system();
        let who = Claimant::next();
        std::iter::repeat_with(|| {
            let started = Instant::now();
            let applied = runtime.block_on(sources.claim(who, current.clone()));
            assert!(applied, "the current source is taken as it is");
            started.elapsed().as_secs_f64() * 1e6
        })
        .take(ASKS)
        .collect()
    }

    /// Put this Mac under `other` and back, `SWITCHES` times each way: each answer timed from the
    /// ask to the switch being heard. The claim is released as the host's stream would be, so
    /// the source this Mac had comes back.
    fn switches(other: &str) -> Vec<f64> {
        const SWITCHES: usize = 20;
        let Some(Some(own)) = on_main(input_source::current) else {
            return Vec::new();
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            return Vec::new();
        };
        let sources = Sources::system();
        let heard = sources.clone();
        let watching = on_main(move || {
            let watch = input_source::Watch::new(Box::new(move || {
                heard.heard(input_source::current());
            }));
            let watching = watch.is_some();
            WATCH.with(|kept| kept.replace(watch));
            watching
        });
        if watching != Some(true) {
            eprintln!("switches unheard: nothing to time");
            return Vec::new();
        }
        sources.hearing();
        let who = Claimant::next();
        let mut times = Vec::with_capacity(SWITCHES * 2);
        for _ in 0..SWITCHES {
            for to in [other, own.as_str()] {
                let started = Instant::now();
                let applied = runtime.block_on(sources.claim(who, to.to_owned()));
                times.push(started.elapsed().as_secs_f64() * 1e6);
                if !applied {
                    eprintln!("{to} was not taken");
                }
            }
        }
        sources.release(who);
        times
    }

    /// `TISCopyCurrentKeyboardInputSource` and its id, on the main thread.
    fn current_reads() -> Vec<f64> {
        on_main(|| {
            std::iter::repeat_with(|| {
                let started = Instant::now();
                let _current = input_source::current();
                started.elapsed().as_secs_f64() * 1e6
            })
            .take(ASKS)
            .collect()
        })
        .unwrap_or_default()
    }

    pub fn run() {
        let Some(mtm) = MainThreadMarker::new() else {
            eprintln!("not on the main thread");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
        let taken = take_keys(Box::new(woken));
        assert!(taken.is_some(), "the monitor is installed");
        TAKEN.with(|t| t.replace(taken));
        let driver = std::thread::spawn(|| {
            for _ in 0..KEYS {
                DispatchQueue::main().exec_async(|| post(true));
                DispatchQueue::main().exec_async(|| post(false));

                std::thread::sleep(EVERY);
            }
            let asked = asks();
            let read = current_reads();
            // Switching changes the input source of whoever uses this Mac: opt-in on its own.
            let switched = std::env::var("ONICODE_MEASURE_SWITCH")
                .ok()
                .filter(|_| {
                    onicode_desktop_testkit::live::opted_in(
                        onicode_desktop_testkit::live::INPUT_LIVE,
                    )
                })
                .map(|other| switches(&other));
            DispatchQueue::main().exec_async(move || {
                TIMINGS.with(|t| {
                    let t = t.take();
                    quantiles(
                        "post to monitor (AppKit's queue to sendEvent:)",
                        t.to_monitor,
                    );
                    quantiles("monitor to drain (the hop the taken path adds)", t.to_drain);
                });
                quantiles(
                    "answer for the current source (main queue round trip)",
                    asked,
                );
                quantiles("TISCopyCurrentKeyboardInputSource and its id", read);
                if let Some(switched) = switched {
                    quantiles(
                        "answer for a switch, from the ask to the switch heard",
                        switched,
                    );
                }
                #[expect(clippy::exit, reason = "NSApplication's run loop never returns")]
                std::process::exit(0);
            });
        });
        app.run();
        let _ended = driver.join();
    }
}

fn main() {
    let measuring = !std::env::args().any(|arg| arg == "--list")
        && std::env::var_os("ONICODE_MEASURE").is_some();
    #[cfg(target_os = "macos")]
    if measuring {
        mac::run();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = measuring;
}
