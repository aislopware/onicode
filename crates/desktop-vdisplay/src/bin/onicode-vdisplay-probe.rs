//! Creates a real virtual display, drives it through its mode, a rotation and teardown, and
//! exits 0 only if each step held. Run by `tests/live.rs` as its own child, because the display
//! must be made on a process's main thread, which a libtest test never runs on.
//!
//! It rearranges the screens of whoever is using the Mac, so only run it on a Mac nobody is
//! using.

use std::io::Write as _;

fn main() -> Result<(), String> {
    let report = probe::run()?;
    writeln!(std::io::stderr(), "{report}").map_err(|e| e.to_string())
}

mod probe {
    use std::time::{Duration, Instant};

    use objc2_core_foundation::{CFRunLoop, kCFRunLoopDefaultMode};
    use objc2_core_graphics::{CGDisplayCopyDisplayMode, CGDisplayIsOnline, CGDisplayMode};
    use onicode_desktop_vdisplay::{ClientKey, Enforced, Plan, Request, VirtualDisplay, plan};

    const SETTLE: Duration = Duration::from_secs(10);

    pub fn run() -> Result<String, String> {
        let client = ClientKey::new(b"onicode-vdisplay-probe");
        let ask = |pixels| Request {
            pixels,
            scale: 2.0,
            refresh_hz: 60,
            client,
        };
        let (landscape, portrait) = (plan(&ask((2560, 1600))), plan(&ask((1600, 2560))));
        let mut display = VirtualDisplay::create(&landscape).map_err(|e| e.to_string())?;
        let id = display.display_id();
        let first = settle(&display, &landscape)?;
        display.resize(&portrait).map_err(|e| e.to_string())?;
        let rotated = settle(&display, &portrait)?;
        drop(display);
        let started = Instant::now();
        while CGDisplayIsOnline(id) {
            if started.elapsed() > SETTLE {
                return Err(format!(
                    "display {id} still online {SETTLE:?} after release"
                ));
            }
            pump();
        }
        Ok(format!(
            "display {id}: {first:?} landscape, {rotated:?} portrait, gone {:?} after release",
            started.elapsed()
        ))
    }

    /// Enforce until the display runs `plan`'s mode; how long that took.
    fn settle(display: &VirtualDisplay, plan: &Plan) -> Result<Duration, String> {
        let started = Instant::now();
        loop {
            if display.enforce().map_err(|e| e.to_string())? == Enforced::Settled {
                let mode = CGDisplayCopyDisplayMode(display.display_id());
                let pixels = (
                    CGDisplayMode::pixel_width(mode.as_deref()),
                    CGDisplayMode::pixel_height(mode.as_deref()),
                );
                let want = (plan.mode.pixels.0 as usize, plan.mode.pixels.1 as usize);
                return if pixels == want {
                    Ok(started.elapsed())
                } else {
                    Err(format!("settled at {pixels:?} pixels, not {want:?}"))
                };
            }
            if started.elapsed() > SETTLE {
                return Err(format!(
                    "mode {:?} not settled within {SETTLE:?}",
                    plan.mode
                ));
            }
            pump();
        }
    }

    fn pump() {
        // SAFETY: framework-provided constant string.
        let mode = unsafe { kCFRunLoopDefaultMode };
        CFRunLoop::run_in_mode(mode, 0.1, false);
    }
}
