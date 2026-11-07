//! A program's OSC 7501 records (program status, rev 0.4), kept by the engine's host for one
//! terminal, viewers or not.
//!
//! libghostty parses and validates each report and answers the support query, but only for the
//! host, which alone registers for them; a viewer's mirror stays silent. The records follow the
//! specification: one per id, a report replaces its record whole, a clear removes its id and every
//! id beneath it (an empty id removes all, as a full reset does), a new prompt (OSC 133 A) or the
//! shell's exit drops the live states (working, blocked and idle) while done and error stay, and
//! at most [`MAX_RECORDS`] are kept, the one updated longest ago going first. What viewers see is
//! one record, the most urgent ([`Programs::status`]).
use std::collections::BTreeMap;

use onicode_protocol::terminal::{InputKind, ProgramState, ProgramStatus};

/// The records one terminal keeps.
pub const MAX_RECORDS: usize = 256;

/// One validated report, its text already cleaned ([`clean`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// Empty for the program itself; `/` nests one record under another.
    pub id: String,
    pub state: ProgramState,
    pub kind: Option<InputKind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
}

/// What changes the records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Report(Report),
    /// Removes the id and every id beneath it; an empty id removes every record.
    Clear(String),
    /// A shell prompt started (OSC 133 A).
    PromptStart,
    /// The shell ended.
    Exit,
}

struct Record {
    report: Report,
    /// When it was last reported, by the records' own clock.
    touched: u64,
}

/// One terminal's records.
#[derive(Default)]
pub struct Programs {
    records: BTreeMap<String, Record>,
    clock: u64,
    /// The records changed since [`Self::take_changed`].
    changed: bool,
}

impl std::fmt::Debug for Programs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Programs")
            .field("records", &self.records.len())
            .finish_non_exhaustive()
    }
}

impl Programs {
    /// Applies one change; true when the records changed.
    pub fn apply(&mut self, change: Change) -> bool {
        let changed = match change {
            Change::Report(report) => self.report(report),
            Change::Clear(id) => {
                let before = self.records.len();
                self.records.retain(|key, _| !beneath(key, &id));
                self.records.len() != before
            }
            Change::PromptStart | Change::Exit => {
                let before = self.records.len();
                self.records.retain(|_, record| {
                    matches!(
                        record.report.state,
                        ProgramState::Done | ProgramState::Error
                    )
                });
                self.records.len() != before
            }
        };
        self.changed |= changed;
        changed
    }

    fn report(&mut self, report: Report) -> bool {
        self.clock = self.clock.wrapping_add(1);
        let touched = self.clock;
        if let Some(record) = self.records.get_mut(&report.id) {
            record.touched = touched;
            if record.report == report {
                return false;
            }
            record.report = report;
            return true;
        }
        if self.records.len() >= MAX_RECORDS
            && let Some(oldest) = self
                .records
                .iter()
                .min_by_key(|(_, record)| record.touched)
                .map(|(id, _)| id.clone())
        {
            self.records.remove(&oldest);
        }
        self.records
            .insert(report.id.clone(), Record { report, touched });
        true
    }

    /// Whether the records changed since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// How many records there are.
    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// No record at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The most urgent record: blocked, then working, error, done and idle, the latest report
    /// first among equals, so an idle or done parent never hides a working or blocked child. A
    /// record without an app takes its nearest ancestor's.
    #[must_use]
    pub fn status(&self) -> Option<ProgramStatus> {
        let (id, record) = self.records.iter().min_by(|(a_id, a), (b_id, b)| {
            urgency(a.report.state)
                .cmp(&urgency(b.report.state))
                .then_with(|| b.touched.cmp(&a.touched))
                .then_with(|| a_id.cmp(b_id))
        })?;
        let report = &record.report;
        let mut app = report.app.clone();
        let mut parent = id.as_str();
        while app.is_none() && !parent.is_empty() {
            parent = parent.rsplit_once('/').map_or("", |(parent, _)| parent);
            app = self
                .records
                .get(parent)
                .and_then(|record| record.report.app.clone());
        }
        Some(ProgramStatus {
            state: report.state,
            kind: report.kind,
            progress: report.progress,
            app,
            title: report.title.clone(),
            message: report.message.clone(),
        })
    }
}

/// `key` is `id` or beneath it; everything is beneath the empty id.
fn beneath(key: &str, id: &str) -> bool {
    id.is_empty()
        || key
            .strip_prefix(id)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Lower is more urgent.
const fn urgency(state: ProgramState) -> u8 {
    match state {
        ProgramState::Blocked => 0,
        ProgramState::Working => 1,
        ProgramState::Error => 2,
        ProgramState::Done => 3,
        ProgramState::Idle => 4,
    }
}

/// A program's text made safe to show outside the terminal: invalid UTF-8 replaced, control
/// characters and the invisible formatting that reorders text (bidi embeddings, overrides,
/// isolates and marks) removed. `None` when nothing is left.
#[must_use]
pub fn clean(bytes: &[u8]) -> Option<String> {
    let text: String = String::from_utf8_lossy(bytes)
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

const fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: &str, state: ProgramState) -> Report {
        Report {
            id: id.to_owned(),
            state,
            kind: None,
            progress: None,
            app: None,
            title: None,
            message: None,
        }
    }

    fn report(id: &str, state: ProgramState) -> Change {
        Change::Report(record(id, state))
    }

    #[test]
    fn children_inherit_the_app_and_clears_take_only_their_subtree() {
        let mut programs = Programs::default();
        programs.apply(Change::Report(Report {
            app: Some("claude-code".into()),
            ..record("", ProgramState::Idle)
        }));
        programs.apply(report("build", ProgramState::Working));
        programs.apply(report("build/test", ProgramState::Blocked));
        programs.apply(report("builder", ProgramState::Done));
        let status = programs.status().unwrap();
        assert_eq!(
            status.state,
            ProgramState::Blocked,
            "a child outranks its idle root"
        );
        assert_eq!(status.app.as_deref(), Some("claude-code"));
        programs.apply(Change::Clear("build".into()));
        assert_eq!(programs.len(), 2);
        assert_eq!(programs.status().unwrap().state, ProgramState::Done);
        programs.apply(Change::Clear(String::new()));
        assert!(programs.status().is_none());
    }

    #[test]
    fn a_prompt_drops_live_records_and_keeps_finished_ones() {
        let mut programs = Programs::default();
        programs.apply(report("", ProgramState::Working));
        programs.apply(report("a", ProgramState::Error));
        programs.apply(report("b", ProgramState::Idle));
        assert!(programs.apply(Change::PromptStart));
        assert_eq!(programs.len(), 1);
        assert_eq!(programs.status().unwrap().state, ProgramState::Error);
        assert!(!programs.apply(Change::Exit));
    }

    #[test]
    fn records_are_bounded_and_the_stalest_goes() {
        let mut programs = Programs::default();
        for n in 0..MAX_RECORDS + 10 {
            programs.apply(report(&format!("r{n}"), ProgramState::Idle));
        }
        assert_eq!(programs.len(), MAX_RECORDS);
        assert!(!programs.records.contains_key("r0"));
        assert!(
            programs
                .records
                .contains_key(&format!("r{}", MAX_RECORDS + 9))
        );
    }

    #[test]
    fn text_is_disarmed() {
        assert_eq!(
            clean(b"ok\x07 \xe2\x80\xaeevil").as_deref(),
            Some("ok evil")
        );
        assert_eq!(clean("\u{2067}\u{200f}".as_bytes()), None);
        assert_eq!(clean(b"  "), None);
    }
}
