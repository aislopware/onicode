//! The permissions checklist (`docs/onicode/decisions/onboarding-grants.md`): every privilege
//! Onicode needs, asked once, in a window of its own.
//!
//! It opens at launch the first time, again after a "Quit & Reopen" it asked for, and when a
//! grant seen on before is off ([`at_launch`]); Settings → General → Permissions reopens it
//! ([`open_window`]). Rows are grouped as the decision tables them, each with its reason, its
//! live status and one button. Status is read every second while the window shows and when it
//! is activated, off the main thread and never prompting; a prompt comes only from a click.
//! What the person did is kept in `permissions.json` beside `ui-settings.json`.
//!
//! The Mac is behind `Platform`: the app's is `mac::Mac`, tests record.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    AnyElement, App, Bounds, Context, Entity, SharedString, Subscription, Task, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, px, size,
};
use onicode_grants::{Action, Checklist, Group, Install, Privilege, Progress, Relaunch, Status};

use crate::icons::{self, icon};
use crate::settings::widgets::{self, ActionTone};
use crate::state::AppState;
use crate::theme::Theme;

/// How often the rows are read while the window shows.
const POLL: Duration = Duration::from_secs(1);
/// The kept progress, beside `ui-settings.json`.
const FILE: &str = "permissions.json";
/// `ONICODE_PERMISSIONS=0` keeps the checklist from opening at launch (smoke runs).
const OFF_ENV: &str = "ONICODE_PERMISSIONS";

/// What one read covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReadPlan {
    /// Rows whose alert was raised: the folders may be opened for these, since opening them no
    /// longer prompts.
    pub asked: BTreeSet<Privilege>,
    /// Start the helper to read its rows: one is not on yet, or nothing was read.
    pub helper: bool,
}

/// What the checklist asks of the Mac. A seam: the app's is `mac::Mac`, tests record.
pub(crate) trait Platform: Send + Sync {
    /// Where the app runs from.
    fn install(&self) -> Install;
    /// Every row it can read, never prompting. Blocking: runs off the main thread.
    fn read(&self, plan: &ReadPlan) -> Vec<(Privilege, Status)>;
    /// "Allow…": raises the row's alert and waits for it. Blocking: runs off the main thread.
    fn allow(&self, privilege: Privilege);
    /// "Open Settings": the row's list, with the drag panel where the list takes a drag. Main
    /// thread.
    fn open_settings(&self, privilege: Privilege);
    /// The latest reading of a row, for the drag panel when it is open for that row.
    fn report(&self, privilege: Privilege, granted: bool);
    /// The row whose drag panel ended on "granted" since the last call.
    fn guide_granted(&self) -> Option<Privilege>;
    /// Arranges for the app to start again once it quits; `false` when it cannot.
    fn quit_and_reopen(&self) -> bool;
}

/// Where the kept progress lives.
fn progress_path(cx: &App) -> Option<PathBuf> {
    crate::settings::data_dir(cx).map(|dir| dir.join(FILE))
}

