//! Automation: whether this app may send Apple Events to System Events and Finder, which agents'
//! `osascript` calls need.
//!
//! `AEDeterminePermissionToAutomateTarget` answers only for a running target and must not run on
//! the main thread. Without asking it never prompts (`errAEEventWouldRequireUserConsent` means
//! "never asked"); asking raises the alert and waits for the answer. System Events is started
//! hidden first when it is not running; Finder always runs.

use std::ffi::c_void;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use objc2_app_kit::NSRunningApplication;
use objc2_foundation::NSString;

use crate::Status;

/// The targets agents script.
const TARGETS: [&str; 2] = ["com.apple.systemevents", "com.apple.finder"];
/// How long a started System Events has to show up.
const START_WAIT: Duration = Duration::from_secs(3);

const fn four_cc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

/// `typeApplicationBundleID`.
const TYPE_BUNDLE_ID: u32 = four_cc(b"bund"); // spellchecker:disable-line (typeApplicationBundleID)
/// `typeWildCard`: any event class, any event id.
const TYPE_WILD_CARD: u32 = four_cc(b"****");
/// `errAEEventNotPermitted`.
const NOT_PERMITTED: i32 = -1743;
/// `errAEEventWouldRequireUserConsent`.
const WOULD_ASK: i32 = -1744;

/// `AEDesc`.
#[repr(C)]
struct AeDesc {
    descriptor_type: u32,
    data_handle: *mut c_void,
}

// Apple Events, in CoreServices' AE framework.
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn AECreateDesc(type_code: u32, data: *const c_void, size: isize, result: *mut AeDesc) -> i16;
    fn AEDisposeDesc(desc: *mut AeDesc) -> i16;
    fn AEDeterminePermissionToAutomateTarget(
        target: *const AeDesc,
        event_class: u32,
        event_id: u32,
        ask_user_if_needed: u8,
    ) -> i32;
}

fn running(bundle_id: &str) -> bool {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id))
        .firstObject()
        .is_some()
}

/// Starts System Events hidden and in the background, and waits a moment for it to run.
fn start_system_events() {
    let started = Command::new("/usr/bin/open")
        .args(["-gj", "-b", "com.apple.systemevents"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if started.is_err() {
        return;
    }
    let until = Instant::now() + START_WAIT;
    while !running("com.apple.systemevents") && Instant::now() < until {
        thread::sleep(Duration::from_millis(100));
    }
}

/// One target's answer.
fn determine(bundle_id: &str, ask: bool) -> Status {
    let mut desc = AeDesc {
        descriptor_type: 0,
        data_handle: std::ptr::null_mut(),
    };
    let Ok(size) = isize::try_from(bundle_id.len()) else {
        return Status::Unknown;
    };
    // SAFETY: the bytes outlive the call, which copies them into `desc`.
    let made = unsafe {
        AECreateDesc(
            TYPE_BUNDLE_ID,
            bundle_id.as_ptr().cast(),
            size,
            &raw mut desc,
        )
    };
    if made != 0 {
        return Status::Unknown;
    }
    // SAFETY: `desc` is the address descriptor just made; this thread is not the main thread
    // (callers' contract).
    let answer = unsafe {
        AEDeterminePermissionToAutomateTarget(
            &raw const desc,
            TYPE_WILD_CARD,
            TYPE_WILD_CARD,
            u8::from(ask),
        )
    };
    // SAFETY: `desc` was made by `AECreateDesc` and is disposed once.
    let _disposed = unsafe { AEDisposeDesc(&raw mut desc) };
    match answer {
        0 => Status::Granted,
        NOT_PERMITTED => Status::Denied,
        WOULD_ASK => Status::NotAsked,
        // `procNotFound` and the rest: the target is not running, or something else failed.
        _ => Status::Unknown,
    }
}

/// Both targets, summed: on when both are, refused when one is, "never asked" when one never
/// was. With `ask`, each target not yet decided raises its alert in turn.
pub(super) fn status(ask: bool) -> Status {
    if !running("com.apple.systemevents") {
        start_system_events();
    }
    let answers: Vec<Status> = TARGETS
        .iter()
        .map(|target| determine(target, ask))
        .collect();
    if answers.iter().all(|&answer| answer == Status::Granted) {
        Status::Granted
    } else if answers.contains(&Status::Denied) {
        Status::Denied
    } else if answers.contains(&Status::NotAsked) {
        Status::NotAsked
    } else {
        Status::Unknown
    }
}
