//! Shared app updater: one check/install across windows; no I/O on the UI thread.
//! The source is Onicode's updater (`onicode-update`): the release feed's manifest
//! says what's offered, `desktop::stage_offer` downloads and verifies it, and
//! `InstallKind::apply_desktop` swaps the bundle and relaunches.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::prelude::*;
use gpui::{App, Context, Entity, EventEmitter, Global, Role, div, px};
use onicode_update::{Channel, Manifest, Offer, channel, desktop};

use crate::theme::*;
use crate::ui_state::UiState;

/// Where an install records the version it put in place, for the
/// "Updated to …" card at the next launch (`app_update.rs`).
pub(crate) const INSTALLED_MARKER: &str = "onicode.installedUpdate";

#[derive(Clone)]
pub(crate) struct Shared(pub Entity<Updater>, Arc<Mutex<InstallGate>>);
impl Global for Shared {}
#[derive(Default)]
struct InstallGate {
    installing: bool,
    quitting: bool,
}

/// A normal quit can cancel a download, but must not interrupt the bundle
/// swap. Keep this gate outside the entity borrow so a finished install can
/// call the regular quit path from its callback.
pub(crate) fn defer_quit(cx: &App) -> bool {
    let Some(shared) = cx.try_global::<Shared>() else {
        return false;
    };
    let mut gate = shared.1.lock().unwrap_or_else(|e| e.into_inner());
    gate.quitting = true;
    gate.installing
}
pub(crate) enum Event {
    Changed,
    Error(String),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Phase {
    Disabled,
    #[default]
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Installing,
}
enum Progress {
    Download(u64, Option<u64>),
    Installing,
}

/// The update row's state machine, apart from the I/O that drives it.
#[derive(Debug, Default)]
struct Machine {
    phase: Phase,
    /// The version the last check offered.
    pending: Option<String>,
    /// Download percent, when the feed says how big the release is.
    progress: Option<u64>,
    /// The last check's or install's outcome, in words for the person.
    message: Option<String>,
}
impl Machine {
    fn disabled(why: &str) -> Self {
        Self {
            phase: Phase::Disabled,
            message: Some(why.into()),
            ..Self::default()
        }
    }

    fn busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Checking | Phase::Downloading | Phase::Installing
        )
    }

    fn actionable(&self) -> bool {
        matches!(
            self.phase,
            Phase::Available | Phase::Downloading | Phase::Installing
        )
    }

    /// Start a check, unless one can't start now.
    fn begin_check(&mut self) -> bool {
        if self.busy() || self.phase == Phase::Disabled {
            return false;
        }
        self.phase = Phase::Checking;
        self.message = None;
        true
    }

    /// What the feed offers this app (`Ok`), or why it couldn't be asked.
    /// A failed check keeps an offer already found.
    fn checked(&mut self, result: Result<(Offer, String), String>) {
        match result {
            Ok((offer, latest)) => match offer {
                Offer::Newer | Offer::Switch => {
                    self.phase = Phase::Available;
                    self.pending = Some(latest);
                }
                Offer::Current => {
                    self.phase = Phase::Current;
                    self.pending = None;
                }
                Offer::Held(why) => {
                    self.phase = Phase::Current;
                    self.pending = None;
                    self.message = Some(why);
                }
            },
            Err(error) => {
                self.phase = if self.pending.is_some() {
                    Phase::Available
                } else {
                    Phase::Idle
                };
                self.message = Some(error);
            }
        }
    }

    /// Whether there's an offer and no work under way.
    fn can_install(&self) -> bool {
        self.phase == Phase::Available && self.pending.is_some()
    }

    /// Start installing the offered version, unless it can't start now.
    fn begin_install(&mut self) -> bool {
        if !self.can_install() {
            return false;
        }
        self.phase = Phase::Downloading;
        self.progress = Some(0);
        self.message = None;
        true
    }

    fn progressed(&mut self, progress: Progress) {
        match progress {
            Progress::Download(done, total) => {
                self.progress = total
                    .filter(|total| *total > 0)
                    .map(|total| (done.saturating_mul(100) / total).min(100));
            }
            Progress::Installing => self.phase = Phase::Installing,
        }
    }

    /// The install stopped short: the offer stands, with why.
    fn install_failed(&mut self, error: String) {
        self.phase = Phase::Available;
        self.progress = None;
        self.message = Some(error);
    }

    fn progress_label(&self) -> String {
        match self.phase {
            Phase::Installing => "Installing…".into(),
            _ => self
                .progress
                .map(|p| format!("Downloading {p}%"))
                .unwrap_or_else(|| "Downloading…".into()),
        }
    }
}

