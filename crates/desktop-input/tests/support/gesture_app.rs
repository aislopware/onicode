//! An application that says what AppKit makes of the events posted to it, for the live gesture
//! test in `tests/inject.rs`: its parent, and the only process that posts to it.
//!
//! It opens one borderless window that nobody at the Mac sees and that catches none of their
//! pointer (clear, shadowless, at the desktop's level under every window), on the main display
//! because AppKit's swipe tracking cancels a gesture whose point is on none, and it never takes
//! the keyboard (an accessory app, ordered in with `orderFrontRegardless`). It writes `ready
//! pid=<pid> x=<x> y=<y> w=<w> h=<h> swipe_tracking=<0|1>` on stdout, the window's frame in
//! global display points (from the main display's top left), then one line for each event it
//! takes off its queue, before AppKit dispatches it (`app type=<NSEventType> …`, with the fields
//! AppKit defines for that type; a press, a drag or a release is read and not dispatched, so
//! nothing raises the app). A sideways trackpad scroll is followed as Safari follows a swipe
//! between pages (`trackSwipeEventWithOptions:…`), one `track …` line for each step AppKit
//! reports. It leaves when its stdin closes, so it never outlives the test.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::Cell;
    use std::io::Write as _;
    use std::process::ExitCode;
    use std::ptr::NonNull;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2::{MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSEvent,
        NSEventMask, NSEventPhase, NSEventSwipeTrackingOptions, NSEventType, NSWindow,
        NSWindowStyleMask,
    };
    use objc2_core_graphics::{
        CGDisplayBounds, CGMainDisplayID, CGWindowLevelForKey, CGWindowLevelKey,
    };
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode, NSPoint, NSRect, NSSize};

    /// On the main display, which AppKit's swipe tracking needs a gesture's point to be on.
    const ORIGIN: NSPoint = NSPoint { x: 0.0, y: 0.0 };
    const SIZE: NSSize = NSSize {
        width: 400.0,
        height: 300.0,
    };

    /// One line on stdout, flushed: the parent reads them as they come.
    fn say(line: &str) {
        let mut out = std::io::stdout().lock();
        let _written = writeln!(out, "{line}").and_then(|()| out.flush());
    }

    /// `event`'s type and the fields AppKit defines for it; reading one it does not define for
    /// the type raises, so each type reads only its own.
    fn describe(event: &NSEvent) -> String {
        let kind = event.r#type();
        let phase = || event.phase().0;
        let fields = match kind {
            NSEventType::ScrollWheel => format!(
                " phase={} momentum={} dx={} dy={} precise={}",
                phase(),
                event.momentumPhase().0,
                event.scrollingDeltaX(),
                event.scrollingDeltaY(),
                u8::from(event.hasPreciseScrollingDeltas()),
            ),
            NSEventType::Magnify => format!(" phase={} amount={}", phase(), event.magnification()),
            NSEventType::Rotate => format!(" phase={} amount={}", phase(), event.rotation()),
            NSEventType::Swipe => format!(" dx={} dy={}", event.deltaX(), event.deltaY()),
            NSEventType::Gesture => format!(" phase={}", phase()),
            _ if is_press(kind) => {
                format!(
                    " number={} button={}",
                    event.eventNumber(),
                    event.buttonNumber()
                )
            }
            _ => String::new(),
        };
        format!("type={}{fields}", kind.0)
    }

    /// A press, a drag or a release of any button.
    fn is_press(kind: NSEventType) -> bool {
        [
            NSEventType::LeftMouseDown,
            NSEventType::LeftMouseDragged,
            NSEventType::LeftMouseUp,
            NSEventType::RightMouseDown,
            NSEventType::RightMouseDragged,
            NSEventType::RightMouseUp,
            NSEventType::OtherMouseDown,
            NSEventType::OtherMouseDragged,
            NSEventType::OtherMouseUp,
        ]
        .contains(&kind)
    }

    /// Follow a sideways trackpad scroll as Safari follows a swipe between pages: from the
    /// first scroll of a gesture that moves more across than down, once per gesture, saying each
    /// step AppKit's tracking reports. `tracking` is whether this gesture was taken.
    fn track_swipe(event: &NSEvent, tracking: &Cell<bool>) {
        if event.r#type() != NSEventType::ScrollWheel || !event.hasPreciseScrollingDeltas() {
            return;
        }
        let phase = event.phase();
        if phase == NSEventPhase::None {
            return;
        }
        if phase.intersects(NSEventPhase::Ended | NSEventPhase::Cancelled) {
            tracking.set(false);
            return;
        }
        if tracking.get() || event.scrollingDeltaX().abs() <= event.scrollingDeltaY().abs() {
            return;
        }
        tracking.set(true);
        let handler = RcBlock::new(
            |amount: f64, phase: NSEventPhase, done: Bool, _stop: NonNull<Bool>| {
                let done = u8::from(done.as_bool());
                say(&format!(
                    "track amount={amount:.3} phase={} complete={done}",
                    phase.0
                ));
            },
        );
        event.trackSwipeEventWithOptions_dampenAmountThresholdMin_max_usingHandler(
            NSEventSwipeTrackingOptions::empty(),
            -1.0,
            1.0,
            &handler,
        );
    }

    pub fn run() -> ExitCode {
        let Some(mtm) = MainThreadMarker::new() else {
            return ExitCode::FAILURE;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();

        // SAFETY: the designated `NSWindow` initialiser; every argument is a plain value and the
        // window is created and used on the main thread (AppKit, `NSWindow`).
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect {
                    origin: ORIGIN,
                    size: SIZE,
                },
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: `false` stops AppKit freeing a window this code still holds (AppKit,
        // `NSWindow.isReleasedWhenClosed`).
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        // Nothing to see, and nothing in the way: clear and shadowless, so the window server
        // passes every real click through it, at the desktop's level, under every window.
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(false);
        let desktop = CGWindowLevelForKey(CGWindowLevelKey::DesktopWindowLevelKey);
        window.setLevel(desktop as isize);
        window.orderFrontRegardless();
        // AppKit's frame is from the main display's bottom left, y up; global display points
        // are from its top left, y down.
        let frame = window.frame();
        let main = CGDisplayBounds(CGMainDisplayID());
        let top = main.size.height - (frame.origin.y + frame.size.height);
        say(&format!(
            "ready pid={} x={} y={top} w={} h={} swipe_tracking={}",
            std::process::id(),
            frame.origin.x,
            frame.size.width,
            frame.size.height,
            u8::from(NSEvent::isSwipeTrackingFromScrollEventsEnabled()),
        ));

        // The parent closing stdin, or going, ends this process.
        let gone = Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let gone = Arc::clone(&gone);
            move || {
                let _read = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
                gone.store(true, Ordering::Relaxed);
            }
        });
        let tracking = Cell::new(false);
        while !gone.load(Ordering::Relaxed) {
            let until = NSDate::dateWithTimeIntervalSinceNow(0.05);
            // SAFETY: the main thread's own queue, read in the default mode as `-[NSApplication
            // run]` does (AppKit, `nextEventMatchingMask:untilDate:inMode:dequeue:`).
            let event = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&until),
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            if let Some(event) = event {
                say(&format!("app {}", describe(&event)));
                track_swipe(&event, &tracking);
                // A press is read, not acted on: nothing here raises the app.
                if !is_press(event.r#type()) {
                    app.sendEvent(&event);
                }
            }
        }
        ExitCode::SUCCESS
    }
}