/// The checklist view.
pub struct Permissions {
    platform: Arc<dyn Platform>,
    checklist: Checklist,
    install: Install,
    path: Option<PathBuf>,
    state: Option<Entity<AppState>>,
    /// The read in flight.
    reading: Option<Task<()>>,
    /// Something has been read.
    read_once: bool,
    /// The row whose alert is up.
    asking: Option<Privilege>,
    _ask: Option<Task<()>>,
    _poll: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Permissions {
    pub(crate) fn new(
        platform: Arc<dyn Platform>,
        progress: Progress,
        path: Option<PathBuf>,
        state: Option<Entity<AppState>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut checklist = Checklist::new(progress);
        checklist.start();
        let install = platform.install();
        let poll = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let polled = this.update_in(cx, |this, window, cx| {
                    if window.visibility().is_visible() {
                        this.poll(cx);
                    }
                });
                if polled.is_err() {
                    break;
                }
            }
        });
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.poll(cx);
            }
        });
        let mut view = Self {
            platform,
            checklist,
            install,
            path,
            state,
            reading: None,
            read_once: false,
            asking: None,
            _ask: None,
            _poll: Some(poll),
            _subscriptions: vec![activation],
        };
        // A resume was taken from the progress: keep that it was.
        view.save();
        view.poll(cx);
        view
    }

    /// The checklist as it stands.
    #[cfg(test)]
    pub(crate) const fn checklist(&self) -> &Checklist {
        &self.checklist
    }

    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        if let Err(error) = self.checklist.progress().save(path) {
            tracing::warn!(%error, "the permissions checklist was not saved");
        }
    }

    /// Reads every row off the main thread, unless a read is in flight.
    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.reading.is_some() || !self.install.settled() {
            return;
        }
        let checklist = &self.checklist;
        let plan = ReadPlan {
            asked: checklist.progress().asked.clone(),
            helper: !self.read_once
                || Privilege::HELPER.iter().any(|&privilege| {
                    checklist.shown(privilege) && checklist.status(privilege) != Status::Granted
                }),
        };
        let platform = self.platform.clone();
        self.reading = Some(cx.spawn(async move |this, cx| {
            let reads = cx
                .background_executor()
                .spawn(async move { platform.read(&plan) })
                .await;
            let _gone = this.update(cx, |this, cx| {
                this.reading = None;
                this.take(reads, cx);
            });
        }));
    }

    fn take(&mut self, reads: Vec<(Privilege, Status)>, cx: &mut Context<Self>) {
        let mut changed = !self.read_once;
        self.read_once = true;
        for (privilege, status) in reads {
            changed |= self.checklist.read(privilege, status);
        }
        if let Some(privilege) = self.platform.guide_granted() {
            self.checklist.confirm(privilege);
            changed = true;
        }
        for privilege in self.checklist.rows() {
            self.platform.report(
                privilege,
                self.checklist.status(privilege) == Status::Granted,
            );
        }
        if changed {
            self.save();
            cx.notify();
        }
    }

    fn act(&mut self, privilege: Privilege, action: Action, cx: &mut Context<Self>) {
        match action {
            Action::Allow => self.allow(privilege, cx),
            Action::OpenSettings => {
                self.checklist.opened_settings(privilege);
                self.platform.open_settings(privilege);
                if privilege.relaunch() == Relaunch::Helper {
                    self.helper_watches(cx);
                }
                self.save();
                cx.notify();
            }
            Action::Confirm => {
                self.checklist.confirm(privilege);
                self.save();
                cx.notify();
            }
        }
    }

    /// Raises the row's alert off the main thread; the row reads "Asking…" until it is answered.
    fn allow(&mut self, privilege: Privilege, cx: &mut Context<Self>) {
        if self.asking.is_some() {
            return;
        }
        self.asking = Some(privilege);
        let platform = self.platform.clone();
        self._ask = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .spawn(async move { platform.allow(privilege) })
                .await;
            let _gone = this.update(cx, |this, cx| {
                this.asking = None;
                this.checklist.allowed(privilege);
                this.save();
                this.poll(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn later(&mut self, privilege: Privilege, cx: &mut Context<Self>) {
        self.checklist.later(privilege);
        self.save();
        cx.notify();
    }

    /// "Continue": done, rows left or not. From a disk image nothing was asked, so nothing is
    /// marked done.
    fn finish(&mut self, window: &mut Window) {
        if self.install.settled() {
            self.checklist.finish();
            self.save();
        }
        window.remove_window();
    }

    fn quit_and_reopen(&mut self, cx: &mut Context<Self>) {
        if !self.platform.quit_and_reopen() {
            return;
        }
        self.checklist.relaunching();
        self.save();
        cx.quit();
    }

    /// Screen Recording reads stale in a running desktopd: tell it the guide is open, so it
    /// restarts itself once a new process sees the grant (`restart_when_granted`). Fire and
    /// forget; a desktopd not running yet starts with the grant.
    #[cfg(target_os = "macos")]
    fn helper_watches(&self, cx: &mut Context<Self>) {
        use onicode_desktop_link::Pane;
        use onicode_desktop_link::broker::OpenGuideParams;
        let Some(engine) = self
            .state
            .as_ref()
            .and_then(|state| state.read(cx).engine().cloned())
        else {
            return;
        };
        let params = serde_json::to_value(OpenGuideParams {
            pane: Pane::ScreenRecording,
        })
        .unwrap_or_default();
        cx.spawn(async move |_, _| {
            let told = engine
                .client()
                .call(onicode_rpc::methods::DESKTOP_OPEN_GUIDE, params)
                .await;
            if let Err(error) = told {
                tracing::debug!(%error, "desktopd did not hear the guide open");
            }
        })
        .detach();
    }

    #[cfg(not(target_os = "macos"))]
    fn helper_watches(&self, _cx: &mut Context<Self>) {
        let _no_desktopd = self.state.as_ref();
    }
}

// ---- rendering ----

/// What a row's status says.
fn status_line(status: Status, skipped: bool, asking: bool) -> &'static str {
    match status {
        _ if asking => "Asking…",
        Status::Granted => "On",
        Status::Unknown | Status::Unavailable => "Checking…",
        _ if skipped => "Later",
        Status::NotAsked | Status::Off => "Not set",
        Status::Denied => "Off",
    }
}

fn button(
    theme: &Theme,
    id: String,
    tone: ActionTone,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    let selector = id.clone();
    widgets::text_action(theme, tone, label)
        .id(SharedString::from(id))
        .debug_selector(move || selector.clone())
        .tab_index(0)
        .role(gpui::Role::Button)
        .aria_label(label)
        .focus_visible(|s| s.border_2().border_color(theme.accent))
}

impl Permissions {
    fn render_row(
        &self,
        theme: &Theme,
        privilege: Privilege,
        first: bool,
        relaunch: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = privilege.key();
        let status = self.checklist.status(privilege);
        let skipped = self.checklist.skipped(privilege);
        let asking = self.asking == Some(privilege);
        let next = self.checklist.next() == Some(privilege);
        let on = status == Status::Granted;
        let mark = div()
            .flex_none()
            .size(px(20.0))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            .map(|mark| {
                if on {
                    mark.bg(theme.success.opacity(0.16))
                        .child(icon(icons::CHECK).size(px(12.0)).text_color(theme.success))
                } else if status == Status::Denied {
                    mark.bg(theme.warning.opacity(0.14)).child(
                        icon(icons::DANGER_TRIANGLE)
                            .size(px(11.0))
                            .text_color(theme.warning),
                    )
                } else {
                    mark.border_1().border_color(if next {
                        theme.accent
                    } else {
                        theme.border_strong
                    })
                }
            });
        let text = div()
            .flex_1()
            .min_w(px(180.0))
            .flex()
            .flex_col()
            .child(widgets::row_title(theme, privilege.title()))
            .child(widgets::meta_line(
                theme,
                vec![
                    div()
                        .child(SharedString::from(privilege.reason()))
                        .into_any_element(),
                ],
            ));
        let line = div()
            .debug_selector(move || format!("permissions-{key}-status"))
            .flex_none()
            .text_size(crate::typography::ui_rems(12.0))
            .text_color(if on { theme.success } else { theme.text_muted })
            .child(SharedString::from(status_line(status, skipped, asking)));
        let mut controls = div()
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0));
        if relaunch {
            controls = controls.child(
                button(
                    theme,
                    format!("permissions-{key}-reopen"),
                    ActionTone::Filled,
                    "Quit & Reopen",
                )
                .on_click(cx.listener(|this, _, _, cx| this.quit_and_reopen(cx))),
            );
        }
        if let Some(action) = self.checklist.action(privilege).filter(|_| !asking) {
            if !skipped {
                controls = controls.child(
                    button(
                        theme,
                        format!("permissions-{key}-later"),
                        ActionTone::Quiet,
                        "Later",
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.later(privilege, cx))),
                );
            }
            let tone = if next {
                ActionTone::Solid
            } else {
                ActionTone::Filled
            };
            controls = controls.child(
                button(
                    theme,
                    format!("permissions-{key}-action"),
                    tone,
                    action.label(),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.act(privilege, action, cx))),
            );
        }
        widgets::card_row(theme, first)
            .debug_selector(move || format!("permissions-{key}"))
            .flex_nowrap()
            .gap(px(12.0))
            .child(mark)
            .child(text)
            .child(line)
            .child(controls)
            .into_any_element()
    }

    fn render_groups(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let relaunch = self.checklist.relaunch_for();
        Group::ALL
            .into_iter()
            .filter_map(|group| {
                let rows: Vec<Privilege> = self
                    .checklist
                    .rows()
                    .filter(|privilege| privilege.group() == group)
                    .collect();
                if rows.is_empty() {
                    return None;
                }
                let card =
                    widgets::section_card(theme)
                        .mt_0()
                        .children(rows.iter().enumerate().map(|(ix, &privilege)| {
                            self.render_row(
                                theme,
                                privilege,
                                ix == 0,
                                relaunch.contains(&privilege),
                                cx,
                            )
                        }));
                Some(widgets::section(theme, group.title(), card).into_any_element())
            })
            .collect()
    }

    fn render_refusal(theme: &Theme, refusal: &'static str) -> AnyElement {
        div()
            .debug_selector(|| "permissions-refusal".to_owned())
            .mt(px(32.0))
            .flex()
            .flex_col()
            .items_center()
            .text_center()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(15.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(SharedString::from(refusal)),
            )
            .child(
                div()
                    .max_w(px(420.0))
                    .text_size(crate::typography::ui_rems(13.0))
                    .text_color(theme.text_muted)
                    .child(SharedString::from(
                        "Onicode is running from a disk image or a temporary copy, and permissions \
                         given now would stay with that copy. Drag Onicode into Applications, open \
                         it from there, and this list comes back.",
                    )),
            )
            .into_any_element()
    }
}

