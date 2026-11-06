//! An application that shows a cursor of known colours, for `tests/cursor.rs`, which is its
//! parent: the worker reads the cursor on screen as the window server hands it back, and a
//! byte-for-byte check of an arrow (black and white) cannot tell BGRA from RGBA.
//!
//! The cursor is a 16-point square in four quadrants, drawn by AppKit at whatever scale the
//! window server asks for: red at the top left, green at the top right, blue at the bottom
//! left and white at half cover at the bottom right, with the hotspot at (3, 5) points. The app
//! is an accessory that never becomes active, so it asks the window server to take its cursor
//! from the background (`SetsCursorInBackground`), sets it, and sets it again every 50 ms in
//! case something else set another. It says `ready pid=<pid>` once the cursor is set, and when
//! its stdin closes it sets the arrow and leaves.

#[cfg(target_os = "macos")]
fn main() -> std::process::ExitCode {
    macos::run()
}

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::{c_int, c_void};
    use std::io::Write as _;
    use std::process::ExitCode;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2::{AnyThread as _, MainThreadMarker};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSColor, NSCursor, NSImage, NSRectFill,
    };
    use objc2_core_foundation::{CFBoolean, CFString, CFType};
    use objc2_foundation::{NSDate, NSPoint, NSRect, NSRunLoop, NSSize};

    /// `CGSConnectionID CGSMainConnectionID(void)`, as `onicode_desktop_capture::cursor` declares
    /// it.
    type MainConnection = unsafe extern "C-unwind" fn() -> c_int;
    /// `CGError CGSSetConnectionProperty(CGSConnectionID, CGSConnectionID, CFStringRef,
    /// CFTypeRef)`, a SkyLight export re-exported by CoreGraphics, as `yabai` declares it.
    type SetProperty = unsafe extern "C-unwind" fn(c_int, c_int, &CFString, &CFType) -> i32;

    fn say(line: &str) {
        let mut out = std::io::stdout().lock();
        let _written = writeln!(out, "{line}").and_then(|()| out.flush());
    }

    fn symbol(name: &std::ffi::CStr) -> Option<*mut c_void> {
        // SAFETY: `dlsym(3)` with `RTLD_DEFAULT` searches every loaded image for a
        // NUL-terminated name.
        let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
        (!found.is_null()).then_some(found)
    }

    /// Ask the window server to take this process's cursor while it is in the background.
    fn set_cursor_in_background() -> Option<()> {
        let (main, set) = (
            symbol(c"CGSMainConnectionID")?,
            symbol(c"CGSSetConnectionProperty")?,
        );
        // SAFETY: CoreGraphics exports `CGSMainConnectionID` with the signature
        // `MainConnection` names.
        let main = unsafe { std::mem::transmute::<*mut c_void, MainConnection>(main) };
        // SAFETY: as above, `CGSSetConnectionProperty` with `SetProperty`'s.
        let set = unsafe { std::mem::transmute::<*mut c_void, SetProperty>(set) };
        // SAFETY: a plain call with no arguments.
        let connection = unsafe { main() };
        // A connection property key of the window server's; no public header names it.
        let key = CFString::from_static_str("SetsCursorInBackground");
        // SAFETY: `kCFBooleanTrue` is an immutable CoreFoundation constant (`CFNumber.h`).
        let yes: &CFBoolean = unsafe { objc2_core_foundation::kCFBooleanTrue }?;
        // SAFETY: this process's own connection twice, a live key and a live value, as the
        // call takes them; it retains what it keeps.
        let error = unsafe { set(connection, connection, &key, yes) };
        (error == 0).then_some(())
    }

    fn srgb(red: f64, green: f64, blue: f64, alpha: f64) -> Retained<NSColor> {
        NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, alpha)
    }

    /// The four-colour square, drawn by AppKit at the scale it is shown at.
    fn picture() -> Retained<NSImage> {
        let draw = RcBlock::new(|rect: NSRect| -> Bool {
            let half = NSSize {
                width: rect.size.width / 2.0,
                height: rect.size.height / 2.0,
            };
            // Flipped: y grows downwards, so the first row is the top one.
            let quadrant = |x: f64, y: f64| NSRect {
                origin: NSPoint {
                    x: x * half.width,
                    y: y * half.height,
                },
                size: half,
            };
            for (colour, at) in [
                (srgb(1.0, 0.0, 0.0, 1.0), quadrant(0.0, 0.0)),
                (srgb(0.0, 1.0, 0.0, 1.0), quadrant(1.0, 0.0)),
                (srgb(0.0, 0.0, 1.0, 1.0), quadrant(0.0, 1.0)),
                (srgb(1.0, 1.0, 1.0, 0.5), quadrant(1.0, 1.0)),
            ] {
                colour.setFill();
                NSRectFill(at);
            }
            Bool::YES
        });
        NSImage::imageWithSize_flipped_drawingHandler(
            NSSize {
                width: 16.0,
                height: 16.0,
            },
            true,
            &draw,
        )
    }

    pub fn run() -> ExitCode {
        let Some(mtm) = MainThreadMarker::new() else {
            return ExitCode::FAILURE;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        app.finishLaunching();
        if set_cursor_in_background().is_none() {
            say("no SetsCursorInBackground");
            return ExitCode::FAILURE;
        }
        let cursor = NSCursor::initWithImage_hotSpot(
            NSCursor::alloc(),
            &picture(),
            NSPoint { x: 3.0, y: 5.0 },
        );
        cursor.set();
        say(&format!("ready pid={}", std::process::id()));
        let done = Arc::new(AtomicBool::new(false));
        std::thread::spawn({
            let done = Arc::clone(&done);
            move || {
                let _read = std::io::copy(&mut std::io::stdin(), &mut std::io::sink());
                done.store(true, Ordering::Relaxed);
            }
        });
        while !done.load(Ordering::Relaxed) {
            NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(0.05));
            cursor.set();
        }
        NSCursor::arrowCursor().set();
        ExitCode::SUCCESS
    }
}
