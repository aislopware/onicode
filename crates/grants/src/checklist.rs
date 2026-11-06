//! The onboarding checklist, pure: each row's status as read, what its one button does, which row
//! is next, what needs a relaunch, and what is kept between launches.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::privilege::{Ask, Privilege, Read, Relaunch};

/// A privilege's state, as a read answers it or as the checklist settles it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum Status {
    /// Not read yet, or the read could not tell.
    #[default]
    Unknown,
    /// Nothing to ask here: no helper bundle in this build, or no login item plist.
    Unavailable,
    /// Off, from an API that cannot tell "never asked" from "refused" (the preflights). The
    /// checklist settles it with whether it asked ([`Checklist::status`]).
    Off,
    /// Never asked: the system alert can still show.
    NotAsked,
    /// Refused, or off in System Settings: only System Settings turns it on now.
    Denied,
    /// On.
    Granted,
}

impl Status {
    /// A preflight's answer.
    #[must_use]
    pub const fn from_bool(on: bool) -> Self {
        if on { Self::Granted } else { Self::Off }
    }

    /// Whether the read says for certain that it is off.
    #[must_use]
    pub const fn is_off(self) -> bool {
        matches!(self, Self::Off | Self::NotAsked | Self::Denied)
    }
}

/// What a row's one button does.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Action {
    /// "Allow…": raise the system alert (or register the login item).
    Allow,
    /// "Open Settings": open the list and show the drag panel.
    OpenSettings,
    /// "I've added it": the person says a row with no status API is on.
    Confirm,
}

impl Action {
    /// The button's label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Allow => "Allow…",
            Self::OpenSettings => "Open Settings",
            Self::Confirm => "I've added it",
        }
    }
}

/// What the checklist keeps between launches, in `permissions.json` beside `ui-settings.json`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Progress {
    /// "Continue" was pressed once: the checklist opens again only for a revoked grant, a
    /// relaunch it asked for, or from Settings.
    pub finished: bool,
    /// Rows whose alert was raised: an API that only says "off" means refused for these.
    pub asked: BTreeSet<Privilege>,
    /// Rows put off with "Later".
    pub skipped: BTreeSet<Privilege>,
    /// Rows with no status API the person said are on.
    pub confirmed: BTreeSet<Privilege>,
    /// Rows seen on, so a grant turned off later is noticed.
    pub granted: BTreeSet<Privilege>,
    /// The app quit to relaunch from the checklist: open it again at the next launch.
    pub resume: bool,
}

impl Progress {
    /// The progress kept at `path`; `None` when there is none yet (a first launch) or it is
    /// unreadable.
    #[must_use]
    pub fn load(path: &Path) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Writes the progress to `path`, whole or not at all.
    ///
    /// # Errors
    ///
    /// The file or its temporary twin cannot be written or renamed.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    /// Whether the checklist opens at launch: a first launch, an unfinished checklist, a relaunch
    /// it asked for, or a grant seen on before and now off (`revoked`).
    #[must_use]
    pub fn opens_at_launch(progress: Option<&Self>, revoked: bool) -> bool {
        progress.is_none_or(|progress| !progress.finished || progress.resume || revoked)
    }
}

/// One row's state in this run.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Row {
    /// The last read.
    read: Status,
    /// The first read of this run that said anything: a relaunch is for what came on since.
    first: Option<Status>,
    /// System Settings was opened for it in this run.
    opened: bool,
}

/// The checklist's state.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Checklist {
    progress: Progress,
    rows: BTreeMap<Privilege, Row>,
    resumed: bool,
}

impl Checklist {
    /// The checklist as `progress` left it. A pending resume is taken: it opens the checklist
    /// once.
    #[must_use]
    pub fn new(mut progress: Progress) -> Self {
        let resumed = std::mem::take(&mut progress.resume);
        Self {
            progress,
            rows: Privilege::ALL
                .into_iter()
                .map(|privilege| (privilege, Row::default()))
                .collect(),
            resumed,
        }
    }

    /// What to keep.
    #[must_use]
    pub const fn progress(&self) -> &Progress {
        &self.progress
    }