impl Render for Permissions {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx).for_settings_surface();
        let body: Vec<AnyElement> = match self.install.refusal() {
            Some(refusal) => vec![Self::render_refusal(&theme, refusal)],
            None => self.render_groups(&theme, cx),
        };
        let left = self
            .checklist
            .rows()
            .filter(|&p| self.checklist.status(p) != Status::Granted)
            .count();
        let subtitle = if left == 0 && self.read_once {
            "Everything Onicode uses is on.".to_owned()
        } else {
            "Each is asked once, here, so nothing stops an agent or a remote session later. \
             Skip any with Later; Settings → General → Permissions brings this back."
                .to_owned()
        };
        let footer = div()
            .flex_none()
            .px(px(40.0))
            .py(px(16.0))
            .border_t_1()
            .border_color(widgets::row_divider(&theme))
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap(px(8.0))
            .when(!self.checklist.relaunch_for().is_empty(), |footer| {
                footer.child(
                    button(
                        &theme,
                        "permissions-reopen".to_owned(),
                        ActionTone::Filled,
                        "Quit & Reopen",
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.quit_and_reopen(cx))),
                )
            })
            .child(
                button(
                    &theme,
                    "permissions-continue".to_owned(),
                    ActionTone::Solid,
                    "Continue",
                )
                .on_click(cx.listener(|this, _, window, _| this.finish(window))),
            );
        div()
            .id("permissions")
            .size_full()
            .bg(theme.surface)
            .text_color(theme.text)
            .flex()
            .flex_col()
            .child(
                div()
                    .id("permissions-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        widgets::page_column()
                            .child(widgets::page_header(&theme, "Permissions", None))
                            .child(widgets::page_subtitle(&theme, subtitle))
                            .children(body),
                    ),
            )
            .child(footer)
    }
}

