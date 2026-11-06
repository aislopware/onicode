//! Rebuilding a view when a label drawn from the clock next changes.
//!
//! Retained mode draws a view from the last frame until something it read
//! changes, and the time is not something it can see read. A view showing
//! "2m", "1:23" or "Opened 3 hours ago" therefore asks, as it renders, to be
//! notified at the moment that text turns over, and no sooner.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use gpui::{App, Task, WeakEntity};

/// The wall clock for time labels. Tests read it through the executor's
/// clock, so `advance_clock` moves the labels too.
pub fn utc_now(cx: &App) -> DateTime<Utc> {
    #[cfg(test)]
    {
        use chrono::TimeDelta;
        thread_local! {
            static ANCHOR: std::cell::OnceCell<(DateTime<Utc>, Instant)> =
                const { std::cell::OnceCell::new() };
        }
        let now = cx.background_executor().now();
        let (utc, at) = ANCHOR.with(|anchor| *anchor.get_or_init(|| (Utc::now(), now)));
        let since = |later: Instant, earlier: Instant| {
            TimeDelta::from_std(later - earlier).unwrap_or_default()
        };
        if now >= at {
            utc + since(now, at)
        } else {
            utc - since(at, now)
        }
    }
    #[cfg(not(test))]
    {
        let _ = cx;
        Utc::now()
    }
}

/// How long until `onicode_proto::view::format_time_ago(then, now)` reads
/// differently: minutes turn over each minute, hours each hour, and days,
/// weeks, months and years at most once a day.
pub fn ago_turns_in(then: DateTime<Utc>, now: DateTime<Utc>) -> Duration {
    let age = now.signed_duration_since(then).num_milliseconds().max(0) as u64;
    let step: u64 = match age {
        ..3_600_000 => 60_000,
        3_600_000..86_400_000 => 3_600_000,
        _ => 86_400_000,
    };
    Duration::from_millis(step - age % step)
}

/// One pending wake for a view's time labels: the earliest asked for wins,
/// and a later one is dropped while it is pending.
#[derive(Default)]
pub struct LabelTick {
    due: Option<Instant>,
    _task: Option<Task<()>>,
}

impl LabelTick {
    /// Notify `view` after `after`, unless a wake no later than that is
    /// already pending.
    pub fn schedule<V: 'static>(&mut self, view: WeakEntity<V>, after: Duration, cx: &mut App) {
        let now = cx.background_executor().now();
        let due = now + after;
        if self
            .due
            .is_some_and(|pending| pending > now && pending <= due)
        {
            return;
        }
        self.due = Some(due);
        self._task = Some(cx.spawn(async move |cx| {
            cx.background_executor().timer(after).await;
            view.update(cx, |_, cx| cx.notify()).ok();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_labels_turn_over_at_their_unit() {
        let now = Utc::now();
        let at = |secs: i64| now - chrono::TimeDelta::seconds(secs);
        assert_eq!(ago_turns_in(at(0), now), Duration::from_secs(60));
        assert_eq!(ago_turns_in(at(45), now), Duration::from_secs(15));
        assert_eq!(ago_turns_in(at(61), now), Duration::from_secs(59));
        assert_eq!(ago_turns_in(at(3_600), now), Duration::from_secs(3_600));
        assert_eq!(
            ago_turns_in(at(86_400 + 10), now),
            Duration::from_secs(86_390)
        );
    }
}
