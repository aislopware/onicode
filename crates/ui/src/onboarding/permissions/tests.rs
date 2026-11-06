//! The checklist in a test window on a recording Mac: nothing prompts, nothing opens System
//! Settings, nothing takes focus.

use std::sync::Mutex;

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext};

use super::*;

/// What the checklist asked of the Mac.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Call {
    Read { helper: bool },
    Allow(Privilege),
    OpenSettings(Privilege),
    QuitAndReopen,
}

struct Fake {
    install: Install,
    reads: Mutex<Vec<(Privilege, Status)>>,
    calls: Mutex<Vec<Call>>,
    guide_granted: Mutex<Option<Privilege>>,
}

impl Fake {
    fn new(install: Install, reads: Vec<(Privilege, Status)>) -> Arc<Self> {
        Arc::new(Self {
            install,
            reads: Mutex::new(reads),
            calls: Mutex::new(Vec::new()),
            guide_granted: Mutex::new(None),
        })
    }

    fn take(&self) -> Vec<Call> {
        std::mem::take(&mut self.calls.lock().unwrap())
    }

    fn set(&self, privilege: Privilege, status: Status) {
        let mut reads = self.reads.lock().unwrap();
        reads.retain(|(p, _)| *p != privilege);
        reads.push((privilege, status));
    }

