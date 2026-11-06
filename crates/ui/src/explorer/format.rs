//! The explorer's labels, computed once when a listing or update arrives, never in render.

use chrono::{DateTime, Local, TimeZone as _};
use onicode_proto::files::PathKind;

/// Bytes as Finder writes them: decimal units, one decimal under 10.
pub(crate) fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut whole = bytes;
    let mut unit = 0;
    // Tenths of the current unit, kept in integers so no cast loses precision.
    let mut tenths = bytes / 100;
    whole /= 1000;
    while whole >= 1000 && unit + 1 < UNITS.len() {
        tenths = whole / 100;
        whole /= 1000;
        unit += 1;
    }
    if whole < 10 {
        format!("{}.{} {}", whole, tenths % 10, UNITS[unit])
    } else {
        format!("{whole} {}", UNITS[unit])
    }
}

/// A modification time: "Today at 14:05", "Yesterday at 09:12", else "6 Nov 2026 at 14:05".
pub(crate) fn date_label(modified_ms: i64, now: DateTime<Local>) -> String {
    let Some(at) = Local.timestamp_millis_opt(modified_ms).single() else {
        return String::new();
    };
    let days = now
        .date_naive()
        .signed_duration_since(at.date_naive())
        .num_days();
    match days {
        0 => format!("Today at {}", at.format("%H:%M")),
        1 => format!("Yesterday at {}", at.format("%H:%M")),
        _ => at.format("%-d %b %Y at %H:%M").to_string(),
    }
}

/// What Finder's Kind column would say, near enough.
pub(crate) fn kind_label(name: &str, kind: PathKind) -> String {
    match kind {
        PathKind::Dir => "Folder".into(),
        PathKind::Symlink => "Alias".into(),
        PathKind::Package => match extension(name) {
            Some(ext) if ext.eq_ignore_ascii_case("app") => "Application".into(),
            _ => "Package".into(),
        },
        PathKind::File => match extension(name) {
            Some(ext) => format!("{} file", ext.to_uppercase()),
            None => "Document".into(),
        },
    }
}

fn extension(name: &str) -> Option<&str> {
    name.rsplit_once('.')
        .filter(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
        .map(|(_, ext)| ext)
}

/// A transfer rate: "12 MB/s".
pub(crate) fn speed_label(bytes_per_sec: u64) -> String {
    if bytes_per_sec < 1000 {
        return format!("{bytes_per_sec} B/s");
    }
    let size = size_label(bytes_per_sec);
    format!("{size}/s")
}

/// Time left at the current rate: "about 3 min", "12 s"; `None` while unknown.
pub(crate) fn eta_label(remaining: u64, bytes_per_sec: u64) -> Option<String> {
    if bytes_per_sec == 0 || remaining == 0 {
        return None;
    }
    let seconds = remaining.div_ceil(bytes_per_sec);
    Some(match seconds {
        0..60 => format!("{seconds} s left"),
        60..3600 => format!("about {} min left", seconds.div_ceil(60)),
        _ => format!("about {} h left", seconds / 3600),
    })
}

/// The last component of a path, or the path itself at the root.
pub(crate) fn file_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((_, name)) if !name.is_empty() => name,
        _ => path,
    }
}

/// `child` inside `dir`, joined with one slash.
pub(crate) fn join(dir: &str, child: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{child}")
    } else {
        format!("{dir}/{child}")
    }
}

/// The folder holding `path`; `None` at the root.
pub(crate) fn parent(path: &str) -> Option<&str> {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some(("", _)) if trimmed.len() > 1 => Some("/"),
        Some((parent, _)) if !parent.is_empty() => Some(parent),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Local, TimeZone as _};
    use onicode_proto::files::PathKind;

    use super::{date_label, eta_label, file_name, join, kind_label, parent, size_label};

    #[test]
    fn sizes_read_as_finder_writes_them() {
        assert_eq!(size_label(0), "0 bytes");
        assert_eq!(size_label(999), "999 bytes");
        assert_eq!(size_label(1_000), "1.0 KB");
        assert_eq!(size_label(1_234), "1.2 KB");
        assert_eq!(size_label(12_345), "12 KB");
        assert_eq!(size_label(4_500_000), "4.5 MB");
        assert_eq!(size_label(3_200_000_000), "3.2 GB");
        assert_eq!(size_label(u64::MAX), "18446 PB");
    }

    #[test]
    fn dates_name_today_and_yesterday() {
        let now = Local.with_ymd_and_hms(2026, 11, 6, 15, 0, 0).unwrap();
        let today = Local.with_ymd_and_hms(2026, 11, 6, 9, 5, 0).unwrap();
        let yesterday = Local.with_ymd_and_hms(2026, 11, 5, 23, 59, 0).unwrap();
        let older = Local.with_ymd_and_hms(2025, 1, 2, 8, 0, 0).unwrap();
        assert_eq!(date_label(today.timestamp_millis(), now), "Today at 09:05");
        assert_eq!(
            date_label(yesterday.timestamp_millis(), now),
            "Yesterday at 23:59"
        );
        assert_eq!(
            date_label(older.timestamp_millis(), now),
            "2 Jan 2025 at 08:00"
        );
    }

    #[test]
    fn kinds_follow_the_extension() {
        assert_eq!(kind_label("a.pdf", PathKind::File), "PDF file");
        assert_eq!(kind_label(".zshrc", PathKind::File), "Document");
        assert_eq!(kind_label("Mail.app", PathKind::Package), "Application");
        assert_eq!(kind_label("src", PathKind::Dir), "Folder");
    }

    #[test]
    fn eta_rounds_up_and_needs_a_rate() {
        assert_eq!(eta_label(10, 0), None);
        assert_eq!(eta_label(0, 10), None);
        assert_eq!(eta_label(10, 3).as_deref(), Some("4 s left"));
        assert_eq!(
            eta_label(61 * 1000, 1000).as_deref(),
            Some("about 2 min left")
        );
    }

    #[test]
    fn paths_split_and_join() {
        assert_eq!(file_name("/Users/me/a.txt"), "a.txt");
        assert_eq!(file_name("/Users/me/dir/"), "dir");
        assert_eq!(file_name("/"), "/");
        assert_eq!(join("/", "Users"), "/Users");
        assert_eq!(join("/Users", "me"), "/Users/me");
        assert_eq!(parent("/Users/me"), Some("/Users"));
        assert_eq!(parent("/Users"), Some("/"));
        assert_eq!(parent("/"), None);
    }
}