/// Run one of `onicode-update`'s async calls to completion on this
/// (background) thread.
fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|runtime| runtime.block_on(future))
        .map_err(|error| format!("Couldn’t start the updater: {error}"))
}

pub(crate) fn shared(cx: &mut App) -> Entity<Updater> {
    if let Some(shared) = cx.try_global::<Shared>() {
        return shared.0.clone();
    }
    let ui_state = crate::ui_state::shared(cx);
    let entity = cx.new(|cx| Updater::new(ui_state, cx));
    let gate = entity.read(cx).gate.clone();
    cx.set_global(Shared(entity.clone(), gate));
    entity
}

/// `version`'s release notes, from the update manifest.
pub(crate) struct ReleaseNotes {
    pub markdown: String,
    /// `YYYY-MM-DD`.
    pub date: Option<String>,
}
impl ReleaseNotes {
    fn of(manifest: &Manifest, version: &str) -> Option<Self> {
        let markdown = manifest.notes.as_deref()?.trim();
        (manifest.version.trim() == version.trim() && !markdown.is_empty()).then(|| Self {
            markdown: markdown.to_owned(),
            date: manifest.date.clone(),
        })
    }
}

pub(crate) struct Updater {
    feed: Option<String>,
    data_dir: Option<PathBuf>,
    /// Where the install marker is kept for the next launch.
    ui_state: UiState,
    machine: Machine,
    /// The last manifest a check fetched: its notes feed What's new.
    manifest: Option<Manifest>,
    /// The channel this app follows, as last read off the main thread.
    channel: Channel,
    poll: Option<gpui::Task<()>>,
    gate: Arc<Mutex<InstallGate>>,
}
impl EventEmitter<Event> for Updater {}
impl Updater {
    fn new(ui_state: UiState, cx: &mut Context<Self>) -> Self {
        let feed = onicode_update::feed_url();
        let mut this = Self {
            machine: if feed.is_some() {
                Machine::default()
            } else {
                Machine::disabled("Automatic updates aren’t configured for this build")
            },
            feed,
            data_dir: channel::data_dir().map(Path::to_path_buf),
            ui_state,
            manifest: None,
            channel: Channel::of_version(onicode_update::app_version()),
            poll: None,
            gate: Arc::new(Mutex::new(InstallGate::default())),
        };
        if this.feed.is_some() {
            this.check(false, cx);
            this.poll = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_secs(4 * 60 * 60))
                        .await;
                    if this.update(cx, |this, cx| this.check(false, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        this
    }

    pub(crate) fn actionable(&self) -> bool {
        self.machine.actionable()
    }

    fn changed(&self, cx: &mut Context<Self>) {
        cx.emit(Event::Changed);
        cx.notify();
    }

    pub(crate) fn check(&mut self, manual: bool, cx: &mut Context<Self>) {
        let Some(feed) = self.feed.clone() else {
            if manual && let Some(why) = self.machine.message.clone() {
                cx.emit(Event::Error(format!("Couldn’t check for updates: {why}")));
            }
            self.changed(cx);
            return;
        };
        if !self.machine.begin_check() {
            return;
        }
        self.changed(cx);
        let work = cx.background_executor().spawn(async move {
            let install = onicode_update::detect_install();
            let chosen = channel::selected();
            let offer = block_on(desktop::current_offer(&feed, &install))
                .and_then(|result| result.map_err(|error| format!("{error:#}")));
            (offer, chosen)
        });
        cx.spawn(async move |this, cx| {
            let (result, chosen) = work.await;
            let _ = this.update(cx, |this, cx| {
                this.channel = chosen;
                let error = result.as_ref().err().cloned();
                let offer = result.map(|(offer, manifest)| {
                    let latest = manifest.version.clone();
                    this.manifest = Some(manifest);
                    (offer, latest)
                });
                this.machine.checked(offer);
                if manual && let Some(error) = error {
                    cx.emit(Event::Error(format!("Couldn’t check for updates: {error}")));
                }
                this.changed(cx);
            });
        })
        .detach();
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let Some(feed) = self.feed.clone() else {
            return;
        };
        if !self.machine.can_install() {
            return;
        }
        let install = onicode_update::detect_install();
        if !install.supports_desktop_update() {
            // A source build or hand-copied app: the release is a download.
            cx.open_url(onicode_update::LATEST_RELEASE_PAGE);
            return;
        }
        if !self.machine.begin_install() {
            return;
        }
        self.changed(cx); // Close the double-click window before starting I/O.
        let data_dir = self.data_dir.clone();
        let ui_state = self.ui_state.clone();
        let gate = self.gate.clone();
        let (tx, rx) = async_channel::bounded(8);
        let work = cx.background_executor().spawn(async move {
            let result = (|| -> Result<(), String> {
                if let Some(blocker) = install.desktop_update_blocker() {
                    return Err(blocker.to_string());
                }
                let data_dir =
                    data_dir.ok_or("Onicode has no data folder to stage the update in")?;
                // Report each new percent, or each MiB when the size is unknown.
                let last_bytes = AtomicU64::new(0);
                let last_percent = AtomicU64::new(u64::MAX);
                let report = |done: u64, total: Option<u64>| {
                    let percent = total
                        .filter(|total| *total > 0)
                        .map(|total| done.saturating_mul(100) / total);
                    let fresh = match percent {
                        Some(percent) => last_percent.swap(percent, Ordering::Relaxed) != percent,
                        None => {
                            done.saturating_sub(last_bytes.load(Ordering::Relaxed)) >= 1024 * 1024
                        }
                    };
                    if fresh {
                        last_bytes.store(done, Ordering::Relaxed);
                        let _ = tx.try_send(Progress::Download(done, total));
                    }
                };
                let (version, staged) =
                    block_on(desktop::stage_offer(&feed, &data_dir, &install, &report))?
                        .map_err(|error| format!("{error:#}"))?;
                {
                    let mut gate = gate.lock().unwrap_or_else(|e| e.into_inner());
                    if gate.quitting {
                        return Err("The app is closing; the update was not installed".into());
                    }
                    gate.installing = true;
                }
                let _ = tx.send_blocking(Progress::Installing);
                // The marker must reach the disk before the swap relaunches the app.
                ui_state.set(
                    INSTALLED_MARKER,
                    serde_json::json!({ "version": version }).to_string(),
                );
                ui_state.flush();
                install
                    .apply_desktop(&staged, true)
                    .map_err(|error| format!("{error:#}"))
            })();
            if result.is_err() {
                ui_state.set(INSTALLED_MARKER, String::new());
            }
            gate.lock().unwrap_or_else(|e| e.into_inner()).installing = false;
            result
        });
        cx.spawn(async move |this, cx| {
            while let Ok(progress) = rx.recv().await {
                if this
                    .update(cx, |this, cx| {
                        this.machine.progressed(progress);
                        this.changed(cx);
                    })
                    .is_err()
                {
                    return;
                }
            }
            let result = work.await;
            let _ = this.update(cx, |this, cx| match result {
                // The relauncher waits for this process to exit.
                Ok(()) => crate::quit(cx),
                Err(error) => {
                    this.machine.install_failed(error.clone());
                    cx.emit(Event::Error(format!(
                        "Couldn’t install the update: {error}"
                    )));
                    this.changed(cx);
                    if this.gate.lock().unwrap_or_else(|e| e.into_inner()).quitting {
                        crate::quit(cx);
                    }
                }
            });
        })
        .detach();
    }

    /// Follow `beta` or stable, then check that channel.
    fn choose_channel(&mut self, beta: bool, cx: &mut Context<Self>) {
        let Some(data_dir) = self.data_dir.clone() else {
            cx.emit(Event::Error(
                "Onicode has no data folder to keep the choice in".into(),
            ));
            return;
        };
        let work = cx
            .background_executor()
            .spawn(async move { desktop::choose_channel(&data_dir, beta) });
        cx.spawn(async move |this, cx| {
            let chosen = work.await;
            let _ = this.update(cx, |this, cx| match chosen {
                Ok(chosen) => {
                    this.channel = chosen;
                    this.changed(cx);
                    this.check(false, cx);
                }
                Err(why) => {
                    cx.emit(Event::Error(why));
                    this.changed(cx);
                }
            });
        })
        .detach();
    }

    /// `version`'s release notes: the last check's manifest's, or the feed's now.
    pub(crate) fn release_notes(
        &self,
        version: &str,
        cx: &App,
    ) -> gpui::Task<Option<ReleaseNotes>> {
        if let Some(notes) = self
            .manifest
            .as_ref()
            .and_then(|manifest| ReleaseNotes::of(manifest, version))
        {
            return gpui::Task::ready(Some(notes));
        }
        let Some(feed) = self.feed.clone() else {
            return gpui::Task::ready(None);
        };
        let version = version.to_owned();
        cx.background_executor().spawn(async move {
            let manifest = block_on(onicode_update::fetch_latest(&feed)).ok()?.ok()?;
            ReleaseNotes::of(&manifest, &version)
        })
    }
}
/// What the updater offers right now, for Settings' About button.
pub(crate) enum Action {
    Check,
    Install(String),
    Busy(String),
}
pub(crate) fn action(entity: &Entity<Updater>, cx: &App) -> Action {
    let state = &entity.read(cx).machine;
    match state.phase {
        Phase::Available => Action::Install(state.pending.clone().unwrap_or_default()),
        Phase::Checking => Action::Busy("Checking…".into()),
        Phase::Downloading | Phase::Installing => Action::Busy(state.progress_label()),
        Phase::Disabled | Phase::Idle | Phase::Current => Action::Check,
    }
}
/// A line for Settings' About description: the last check's outcome, if any.
pub(crate) fn status(entity: &Entity<Updater>, cx: &App) -> Option<String> {
    let state = &entity.read(cx).machine;
    match state.phase {
        Phase::Current => Some(
            state
                .message
                .clone()
                .unwrap_or_else(|| "You’re on the latest version.".into()),
        ),
        Phase::Checking => None,
        _ => state.message.clone(),
    }
}
pub(crate) fn install(entity: &Entity<Updater>, cx: &mut App) {
    entity.update(cx, |updater, cx| updater.install(cx));
}
/// The channel the app follows, for Settings' Channel row.
pub(crate) fn followed_channel(entity: &Entity<Updater>, cx: &App) -> Channel {
    entity.read(cx).channel
}
pub(crate) fn choose_channel(entity: &Entity<Updater>, beta: bool, cx: &mut App) {
    entity.update(cx, |updater, cx| updater.choose_channel(beta, cx));
}
/// The sidebar's update card, shown while an update is available or installing.
pub(crate) fn row(entity: Entity<Updater>, cx: &App) -> gpui::AnyElement {
    let action = entity.clone();
    let state = &entity.read(cx).machine;
    let available = state.phase == Phase::Available;
    // Matches the desktop footer: one line, accent-tinted until it starts.
    let label = if available {
        format!(
            "Update to v{}",
            state.pending.as_deref().unwrap_or_default()
        )
    } else {
        state.progress_label()
    };
    let glyph = if state.busy() {
        crate::pulse::looping(
            "native-update-spin",
            Duration::from_secs(1),
            crate::pulse::Rate::Smooth,
            |turn| {
                icon("loader", 16.)
                    .text_color(content(0.7))
                    .with_transformation(gpui::Transformation::rotate(gpui::percentage(turn)))
            },
        )
        .into_any_element()
    } else {
        icon("arrow-down-circle", 16.)
            .text_color(accent_color())
            .into_any_element()
    };
    let mut button = div()
        .id("install-native-update")
        .role(Role::Button)
        .rounded(px(8.))
        .px_2()
        .py_2()
        .flex()
        .items_center()
        .gap_2()
        .bg(if available {
            accent_color().opacity(0.15)
        } else {
            content(0.05)
        })
        .text_color(content(if available { 1. } else { 0.75 }))
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(glyph),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(label),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.))
                        .text_color(content(0.4))
                        .child(format!("v{}", onicode_update::app_version())),
                ),
        );
    if available {
        button = button
            .aria_label("Install the available Onicode update")
            .cursor_pointer()
            .hover(|s| s.bg(accent_color().opacity(0.2)))
            .on_click(move |_, _, cx| action.update(cx, |updater, cx| updater.install(cx)));
    }
    div()
        .id("native-app-update")
        .role(Role::Status)
        .w_full()
        .min_w_0()
        .child(button)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn quit_waits_for_installation_to_finish(cx: &mut TestAppContext) {
        let updater = cx.update(shared);
        let gate = updater.read_with(cx, |updater, _| updater.gate.clone());
        gate.lock().unwrap().installing = true;
        assert!(cx.update(|cx| defer_quit(cx)));
        assert!(gate.lock().unwrap().quitting);
        gate.lock().unwrap().installing = false;
        assert!(!cx.update(|cx| defer_quit(cx)));
    }

    #[gpui::test]
    fn windows_share_one_updater_and_busy_operations_cannot_start_again(cx: &mut TestAppContext) {
        let first = cx.update(shared);
        let second = cx.update(shared);
        assert_eq!(first.entity_id(), second.entity_id());
        first.update(cx, |view, cx| {
            // Busy work needs a feed to get past the disabled gate.
            view.feed = Some("https://feed.invalid".into());
            assert!(!view.actionable());
            for phase in [Phase::Downloading, Phase::Installing] {
                view.machine.phase = phase;
                view.machine.pending = Some("9.9.9".into());
                view.machine.progress = Some(42);
                view.install(cx);
                view.check(true, cx);
                assert_eq!(view.machine.phase, phase);
                assert_eq!(view.machine.progress, Some(42));
                assert!(view.actionable());
            }
            view.machine.phase = Phase::Current;
            assert!(!view.actionable());
        });
    }

    /// The row's whole life: idle, checking, offered, downloading with its
    /// percent, installing; a failed install keeps the offer.
    #[test]
    fn the_update_row_goes_from_idle_to_installing() {
        let mut row = Machine::default();
        assert_eq!(row.phase, Phase::Idle);
        assert!(!row.actionable());
        assert!(row.begin_check());
        assert_eq!(row.phase, Phase::Checking);
        assert!(!row.begin_check(), "one check at a time");
        row.checked(Ok((Offer::Newer, "0.2.0".into())));
        assert_eq!(row.phase, Phase::Available);
        assert_eq!(row.pending.as_deref(), Some("0.2.0"));
        assert!(row.actionable());

        assert!(row.begin_install());
        assert_eq!((row.phase, row.progress), (Phase::Downloading, Some(0)));
        assert!(!row.begin_install(), "one install at a time");
        assert!(!row.begin_check(), "no check while downloading");
        row.progressed(Progress::Download(512, Some(1024)));
        assert_eq!(row.progress_label(), "Downloading 50%");
        row.progressed(Progress::Download(4096, None));
        assert_eq!(row.progress_label(), "Downloading…");
        row.progressed(Progress::Installing);
        assert_eq!(row.phase, Phase::Installing);
        assert_eq!(row.progress_label(), "Installing…");

        row.install_failed("disk full".into());
        assert_eq!(row.phase, Phase::Available);
        assert_eq!(row.message.as_deref(), Some("disk full"));
        assert!(row.begin_install(), "the offer stands after a failure");
    }

    #[test]
    fn a_check_settles_on_current_held_or_a_kept_offer() {
        let mut row = Machine::default();
        row.begin_check();
        row.checked(Ok((Offer::Current, "0.1.0".into())));
        assert_eq!((row.phase, row.pending.as_deref()), (Phase::Current, None));
        assert!(row.message.is_none());

        // Held: nothing to install, and why.
        row.begin_check();
        row.checked(Ok((Offer::Held("needs macOS 28.0".into()), "0.2.0".into())));
        assert_eq!(row.phase, Phase::Current);
        assert_eq!(row.message.as_deref(), Some("needs macOS 28.0"));
        assert!(!row.begin_install());

        // Leaving beta for an older stable is still an offer.
        row.begin_check();
        row.checked(Ok((Offer::Switch, "0.1.9".into())));
        assert_eq!(row.phase, Phase::Available);

        // Offline: the offer found before stays; without one the row is idle.
        row.begin_check();
        row.checked(Err("offline".into()));
        assert_eq!(row.phase, Phase::Available);
        assert_eq!(row.message.as_deref(), Some("offline"));
        let mut fresh = Machine::default();
        fresh.begin_check();
        fresh.checked(Err("offline".into()));
        assert_eq!(fresh.phase, Phase::Idle);
    }

    #[test]
    fn without_a_feed_the_row_never_checks() {
        let mut row = Machine::disabled("no feed");
        assert!(!row.begin_check());
        assert!(!row.begin_install());
        assert_eq!(row.phase, Phase::Disabled);
    }

    #[test]
    fn release_notes_come_from_the_running_version_s_manifest() {
        let manifest = Manifest {
            version: "0.2.0".into(),
            notes: Some("- Faster.\n".into()),
            date: Some("2026-11-07".into()),
            ..Manifest::default()
        };
        let notes = ReleaseNotes::of(&manifest, "0.2.0").unwrap();
        assert_eq!(notes.markdown, "- Faster.");
        assert_eq!(notes.date.as_deref(), Some("2026-11-07"));
        assert!(ReleaseNotes::of(&manifest, "0.1.0").is_none());
        let silent = Manifest {
            notes: Some("  ".into()),
            ..manifest
        };
        assert!(ReleaseNotes::of(&silent, "0.2.0").is_none());
    }
}