    fn push(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Platform for Fake {
    fn install(&self) -> Install {
        self.install
    }

    fn read(&self, plan: &ReadPlan) -> Vec<(Privilege, Status)> {
        self.push(Call::Read {
            helper: plan.helper,
        });
        self.reads.lock().unwrap().clone()
    }

    fn allow(&self, privilege: Privilege) {
        self.push(Call::Allow(privilege));
    }

    fn open_settings(&self, privilege: Privilege) {
        self.push(Call::OpenSettings(privilege));
    }

    fn report(&self, _privilege: Privilege, _granted: bool) {}

    fn guide_granted(&self) -> Option<Privilege> {
        self.guide_granted.lock().unwrap().take()
    }

    fn quit_and_reopen(&self) -> bool {
        self.push(Call::QuitAndReopen);
        false
    }
}

/// Every row read as `status`.
fn every(status: Status) -> Vec<(Privilege, Status)> {
    Privilege::ALL
        .into_iter()
        .map(|privilege| (privilege, status))
        .collect()
}

/// Only `shown` rows on screen, read as given; the rest this Mac cannot ask about. The rows
/// without a status API are never read, so they always show.
fn only(shown: &[(Privilege, Status)]) -> Vec<(Privilege, Status)> {
    let mut reads = every(Status::Unavailable);
    for &(privilege, status) in shown {
        reads.retain(|(p, _)| *p != privilege);
        reads.push((privilege, status));
    }
    reads
}

/// The calls other than reads, which the window's activation may add.
fn asks(calls: Vec<Call>) -> Vec<Call> {
    calls
        .into_iter()
        .filter(|call| !matches!(call, Call::Read { .. }))
        .collect()
}

fn checklist<'a>(
    cx: &'a mut TestAppContext,
    fake: &Arc<Fake>,
) -> (Entity<Permissions>, &'a mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let platform: Arc<dyn Platform> = fake.clone();
    let (view, cx) = cx.add_window_view(|window, cx| {
        Permissions::new(platform, Progress::default(), None, None, window, cx)
    });
    cx.update(|window, _| window.set_check_retained(Some(gpui::RetainedCheck::Panic)));
    cx.run_until_parked();
    (view, cx)
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

/// Each status shows its row's one button: none when on, "Allow…" never asked, "Open Settings"
/// refused; a row the Mac cannot ask about is not shown.
#[gpui::test]
fn every_status_has_its_row(cx: &mut TestAppContext) {
    let fake = Fake::new(
        Install::Settled,
        only(&[
            (Privilege::ScreenRecording, Status::Granted),
            (Privilege::Accessibility, Status::Off),
            (Privilege::BackgroundActivity, Status::Denied),
            (Privilege::InputMonitoring, Status::Unknown),
        ]),
    );
    let (view, cx) = checklist(cx, &fake);
    assert_eq!(
        fake.take().first(),
        Some(&Call::Read { helper: true }),
        "read at once, the helper too"
    );

    assert!(cx.debug_bounds("permissions-screen-recording").is_some());
    assert!(
        cx.debug_bounds("permissions-screen-recording-action")
            .is_none()
    );
    assert!(
        cx.debug_bounds("permissions-accessibility-action")
            .is_some()
    );
    assert!(cx.debug_bounds("permissions-accessibility-later").is_some());
    assert!(cx.debug_bounds("permissions-post-event").is_none());
    assert!(
        cx.debug_bounds("permissions-background-activity-action")
            .is_some()
    );
    assert!(cx.debug_bounds("permissions-input-monitoring").is_some());
    assert!(
        cx.debug_bounds("permissions-input-monitoring-action")
            .is_none(),
        "unknown: no button until a read says"
    );
    assert!(cx.debug_bounds("permissions-microphone").is_none());
    assert!(cx.debug_bounds("permissions-continue").is_some());
    view.read_with(cx, |view, _| {
        let checklist = view.checklist();
        assert_eq!(checklist.status(Privilege::Accessibility), Status::NotAsked);
        assert_eq!(
            checklist.action(Privilege::BackgroundActivity),
            Some(Action::OpenSettings)
        );
        assert_eq!(checklist.next(), Some(Privilege::Accessibility));
    });
}

/// "Allow…" asks the Mac once and the rows are read again; "Open Settings" opens the list;
/// "Later" moves on and asks nothing.
#[gpui::test]
fn buttons_call_the_mac(cx: &mut TestAppContext) {
    let fake = Fake::new(
        Install::Settled,
        only(&[
            (Privilege::ScreenRecording, Status::Off),
            (Privilege::Accessibility, Status::Off),
            (Privilege::BackgroundActivity, Status::Denied),
        ]),
    );
    let (view, cx) = checklist(cx, &fake);
    fake.take();

    click(cx, "permissions-screen-recording-action");
    let calls = fake.take();
    assert_eq!(
        asks(calls.clone()),
        [Call::Allow(Privilege::ScreenRecording)]
    );
    assert!(
        calls.contains(&Call::Read { helper: true }),
        "read again after"
    );
    view.read_with(cx, |view, _| {
        let checklist = view.checklist();
        assert!(
            checklist
                .progress()
                .asked
                .contains(&Privilege::ScreenRecording)
        );
        // Still off after its alert: only System Settings turns it on now.
        assert_eq!(
            checklist.action(Privilege::ScreenRecording),
            Some(Action::OpenSettings)
        );
    });

    click(cx, "permissions-background-activity-action");
    assert_eq!(
        asks(fake.take()),
        [Call::OpenSettings(Privilege::BackgroundActivity)]
    );

    click(cx, "permissions-accessibility-later");
    view.read_with(cx, |view, _| {
        assert!(view.checklist().skipped(Privilege::Accessibility));
    });
    assert!(cx.debug_bounds("permissions-accessibility-later").is_none());
    assert!(
        asks(fake.take()).is_empty(),
        "Later asks nothing of the Mac"
    );
}

/// A row without a status API: Settings first, then the person's word; the drag panel's
/// "granted" counts as that word.
#[gpui::test]
fn rows_without_a_status_are_confirmed(cx: &mut TestAppContext) {
    let fake = Fake::new(Install::Settled, only(&[]));
    let (view, cx) = checklist(cx, &fake);
    fake.take();
    click(cx, "permissions-developer-tools-action");
    assert_eq!(
        asks(fake.take()),
        [Call::OpenSettings(Privilege::DeveloperTools)]
    );
    click(cx, "permissions-developer-tools-action");
    view.read_with(cx, |view, _| {
        assert_eq!(
            view.checklist().status(Privilege::DeveloperTools),
            Status::Granted
        );
    });

    *fake.guide_granted.lock().unwrap() = Some(Privilege::AppManagement);
    view.update(cx, |view, cx| view.poll(cx));
    cx.run_until_parked();
    view.read_with(cx, |view, _| assert!(view.checklist().all_granted()));
}

/// A grant that takes a relaunch offers "Quit & Reopen", which asks the Mac to reopen.
#[gpui::test]
fn a_relaunch_grant_offers_quit_and_reopen(cx: &mut TestAppContext) {
    let fake = Fake::new(
        Install::Settled,
        only(&[(Privilege::InputMonitoring, Status::NotAsked)]),
    );
    let (view, cx) = checklist(cx, &fake);
    assert!(cx.debug_bounds("permissions-reopen").is_none());
    fake.set(Privilege::InputMonitoring, Status::Granted);
    view.update(cx, |view, cx| view.poll(cx));
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("permissions-input-monitoring-reopen")
            .is_some()
    );
    fake.take();
    click(cx, "permissions-reopen");
    assert_eq!(asks(fake.take()), [Call::QuitAndReopen]);
    view.read_with(cx, |view, _| {
        assert!(
            !view.checklist().progress().resume,
            "the fake could not reopen"
        );
    });
}

/// From a disk image the checklist reads nothing and says to move the app.
#[gpui::test]
fn a_disk_image_is_refused(cx: &mut TestAppContext) {
    let fake = Fake::new(Install::Volume, every(Status::NotAsked));
    let (view, cx) = checklist(cx, &fake);
    assert!(fake.take().is_empty(), "nothing read");
    assert!(cx.debug_bounds("permissions-refusal").is_some());
    assert!(cx.debug_bounds("permissions-screen-recording").is_none());
    view.update(cx, |view, cx| view.poll(cx));
    cx.run_until_parked();
    assert!(fake.take().is_empty());
}

/// Once the helper's rows are on, the helper is not started for every read.
#[gpui::test]
fn the_helper_is_probed_only_while_needed(cx: &mut TestAppContext) {
    let fake = Fake::new(Install::Settled, every(Status::Granted));
    let (view, cx) = checklist(cx, &fake);
    assert_eq!(fake.take().first(), Some(&Call::Read { helper: true }));
    view.update(cx, |view, cx| view.poll(cx));
    cx.run_until_parked();
    assert_eq!(fake.take().last(), Some(&Call::Read { helper: false }));
}