    /// Whether this run continues one that quit to relaunch.
    #[must_use]
    pub const fn resumed(&self) -> bool {
        self.resumed
    }

    fn row(&self, privilege: Privilege) -> Row {
        self.rows.get(&privilege).copied().unwrap_or_default()
    }

    /// Takes a read of `privilege`; `true` when what the checklist shows changed.
    pub fn read(&mut self, privilege: Privilege, read: Status) -> bool {
        let before = self.status(privilege);
        let granted_before = self.progress.granted.clone();
        let row = self.rows.entry(privilege).or_default();
        row.read = read;
        if row.first.is_none() && read != Status::Unknown {
            row.first = Some(read);
        }
        if self.status(privilege) == Status::Granted {
            self.progress.granted.insert(privilege);
        }
        before != self.status(privilege) || granted_before != self.progress.granted
    }

    /// The row's status, settled: a confirmed row is on, and an "off" from a preflight is
    /// "never asked" until the checklist raised its alert.
    #[must_use]
    pub fn status(&self, privilege: Privilege) -> Status {
        if privilege.read() == Read::Confirmed {
            return if self.progress.confirmed.contains(&privilege) {
                Status::Granted
            } else {
                Status::NotAsked
            };
        }
        match self.row(privilege).read {
            Status::Off if self.progress.asked.contains(&privilege) => Status::Denied,
            Status::Off => Status::NotAsked,
            read => read,
        }
    }

    /// Whether the row is shown: everything this Mac and build can ask for.
    #[must_use]
    pub fn shown(&self, privilege: Privilege) -> bool {
        self.status(privilege) != Status::Unavailable
    }

