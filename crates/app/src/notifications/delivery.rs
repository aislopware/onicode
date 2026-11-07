//! Shared, side-effect-free notification delivery policy. The host owns OS
//! permission requests, dispatch, settings persistence, and opening sessions.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    Prompt,
    Granted,
    Denied,
    Unsupported,
}

pub fn should_notify(
    enabled: bool,
    permission: Permission,
    window_focused: bool,
    session_visible: bool,
) -> bool {
    enabled
        && !(window_focused && session_visible)
        && matches!(permission, Permission::Prompt | Permission::Granted)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    Session(i64),
    /// Preserve the due timestamp when clearing: stale clicks must never clear a
    /// newer reminder. The host must clear with Some(due_at) before opening.
    Reminder {
        session_id: i64,
        due_at: u64,
    },
}

impl Target {
    /// Sequence prevents replacement of an earlier banner for this session.
    pub fn tag(self, sequence: u64) -> String {
        match self {
            Self::Session(id) => format!("onicode-session:{id}:{sequence}"),
            Self::Reminder { session_id, due_at } => {
                format!("onicode-reminder:{session_id}:{due_at}")
            }
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        // An agent alert, posted by the app or by the engine while the app was closed.
        if let Some(row) = onicode_protocol::alerts::alert_row(tag) {
            return Some(Self::Session(row));
        }
        if let Some(rest) = tag.strip_prefix("onicode-session:") {
            let (id, sequence) = rest.split_once(':')?;
            sequence.parse::<u64>().ok()?;
            return Some(Self::Session(id.parse().ok()?));
        }
        let (id, due) = tag.strip_prefix("onicode-reminder:")?.split_once(':')?;
        Some(Self::Reminder {
            session_id: id.parse().ok()?,
            due_at: due.parse().ok()?,
        })
    }
}

/// First nonempty paragraph with collapsed whitespace, capped at 240 UTF-16
/// code units like the web UI. Never split a Unicode scalar while truncating.
pub fn clip_body(text: &str) -> String {
    let mut paragraph = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            if !paragraph.is_empty() {
                break;
            }
        } else {
            paragraph.extend(line.split_whitespace());
        }
    }
    let body = paragraph.join(" ");
    if body.encode_utf16().count() <= 240 {
        return body;
    }
    let mut used = 0;
    let mut clipped = String::new();
    for character in body.chars() {
        used += character.len_utf16();
        if used > 239 {
            break;
        }
        clipped.push(character);
    }
    clipped.push('…');
    clipped
}

/// Count sessions that need input, never individual requests. The badge is
/// independent of notification opt-in, project muting, and visible sessions.
pub fn dock_badge_count(sessions: impl IntoIterator<Item = (i64, bool)>) -> usize {
    sessions
        .into_iter()
        .filter_map(|(id, needs_input)| needs_input.then_some(id))
        .collect::<HashSet<_>>()
        .len()
}
