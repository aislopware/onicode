//! Whether another live process is writing a Claude session.
//!
//! Every running `claude` registers itself in `<config>/sessions/<pid>.json`
//! (`{pid, sessionId, kind, procStart, startedAt, updatedAt, …}`). Resuming
//! a session that a terminal `claude` or another onicode server still runs
//! makes two writers of one transcript, each blind to the other's turns.
//! Only `pid`, `sessionId`, `kind` and `updatedAt` are read: the registry
//! holds pids and paths, never credentials.

use std::path::Path;

/// A process holding the session now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LiveWriter {
    pub pid: u32,
    /// `interactive`, `print`, `sdk`, …, when the entry says.
    pub kind: Option<String>,
}

impl LiveWriter {
    /// What the person is told when the resume is refused.
    pub(crate) fn refusal(&self) -> String {
        let what = match self.kind.as_deref() {
            Some("interactive") => "a Claude Code terminal",
            Some(_) | None => "another Claude Code process",
        };
        format!(
            "This chat's Claude session is open in {what} (pid {}). Two processes writing one \
             session lose each other's turns, so it was not resumed here. Close it there, or \
             branch the chat to continue from a copy.",
            self.pid
        )
    }
}

/// An entry not touched for this long is stale: a crashed CLI leaves its
/// file behind, and its pid may since belong to anything.
const STALE_AFTER_MS: u64 = 24 * 60 * 60 * 1000;

/// The live process other than any of `ours` writing `session_id`, if one.
pub(crate) fn live_writer(
    config_root: &Path,
    session_id: &str,
    ours: &[u32],
) -> Option<LiveWriter> {
    let dir = std::fs::read_dir(config_root.join("sessions")).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    dir.flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
        .filter_map(|entry| {
            // Small files; a huge one is not a registry entry.
            let meta = entry.metadata().ok()?;
            if meta.len() > 64 * 1024 {
                return None;
            }
            let text = std::fs::read_to_string(entry.path()).ok()?;
            parse_entry(&text)
        })
        .find(|entry| {
            entry.session_id == session_id
                && !ours.contains(&entry.pid)
                && entry
                    .updated_at
                    .is_none_or(|at| now.saturating_sub(at) < STALE_AFTER_MS)
                && alive(entry.pid)
        })
        .map(|entry| LiveWriter {
            pid: entry.pid,
            kind: entry.kind,
        })
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    pid: u32,
    session_id: String,
    kind: Option<String>,
    updated_at: Option<u64>,
}

fn parse_entry(text: &str) -> Option<Entry> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let pid = value.get("pid")?.as_u64().filter(|p| *p > 1)? as u32;
    let session_id = value.get("sessionId")?.as_str()?.to_owned();
    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    // Epoch millis, or an ISO timestamp from a CLI that writes one.
    let updated_at = value.get("updatedAt").and_then(|at| {
        at.as_u64().or_else(|| {
            chrono::DateTime::parse_from_rfc3339(at.as_str()?)
                .ok()
                .map(|t| t.timestamp_millis().max(0) as u64)
        })
    });
    Some(Entry {
        pid,
        session_id,
        kind,
        updated_at,
    })
}

fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks the process exists; EPERM means it does.
    let exists = unsafe { libc::kill(pid, 0) } == 0;
    exists || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, pid: u32, body: serde_json::Value) {
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(dir.join(format!("sessions/{pid}.json")), body.to_string()).unwrap();
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    #[test]
    fn a_live_foreign_writer_is_found_and_everything_else_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let me = std::process::id();
        // This test process stands in for the other writer: it is alive.
        write(
            dir.path(),
            me,
            serde_json::json!({"pid": me, "sessionId": "s-live", "kind": "interactive",
                "updatedAt": now_ms(), "messagingSocketPath": "/tmp/x"}),
        );
        let found = live_writer(dir.path(), "s-live", &[]).expect("live writer");
        assert_eq!(found.pid, me);
        assert!(found.refusal().contains("terminal"), "{}", found.refusal());
        // Another session, or our own process, is no conflict.
        assert_eq!(live_writer(dir.path(), "s-other", &[]), None);
        assert_eq!(live_writer(dir.path(), "s-live", &[me]), None);
        // A stale entry is a crashed CLI's leftover.
        write(
            dir.path(),
            me,
            serde_json::json!({"pid": me, "sessionId": "s-live", "updatedAt": 1_000}),
        );
        assert_eq!(live_writer(dir.path(), "s-live", &[]), None);
        // A dead pid holds nothing.
        let mut dead = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait().unwrap();
        write(
            dir.path(),
            dead_pid,
            serde_json::json!({"pid": dead_pid, "sessionId": "s-dead"}),
        );
        assert_eq!(live_writer(dir.path(), "s-dead", &[]), None);
        // No registry at all: nothing to refuse.
        assert_eq!(live_writer(&dir.path().join("none"), "s-live", &[]), None);
    }

    #[test]
    fn entries_parse_leniently() {
        assert_eq!(parse_entry("not json"), None);
        assert_eq!(parse_entry(r#"{"pid":1,"sessionId":"s"}"#), None);
        let entry = parse_entry(r#"{"pid":42,"sessionId":"s","updatedAt":"2026-11-06T10:00:00Z"}"#)
            .unwrap();
        assert_eq!(entry.pid, 42);
        assert!(entry.updated_at.is_some());
    }
}