    /// The rows shown, in order.
    pub fn rows(&self) -> impl Iterator<Item = Privilege> + '_ {
        Privilege::ALL
            .into_iter()
            .filter(|&privilege| self.shown(privilege))
    }

    /// The row's one button; `None` once it is on, or while its state is unknown.
    #[must_use]
    pub fn action(&self, privilege: Privilege) -> Option<Action> {
        let status = self.status(privilege);
        let action = match (privilege.ask(), status) {
            (_, Status::Granted | Status::Unknown | Status::Unavailable) => return None,
            (Ask::Prompt | Ask::Register, Status::NotAsked | Status::Off) => Action::Allow,
            (Ask::Prompt | Ask::Register, Status::Denied) => Action::OpenSettings,
            (Ask::Drag, _) if privilege.read() == Read::Confirmed => {
                if self.row(privilege).opened {
                    Action::Confirm
                } else {
                    Action::OpenSettings
                }
            }
            (Ask::Drag, Status::NotAsked | Status::Off) if privilege.first_prompts() => {
                Action::Allow
            }
            (Ask::Drag, _) => Action::OpenSettings,
        };
        Some(action)
    }

    /// Whether the row was put off with "Later".
    #[must_use]
    pub fn skipped(&self, privilege: Privilege) -> bool {
        self.progress.skipped.contains(&privilege)
    }

    /// The row to do next: the first shown that is neither on nor put off.
    #[must_use]
    pub fn next(&self) -> Option<Privilege> {
        self.rows().find(|&privilege| {
            self.status(privilege) != Status::Granted && !self.skipped(privilege)
        })
    }

    /// Whether every shown row is on.
    #[must_use]
    pub fn all_granted(&self) -> bool {
        self.rows()
            .all(|privilege| self.status(privilege) == Status::Granted)
    }

    /// "Allow…" was pressed: the alert was raised.
    pub fn allowed(&mut self, privilege: Privilege) {
        self.progress.asked.insert(privilege);
        self.progress.skipped.remove(&privilege);
    }

    /// "Open Settings" was pressed.
    pub fn opened_settings(&mut self, privilege: Privilege) {
        if let Some(row) = self.rows.get_mut(&privilege) {
            row.opened = true;
        }
        self.progress.skipped.remove(&privilege);
    }

    /// "I've added it" was pressed, or the drag panel saw the row turned on.
    pub fn confirm(&mut self, privilege: Privilege) {
        if privilege.read() == Read::Confirmed {
            self.progress.confirmed.insert(privilege);
            self.progress.granted.insert(privilege);
            self.progress.skipped.remove(&privilege);
        }
    }

    /// "Later" was pressed.
    pub fn later(&mut self, privilege: Privilege) {
        self.progress.skipped.insert(privilege);
    }

    /// The rows the app must quit and reopen for: each needs a relaunch, was off when this run
    /// first read it, and has come on since or had System Settings opened for it.
    #[must_use]
    pub fn relaunch_for(&self) -> Vec<Privilege> {
        self.rows()
            .filter(|&privilege| {
                let row = self.row(privilege);
                // A confirmed row's first state is whether it was confirmed at launch
                // ([`Self::start`]).
                privilege.relaunch() == Relaunch::App
                    && row.first != Some(Status::Granted)
                    && (self.status(privilege) == Status::Granted || row.opened)
            })
            .collect()
    }

    /// Notes the first state of the confirmed rows, which no read reports. Call once, when the
    /// checklist opens.
    pub fn start(&mut self) {
        for privilege in Privilege::ALL {
            if privilege.read() == Read::Confirmed {
                let on = self.progress.confirmed.contains(&privilege);
                if let Some(row) = self.rows.get_mut(&privilege) {
                    row.first.get_or_insert(Status::from_bool(on));
                }
            }
        }
    }

    /// The app is about to quit and reopen: open the checklist again after.
    pub fn relaunching(&mut self) {
        self.progress.resume = true;
    }

    /// Grants seen on before that a read now says are off.
    #[must_use]
    pub fn revoked(&self) -> Vec<Privilege> {
        self.progress
            .granted
            .iter()
            .copied()
            .filter(|&privilege| self.status(privilege).is_off())
            .collect()
    }

    /// "Continue": the checklist is done, rows left or not. A revoked grant is forgotten, so it
    /// does not reopen the checklist at every launch.
    pub fn finish(&mut self) {
        self.progress.finished = true;
        for privilege in self.revoked() {
            self.progress.granted.remove(&privilege);
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    fn all_read(checklist: &mut Checklist, status: Status) {
        for privilege in Privilege::ALL {
            checklist.read(privilege, status);
        }
    }

    #[test]
    fn a_preflight_off_is_not_asked_until_the_alert_was_raised() {
        let mut checklist = Checklist::new(Progress::default());
        checklist.read(Privilege::Accessibility, Status::Off);
        assert_eq!(checklist.status(Privilege::Accessibility), Status::NotAsked);
        assert_eq!(
            checklist.action(Privilege::Accessibility),
            Some(Action::Allow)
        );
        checklist.allowed(Privilege::Accessibility);
        assert_eq!(checklist.status(Privilege::Accessibility), Status::Denied);
        assert_eq!(
            checklist.action(Privilege::Accessibility),
            Some(Action::OpenSettings)
        );
        checklist.read(Privilege::Accessibility, Status::Granted);
        assert_eq!(checklist.action(Privilege::Accessibility), None);
    }

    #[test]
    fn each_kind_of_row_has_its_button() {
        let mut checklist = Checklist::new(Progress::default());
        all_read(&mut checklist, Status::NotAsked);
        assert_eq!(checklist.action(Privilege::Microphone), Some(Action::Allow));
        assert_eq!(
            checklist.action(Privilege::BackgroundActivity),
            Some(Action::Allow)
        );
        assert_eq!(
            checklist.action(Privilege::FullDiskAccess),
            Some(Action::OpenSettings)
        );
        // Paste from Other Apps lists the app only after its first alert.
        assert_eq!(checklist.action(Privilege::Pasteboard), Some(Action::Allow));
        checklist.read(Privilege::Pasteboard, Status::Denied);
        assert_eq!(
            checklist.action(Privilege::Pasteboard),
            Some(Action::OpenSettings)
        );
        // No status API: Settings first, then the person says it is on.
        assert_eq!(
            checklist.action(Privilege::DeveloperTools),
            Some(Action::OpenSettings)
        );
        checklist.opened_settings(Privilege::DeveloperTools);
        assert_eq!(
            checklist.action(Privilege::DeveloperTools),
            Some(Action::Confirm)
        );
        checklist.confirm(Privilege::DeveloperTools);
        assert_eq!(checklist.status(Privilege::DeveloperTools), Status::Granted);
        assert_eq!(checklist.action(Privilege::DeveloperTools), None);
        // A login item waiting for approval goes to Login Items.
        checklist.read(Privilege::BackgroundActivity, Status::Denied);
        assert_eq!(
            checklist.action(Privilege::BackgroundActivity),
            Some(Action::OpenSettings)
        );
    }

    #[test]
    fn next_walks_the_rows_in_order_past_later_and_granted() {
        let mut checklist = Checklist::new(Progress::default());
        all_read(&mut checklist, Status::NotAsked);
        assert_eq!(checklist.next(), Some(Privilege::ScreenRecording));
        checklist.read(Privilege::ScreenRecording, Status::Granted);
        assert_eq!(checklist.next(), Some(Privilege::Accessibility));
        checklist.later(Privilege::Accessibility);
        assert_eq!(checklist.next(), Some(Privilege::PostEvent));
        // Asking again takes the row back.
        checklist.allowed(Privilege::Accessibility);
        assert_eq!(checklist.next(), Some(Privilege::Accessibility));
    }

    #[test]
    fn unavailable_rows_are_hidden() {
        let mut checklist = Checklist::new(Progress::default());
        all_read(&mut checklist, Status::Granted);
        for privilege in Privilege::HELPER {
            checklist.read(privilege, Status::Unavailable);
        }
        assert!(!checklist.rows().any(|p| Privilege::HELPER.contains(&p)));
        // The confirmed rows are never read; until confirmed they are not on.
        assert_eq!(checklist.next(), Some(Privilege::AppManagement));
        checklist.confirm(Privilege::AppManagement);
        checklist.confirm(Privilege::DeveloperTools);
        assert!(checklist.all_granted());
        assert_eq!(checklist.next(), None);
    }

    #[test]
    fn a_relaunch_is_for_what_came_on_in_this_run() {
        let mut checklist = Checklist::new(Progress::default());
        checklist.start();
        all_read(&mut checklist, Status::NotAsked);
        assert!(checklist.relaunch_for().is_empty());
        // Live rows never ask for one.
        checklist.read(Privilege::Microphone, Status::Granted);
        assert!(checklist.relaunch_for().is_empty());
        checklist.read(Privilege::InputMonitoring, Status::Granted);
        assert_eq!(checklist.relaunch_for(), [Privilege::InputMonitoring]);
        // Full Disk Access may read off until the relaunch: opening Settings is enough.
        checklist.opened_settings(Privilege::FullDiskAccess);
        assert_eq!(
            checklist.relaunch_for(),
            [Privilege::InputMonitoring, Privilege::FullDiskAccess]
        );
        checklist.confirm(Privilege::AppManagement);
        assert!(checklist.relaunch_for().contains(&Privilege::AppManagement));
        // Screen Recording is the helper's to restart for.
        checklist.read(Privilege::ScreenRecording, Status::Granted);
        assert!(
            !checklist
                .relaunch_for()
                .contains(&Privilege::ScreenRecording)
        );

        // After the relaunch, everything on reads on from the start.
        checklist.relaunching();
        let mut next = Checklist::new(checklist.progress().clone());
        assert!(next.resumed());
        assert!(!next.progress().resume, "a resume opens once");
        next.start();
        all_read(&mut next, Status::Granted);
        assert!(next.relaunch_for().is_empty());
    }

    #[test]
    fn progress_survives_a_save() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("permissions.json");
        assert_eq!(Progress::load(&path), None);
        assert!(Progress::opens_at_launch(None, false));
        let mut checklist = Checklist::new(Progress::default());
        checklist.read(Privilege::Microphone, Status::Granted);
        checklist.allowed(Privilege::Automation);
        checklist.later(Privilege::FullDiskAccess);
        checklist.confirm(Privilege::DeveloperTools);
        checklist.finish();
        checklist.progress().save(&path).expect("saved");
        let loaded = Progress::load(&path);
        assert_eq!(loaded.as_ref(), Some(checklist.progress()));
        assert!(!Progress::opens_at_launch(loaded.as_ref(), false));
        assert!(Progress::opens_at_launch(loaded.as_ref(), true));
        // Unknown fields and missing ones are fine; garbage reads as a first launch.
        std::fs::write(&path, br#"{"finished":true,"future":1}"#).expect("written");
        assert_eq!(Progress::load(&path).map(|p| p.finished), Some(true));
        std::fs::write(&path, b"not json").expect("written");
        assert_eq!(Progress::load(&path), None);
    }

    #[test]
    fn a_revoked_grant_reopens_until_continue() {
        let mut checklist = Checklist::new(Progress::default());
        checklist.read(Privilege::Microphone, Status::Granted);
        checklist.finish();
        let mut later = Checklist::new(checklist.progress().clone());
        later.read(Privilege::Microphone, Status::Denied);
        assert_eq!(later.revoked(), [Privilege::Microphone]);
        assert!(Progress::opens_at_launch(
            Some(later.progress()),
            !later.revoked().is_empty()
        ));
        // Unknown is no revoke.
        let mut unsure = Checklist::new(checklist.progress().clone());
        unsure.read(Privilege::Microphone, Status::Unknown);
        assert!(unsure.revoked().is_empty());
        later.finish();
        assert!(later.revoked().is_empty());
    }

    fn status() -> impl Strategy<Value = Status> {
        prop_oneof![
            Just(Status::Unknown),
            Just(Status::Unavailable),
            Just(Status::Off),
            Just(Status::NotAsked),
            Just(Status::Denied),
            Just(Status::Granted),
        ]
    }

    #[derive(Clone, Debug)]
    enum Step {
        Read(Privilege, Status),
        Allow(Privilege),
        Open(Privilege),
        Confirm(Privilege),
        Later(Privilege),
    }

    fn privilege() -> impl Strategy<Value = Privilege> {
        (0..Privilege::ALL.len()).prop_map(|ix| Privilege::ALL[ix])
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            4 => (privilege(), status()).prop_map(|(p, s)| Step::Read(p, s)),
            1 => privilege().prop_map(Step::Allow),
            1 => privilege().prop_map(Step::Open),
            1 => privilege().prop_map(Step::Confirm),
            1 => privilege().prop_map(Step::Later),
        ]
    }

    proptest! {
        #[test]
        fn the_model_holds_its_promises(steps in prop::collection::vec(step(), 0..80)) {
            let mut checklist = Checklist::new(Progress::default());
            checklist.start();
            for step in steps {
                match step {
                    Step::Read(p, s) => { checklist.read(p, s); }
                    Step::Allow(p) => checklist.allowed(p),
                    Step::Open(p) => checklist.opened_settings(p),
                    Step::Confirm(p) => checklist.confirm(p),
                    Step::Later(p) => checklist.later(p),
                }
                for p in Privilege::ALL {
                    let status = checklist.status(p);
                    // A row on has no button; "Off" never shows.
                    prop_assert_eq!(status == Status::Granted, checklist.action(p).is_none()
                        && !matches!(status, Status::Unknown | Status::Unavailable));
                    prop_assert_ne!(status, Status::Off);
                    // Only rows without a status API are confirmed.
                    if checklist.action(p) == Some(Action::Confirm) {
                        prop_assert_eq!(p.read(), Read::Confirmed);
                    }
                }
                // The next row is shown, not on, not put off.
                if let Some(next) = checklist.next() {
                    prop_assert!(checklist.shown(next));
                    prop_assert_ne!(checklist.status(next), Status::Granted);
                    prop_assert!(!checklist.skipped(next));
                }
                // Every relaunch row needs one and is shown.
                for p in checklist.relaunch_for() {
                    prop_assert_eq!(p.relaunch(), Relaunch::App);
                    prop_assert!(checklist.shown(p));
                }
                // Everything revoked was once seen on.
                for p in checklist.revoked() {
                    prop_assert!(checklist.progress().granted.contains(&p));
                }
            }
            checklist.finish();
            prop_assert!(checklist.revoked().is_empty());
        }
    }
}
