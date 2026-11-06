//! Whether a stream's source is drawing, still, or behind the lock screen
//! ([`ScreenEvent::Source`](onicode_desktop_proto::screen::ScreenEvent::Source)).

use std::time::{Duration, Instant};

use onicode_desktop_capture::Console;
use onicode_desktop_media::wire::SourceState;

/// How long without a coded frame before a drawing source reads as idle.
pub const SOURCE_QUIET_AFTER: Duration = Duration::from_secs(2);

/// How long a stream that never coded a frame waits before it says idle.
pub const SOURCE_IDLE_AFTER: Duration = Duration::from_millis(400);

/// What a stream last told about its source.
#[derive(Clone, Copy, Debug)]
pub struct SourceTracker {
    reported: Option<SourceState>,
    console: Console,
    seen: u64,
    last_frame: Option<Instant>,
    opened_at: Instant,
}

impl SourceTracker {
    /// A stream opened at `now`.
    #[must_use]
    pub const fn new(now: Instant) -> Self {
        Self {
            reported: None,
            seen: 0,
            last_frame: None,
            opened_at: now,
            console: Console::Shown,
        }
    }

    /// The console's state, as the platform reads it.
    pub const fn set_console(&mut self, console: Console) {
        self.console = console;
    }

    /// The stream has coded `encoded` frames by `now`: the state to tell, when it changed.
    pub fn poll(&mut self, encoded: u64, now: Instant) -> Option<SourceState> {
        if encoded > self.seen {
            self.seen = encoded;
            self.last_frame = Some(now);
        }
        let state = match self.last_frame {
            _ if self.console == Console::Locked => SourceState::Locked,
            _ if self.console == Console::Away => SourceState::Away,
            None if now.saturating_duration_since(self.opened_at) < SOURCE_IDLE_AFTER => {
                return None;
            }
            None => SourceState::Idle,
            Some(at) if now.saturating_duration_since(at) >= SOURCE_QUIET_AFTER => {
                SourceState::Idle
            }
            Some(_drew) => SourceState::Live,
        };
        (self.reported != Some(state)).then(|| {
            self.reported = Some(state);
            state
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_goes_live_idle_and_locked_once_each() {
        let t0 = Instant::now();
        let mut source = SourceTracker::new(t0);
        assert_eq!(source.poll(0, t0), None, "too early to say");
        assert_eq!(
            source.poll(0, t0 + SOURCE_IDLE_AFTER),
            Some(SourceState::Idle)
        );
        assert_eq!(
            source.poll(1, t0 + SOURCE_IDLE_AFTER),
            Some(SourceState::Live)
        );
        assert_eq!(source.poll(1, t0 + SOURCE_IDLE_AFTER), None, "told once");
        let quiet = t0 + SOURCE_IDLE_AFTER + SOURCE_QUIET_AFTER;
        assert_eq!(source.poll(1, quiet), Some(SourceState::Idle));
        source.set_console(Console::Locked);
        assert_eq!(source.poll(2, quiet), Some(SourceState::Locked));
        source.set_console(Console::Shown);
        assert_eq!(source.poll(2, quiet), Some(SourceState::Live));
    }
}