// ---- opening ----

/// The platform this build has: the Mac's, or none.
fn platform() -> Option<Arc<dyn Platform>> {
    #[cfg(target_os = "macos")]
    {
        Some(Arc::new(mac::Mac::new()))
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Opens the checklist window, or brings it forward when it is open.
pub fn open_window(state: Option<Entity<AppState>>, cx: &mut App) {
    let open = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<Permissions>());
    if let Some(open) = open {
        let _shown = open.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let Some(platform) = platform() else {
        return;
    };
    let path = progress_path(cx);
    let progress = path.as_deref().and_then(Progress::load).unwrap_or_default();
    open_with(platform, progress, path, state, cx);
}

fn open_with(
    platform: Arc<dyn Platform>,
    progress: Progress,
    path: Option<PathBuf>,
    state: Option<Entity<AppState>>,
    cx: &mut App,
) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(720.0), px(760.0)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Permissions".into()),
            appears_transparent: true,
            ..TitlebarOptions::default()
        }),
        window_min_size: Some(size(px(560.0), px(480.0))),
        window_background: Theme::of(cx).window_background_appearance(),
        ..WindowOptions::default()
    };
    let opened = cx.open_window(options, move |window, cx| {
        cx.new(|cx| Permissions::new(platform, progress, path, state, window, cx))
    });
    if let Err(error) = opened {
        tracing::warn!(%error, "no window for the permissions checklist");
    }
}

/// At launch: opens the checklist on a first launch, an unfinished checklist, a relaunch it
/// asked for, or a grant seen on before that is off now. The last is read off the main thread;
/// nothing prompts.
pub fn at_launch(state: Entity<AppState>, cx: &mut App) {
    if std::env::var_os(OFF_ENV).is_some_and(|value| value == "0") {
        return;
    }
    let Some(platform) = platform() else {
        return;
    };
    let path = progress_path(cx);
    let progress = path.as_deref().and_then(Progress::load);
    if Progress::opens_at_launch(progress.as_ref(), false) {
        open_with(
            platform,
            progress.unwrap_or_default(),
            path,
            Some(state),
            cx,
        );
        return;
    }
    let Some(progress) = progress else {
        return;
    };
    cx.spawn(async move |cx| {
        let plan = ReadPlan {
            asked: progress.asked.clone(),
            helper: true,
        };
        let reader = platform.clone();
        let reads = cx
            .background_executor()
            .spawn(async move { reader.read(&plan) })
            .await;
        let mut checklist = Checklist::new(progress.clone());
        for (privilege, status) in reads {
            checklist.read(privilege, status);
        }
        if !checklist.revoked().is_empty() {
            cx.update(|cx| open_with(platform, progress, path, Some(state), cx));
        }
    })
    .detach();
}
