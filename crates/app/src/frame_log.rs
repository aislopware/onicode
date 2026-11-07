//! A frame readout for profiling: launched with `ONICODE_FRAME_LOG=<file>`,
//! the app appends one line per second it drew anything, so you can see how
//! many frames streaming or scrolling costs.
//! `open --env ONICODE_FRAME_LOG=/tmp/onicode-frames.log "Onicode.app"`
//!
//! - `fps`: draws per second, across every window.
//! - `draw`: average and slowest time inside GPUI's draw (render, layout, prepaint and paint), in
//!   milliseconds, from each window's [`gpui::Window::observe_draws`].
//! - `busy`: the share of the second spent drawing.
//! - `main`: main-thread CPU over the second, the number to keep low.
//! - after `|`: counts the app adds with [`add`].
use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{App, Subscription, WindowId};

const PERIOD: Duration = Duration::from_secs(1);

static ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// The app's named counts since the last line, kept on the main thread.
    static COUNTERS: RefCell<HashMap<&'static str, u32>> = RefCell::default();
}

/// Adds `amount` to a named count shown on the frame log's line, for app code that wants to see
/// how often something expensive happens or how long it took. A no-op unless the log is on; call
/// it on the main thread.
pub(crate) fn add(name: &'static str, amount: u32) {
    if ENABLED.load(Ordering::Relaxed) {
        COUNTERS.with_borrow_mut(|counters| {
            let total = counters.entry(name).or_default();
            *total = total.saturating_add(amount);
        });
    }
}

/// What the windows drew since the last line.
#[derive(Default)]
struct Draws {
    frames: u32,
    draw_time: Duration,
    max_draw: Duration,
}

pub(crate) fn start(cx: &mut App) {
    let Some(path) = std::env::var_os("ONICODE_FRAME_LOG") else {
        return;
    };
    ENABLED.store(true, Ordering::Relaxed);
    let draws = Rc::new(RefCell::new(Draws::default()));
    cx.spawn(async move |cx| {
        // The task runs on the main thread, so this is the thread to measure.
        let mut cpu = MainThreadCpu::new();
        let mut last = Instant::now();
        let mut observed: HashMap<WindowId, Subscription> = HashMap::new();
        loop {
            cx.update(|cx| observe_windows(&mut observed, &draws, cx));
            cx.background_executor().timer(PERIOD).await;
            let stats = std::mem::take(&mut *draws.borrow_mut());
            let mut counters: Vec<_> = COUNTERS.with_borrow_mut(|c| c.drain().collect());
            counters.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
            let now = Instant::now();
            let main = cpu.percent_since_last(now.duration_since(last));
            last = now;
            if stats.frames == 0 {
                continue;
            }
            let line = format_line(&stats, &counters, main);
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                let _ = writeln!(file, "{line}");
            }
        }
    })
    .detach();
}

/// Times the draws of every window opened since the last call, and forgets closed ones.
fn observe_windows(
    observed: &mut HashMap<WindowId, Subscription>,
    draws: &Rc<RefCell<Draws>>,
    cx: &mut App,
) {
    let windows = cx.windows();
    observed.retain(|id, _| windows.iter().any(|window| window.window_id() == *id));
    for window in windows {
        let id = window.window_id();
        if observed.contains_key(&id) {
            continue;
        }
        let draws = draws.clone();
        let subscription = window.update(cx, |_, window, cx| {
            window.observe_draws(cx, move |_, took, _| {
                let mut draws = draws.borrow_mut();
                draws.frames += 1;
                draws.draw_time += took;
                draws.max_draw = draws.max_draw.max(took);
            })
        });
        if let Ok(subscription) = subscription {
            observed.insert(id, subscription);
        }
    }
}

fn format_line(stats: &Draws, counters: &[(&'static str, u32)], main: Option<f64>) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1000.;
    let average = ms(stats.draw_time) / f64::from(stats.frames.max(1));
    let counters = counters
        .iter()
        .map(|(name, count)| format!(" {name}={count}"))
        .collect::<String>();
    let main = main.map_or_else(|| "?".into(), |main| format!("{main:.1}%"));
    format!(
        "{} fps={} draw={average:.2}/{:.2}ms busy={:.1}% main={main}{}",
        chrono::Local::now().format("%H:%M:%S"),
        stats.frames,
        ms(stats.max_draw),
        100. * stats.draw_time.as_secs_f64() / PERIOD.as_secs_f64(),
        if counters.is_empty() {
            String::new()
        } else {
            format!(" |{counters}")
        },
    )
}

/// CPU time of the thread that created it, read through Mach.
struct MainThreadCpu {
    #[cfg(target_os = "macos")]
    thread: libc::mach_port_t,
    last: Option<Duration>,
}

impl MainThreadCpu {
    fn new() -> Self {
        let mut cpu = Self {
            // SAFETY: names the calling thread's Mach port without taking a
            // new right, so there is nothing to deallocate.
            #[cfg(target_os = "macos")]
            thread: unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) },
            last: None,
        };
        cpu.last = cpu.used();
        cpu
    }

    fn percent_since_last(&mut self, wall: Duration) -> Option<f64> {
        let used = self.used()?;
        let previous = self.last.replace(used)?;
        let busy = used.saturating_sub(previous);
        Some(100. * busy.as_secs_f64() / wall.as_secs_f64().max(f64::EPSILON))
    }

    #[cfg(target_os = "macos")]
    fn used(&self) -> Option<Duration> {
        let mut info = std::mem::MaybeUninit::<libc::thread_basic_info>::zeroed();
        let mut count = libc::THREAD_BASIC_INFO_COUNT;
        // SAFETY: the buffer is a thread_basic_info and `count` its size in
        // integers, which is what THREAD_BASIC_INFO fills.
        let status = unsafe {
            libc::thread_info(
                self.thread,
                libc::THREAD_BASIC_INFO as u32,
                info.as_mut_ptr().cast(),
                &mut count,
            )
        };
        if status != libc::KERN_SUCCESS {
            return None;
        }
        // SAFETY: a successful call initialized the struct.
        let info = unsafe { info.assume_init() };
        let time = |t: libc::time_value_t| {
            Duration::from_secs(t.seconds as u64) + Duration::from_micros(t.microseconds as u64)
        };
        Some(time(info.user_time) + time(info.system_time))
    }
}
