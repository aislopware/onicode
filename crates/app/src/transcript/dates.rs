//! AgentTranscript's opensNewStretch and message times, in the reader's local time.
use chrono::{DateTime, Local, NaiveDate};

use super::*;

pub(super) fn full_stamp(at: u64) -> String {
    i64::try_from(at)
        .ok()
        .and_then(DateTime::from_timestamp_millis)
        .map(|date| date.with_timezone(&Local).format("%c").to_string())
        .unwrap_or_default()
}

fn local_day(at: u64) -> Option<NaiveDate> {
    DateTime::from_timestamp_millis(i64::try_from(at).ok()?)
        .map(|date| date.with_timezone(&Local).date_naive())
}

fn opens_stretch(at: Option<u64>, previous: Option<u64>) -> bool {
    let Some(at) = at.filter(|at| local_day(*at).is_some()) else {
        return false;
    };
    previous.is_none_or(|previous| local_day(at) != local_day(previous))
}

/// When the message before `start` was sent: the previous prompt, or the
/// conversation's first row.
fn previous_send(source: &[Row], start: usize) -> Option<u64> {
    source[..start]
        .iter()
        .enumerate()
        .rfind(|(ix, row)| row.first && (row.is_prompt() || *ix == 0))
        .and_then(|(_, row)| row.sent_at)
}

/// Whether the message at `start` opens a new day after the one before it.
pub(super) fn opens_stretch_at(source: &[Row], start: usize) -> bool {
    opens_stretch(source[start].sent_at, previous_send(source, start))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at(day: u32, hour: u32, minute: u32) -> u64 {
        Local
            .with_ymd_and_hms(2026, 10, day, hour, minute, 0)
            .unwrap()
            .timestamp_millis() as u64
    }

    #[test]
    fn stretches_match_react_and_cross_midnight_in_local_time() {
        assert!(!opens_stretch(None, None));
        assert!(opens_stretch(Some(at(5, 9, 0)), None));
        assert!(!opens_stretch(Some(at(5, 10, 0)), Some(at(5, 9, 0))));
        assert!(
            !opens_stretch(Some(at(5, 22, 30)), Some(at(5, 7, 54))),
            "a long pause within one day is not a new day"
        );
        assert!(opens_stretch(Some(at(6, 0, 1)), Some(at(5, 23, 59))));
    }
}
