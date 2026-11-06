//! A server Mac stays awake while an agent runs on it (`docs/onicode/decisions/keep-awake.md`).
//!
//! While any of this engine's chats has a turn open (working, or waiting on its owner) or one of
//! its terminals runs a program in the foreground, the engine holds one
//! `PreventUserIdleSystemSleep` power assertion. It never keeps the display on, and it lets go
//! [`LINGER`] after the last one ends. The decision is the pure [`KeepAwake`]; the assertion sits
//! behind [`PowerAssertion`], so no test touches the machine's power state. The owner turns it off
//! with `keepAwakeWhileAgentsRun` in the app's `ui-settings.json` (on by default).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use onicode_proto::Session;
use onicode_proto::view::{Indicator, native_indicator};

/// The name `pmset -g assertions` shows.
pub const REASON: &str = "Onicode: an agent or a terminal command is running";

/// How long the assertion outlives the last turn or command, so back-to-back turns hold one.
pub const LINGER: Duration = Duration::from_secs(10);

/// How often terminals and the setting are looked at again.
const POLL: Duration = Duration::from_secs(15);

/// The settings key.
const SETTING: &str = "keepAwakeWhileAgentsRun";

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Engines assembled from now on hold the assertion while they work. Only a real engine calls
/// this; tests and fixtures never do.
pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// What wants the Mac awake now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Load {
    /// The owner's setting.
    pub allowed: bool,
    /// Chats on this engine with a turn open.
    pub turns: usize,
    /// Terminals on this engine running a program in the foreground.
    pub commands: usize,
}

impl Load {
    fn busy(self) -> bool {
        self.turns + self.commands > 0
    }
}

/// What the assertion must do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Take,
    Release,
}

/// The engine's one assertion, counted by what holds it.
#[derive(Debug, Default)]
pub struct KeepAwake {
    held: bool,
    /// When the load last went to nothing while held.
    idle_since: Option<Instant>,
}

impl KeepAwake {
    pub fn held(&self) -> bool {
        self.held
    }

    /// When a held assertion is due to go, if nothing else comes.
    pub fn deadline(&self) -> Option<Instant> {
        self.idle_since.map(|since| since + LINGER)
    }

    /// Take in the load at `now`; returns what the assertion must do, if anything.
    pub fn step(&mut self, load: Load, now: Instant) -> Option<Power> {
        if !load.allowed {
            self.idle_since = None;
            return std::mem::take(&mut self.held).then_some(Power::Release);
        }
        if load.busy() {
            self.idle_since = None;
            return (!std::mem::replace(&mut self.held, true)).then_some(Power::Take);
        }
        if !self.held {
            return None;
        }
        let since = *self.idle_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= LINGER {
            self.held = false;
            self.idle_since = None;
            return Some(Power::Release);
        }
        None
    }
}

/// The power assertion itself.
pub trait PowerAssertion: Send {
    /// Hold it; false when the system refused.
    fn take(&mut self, reason: &str) -> bool;
    fn release(&mut self);
}

/// Chats on `device` with a turn open, as their live rows say.
pub fn open_turns(sessions: &[Session], device: &str, now: chrono::DateTime<chrono::Utc>) -> usize {
    sessions
        .iter()
        .filter(|session| session.device_id == device)
        .filter(|session| {
            matches!(
                native_indicator(Some(session), now),
                Indicator::Working | Indicator::AwaitingInput
            )
        })
        .count()
}

/// The owner's setting, on unless turned off.
fn allowed(data_dir: &Path) -> bool {
    std::fs::read(data_dir.join("ui-settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.get(SETTING).and_then(serde_json::Value::as_bool))
        .unwrap_or(true)
}

/// Terminals running a program in the foreground.
fn busy_terminals(terminals: &crate::Terminals) -> usize {
    terminals
        .chats()
        .keys()
        .filter(|id| terminals.status(id).is_ok_and(|status| status.busy))
        .count()
}

/// Watch this engine's turns and terminals and hold the assertion while they work, until the
/// runtime shuts down. Dropping the task releases it.
pub(crate) fn spawn(core: &crate::EngineCore, data_dir: PathBuf) -> tokio::task::JoinHandle<()> {
    let mut sessions = core.sessions.watch_sessions();
    let terminals = core.terminals.clone();
    let device = core.device_id.clone();
    tokio::spawn(async move {
        let mut power = system();
        let mut awake = KeepAwake::default();
        let mut poll = tokio::time::interval(POLL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let load = Load {
                allowed: allowed(&data_dir),
                turns: open_turns(&sessions.borrow(), &device, chrono::Utc::now()),
                commands: busy_terminals(&terminals),
            };
            match awake.step(load, Instant::now()) {
                Some(Power::Take) => {
                    if !power.take(REASON) {
                        tracing::warn!("this Mac refused to stay awake");
                    }
                }
                Some(Power::Release) => power.release(),
                None => {}
            }
            let linger = awake.deadline().map(tokio::time::Instant::from_std);
            tokio::select! {
                changed = sessions.changed() => if changed.is_err() { break },
                _ = poll.tick() => {}
                () = async {
                    match linger {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {}
            }
        }
    })
}

/// This machine's assertion.
fn system() -> Box<dyn PowerAssertion> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::Assertion::default())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(Unsupported)
    }
}

#[cfg(not(target_os = "macos"))]
struct Unsupported;

#[cfg(not(target_os = "macos"))]
impl PowerAssertion for Unsupported {
    fn take(&mut self, _reason: &str) -> bool {
        false
    }

    fn release(&mut self) {}
}

#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::c_void;

    /// `kIOPMAssertionTypePreventUserIdleSystemSleep`: idle system sleep only; the display still
    /// sleeps.
    const PREVENT_IDLE_SYSTEM_SLEEP: &str = "PreventUserIdleSystemSleep";
    const ASSERTION_LEVEL_ON: u32 = 255;
    const IO_RETURN_SUCCESS: i32 = 0;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: *const c_void,
            bytes: *const u8,
            length: isize,
            encoding: u32,
            external_representation: u8,
        ) -> *const c_void;
        fn CFRelease(object: *const c_void);
    }

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMAssertionCreateWithName(
            assertion_type: *const c_void,
            level: u32,
            name: *const c_void,
            assertion_id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(assertion_id: u32) -> i32;
    }

    /// A CFString owned until dropped.
    struct CfString(*const c_void);

    impl CfString {
        fn new(text: &str) -> Option<Self> {
            let length = isize::try_from(text.len()).ok()?;
            // SAFETY: the bytes are valid UTF-8 for `length` bytes; a null result is handled.
            let string = unsafe {
                CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), length, UTF8, 0)
            };
            (!string.is_null()).then_some(Self(string))
        }
    }

    impl Drop for CfString {
        fn drop(&mut self) {
            // SAFETY: created by `CFStringCreateWithBytes` (the create rule), released once.
            unsafe { CFRelease(self.0) };
        }
    }

    /// The IOKit assertion; released on drop, and by the system if the process dies.
    #[derive(Default)]
    pub(super) struct Assertion {
        id: Option<u32>,
    }

    impl super::PowerAssertion for Assertion {
        fn take(&mut self, reason: &str) -> bool {
            if self.id.is_some() {
                return true;
            }
            let (Some(kind), Some(name)) = (
                CfString::new(PREVENT_IDLE_SYSTEM_SLEEP),
                CfString::new(reason),
            ) else {
                return false;
            };
            let mut id = 0u32;
            // SAFETY: both strings live across the call; IOKit copies what it keeps.
            let status = unsafe {
                IOPMAssertionCreateWithName(kind.0, ASSERTION_LEVEL_ON, name.0, &raw mut id)
            };
            if status != IO_RETURN_SUCCESS {
                return false;
            }
            tracing::info!(reason, "holding this Mac awake");
            self.id = Some(id);
            true
        }

        fn release(&mut self) {
            if let Some(id) = self.id.take() {
                // SAFETY: `id` came from a successful `IOPMAssertionCreateWithName`.
                unsafe { IOPMAssertionRelease(id) };
                tracing::info!("letting this Mac sleep");
            }
        }
    }

    impl Drop for Assertion {
        fn drop(&mut self) {
            super::PowerAssertion::release(self);
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    /// Records what the assertion was told, and fails on a double take or release.
    #[derive(Default)]
    struct Recorded {
        held: bool,
        log: Vec<Power>,
    }

    impl PowerAssertion for Recorded {
        fn take(&mut self, _reason: &str) -> bool {
            assert!(!self.held, "taken twice");
            self.held = true;
            self.log.push(Power::Take);
            true
        }

        fn release(&mut self) {
            assert!(self.held, "released while not held");
            self.held = false;
            self.log.push(Power::Release);
        }
    }

    fn drive(awake: &mut KeepAwake, power: &mut Recorded, load: Load, now: Instant) {
        match awake.step(load, now) {
            Some(Power::Take) => {
                power.take(REASON);
            }
            Some(Power::Release) => power.release(),
            None => {}
        }
    }

    #[test]
    fn a_turn_holds_the_mac_awake_and_lets_go_after_the_linger() {
        let t0 = Instant::now();
        let mut awake = KeepAwake::default();
        let mut power = Recorded::default();
        let busy = Load {
            allowed: true,
            turns: 1,
            commands: 0,
        };
        let idle = Load {
            allowed: true,
            ..Load::default()
        };
        drive(&mut awake, &mut power, busy, t0);
        assert!(power.held);
        drive(&mut awake, &mut power, idle, t0 + Duration::from_secs(1));
        assert!(power.held, "lingers");
        assert_eq!(awake.deadline(), Some(t0 + Duration::from_secs(1) + LINGER));
        // The next turn inside the linger keeps the one assertion.
        drive(&mut awake, &mut power, busy, t0 + Duration::from_secs(5));
        drive(&mut awake, &mut power, idle, t0 + Duration::from_secs(6));
        drive(
            &mut awake,
            &mut power,
            idle,
            t0 + Duration::from_secs(6) + LINGER,
        );
        assert!(!power.held);
        assert_eq!(power.log, vec![Power::Take, Power::Release]);
    }

    #[test]
    fn turning_it_off_lets_go_at_once() {
        let t0 = Instant::now();
        let mut awake = KeepAwake::default();
        let mut power = Recorded::default();
        let load = Load {
            allowed: true,
            turns: 0,
            commands: 2,
        };
        drive(&mut awake, &mut power, load, t0);
        drive(
            &mut awake,
            &mut power,
            Load {
                allowed: false,
                ..load
            },
            t0,
        );
        assert!(!power.held);
        assert_eq!(awake.deadline(), None);
    }

    #[test]
    fn only_live_turns_on_this_engine_count() {
        let now = chrono::Utc::now();
        let row = |device: &str, status: &str, age_s: i64| -> Session {
            serde_json::from_value(serde_json::json!({
                "chatId": format!("{device}-{status}-{age_s}"),
                "deviceId": device,
                "status": status,
                "updatedAt": now - chrono::TimeDelta::seconds(age_s),
            }))
            .expect("session fixture")
        };
        let rows = [
            row("studio", "working", 1),
            row("studio", "awaitingInput", 1),
            row("studio", "idle", 1),
            row("studio", "working", 600),
            row("laptop", "working", 1),
        ];
        assert_eq!(open_turns(&rows, "studio", now), 2);
    }

    proptest! {
        /// Over any run of loads and times: the assertion is taken and released in turn, held
        /// whenever allowed work runs, never held while not allowed, and never past the linger.
        #[test]
        fn the_assertion_follows_the_work(
            steps in proptest::collection::vec(
                (any::<bool>(), 0usize..3, 0usize..3, 0u64..20_000),
                1..60,
            ),
        ) {
            let t0 = Instant::now();
            let mut now = t0;
            let mut awake = KeepAwake::default();
            let mut power = Recorded::default();
            let mut last_busy: Option<Instant> = None;
            for (allowed, turns, commands, advance_ms) in steps {
                now += Duration::from_millis(advance_ms);
                let load = Load { allowed, turns, commands };
                drive(&mut awake, &mut power, load, now);
                prop_assert_eq!(power.held, awake.held());
                if !allowed {
                    prop_assert!(!power.held);
                    last_busy = None;
                    continue;
                }
                if load.busy() {
                    prop_assert!(power.held);
                    last_busy = Some(now);
                } else if power.held {
                    prop_assert!(last_busy.is_some(), "held with no work seen");
                    let deadline = awake.deadline();
                    prop_assert!(deadline.is_some_and(|at| now < at), "held past the linger");
                }
            }
            for pair in power.log.windows(2) {
                prop_assert_ne!(pair[0], pair[1]);
            }
        }
    }
}
