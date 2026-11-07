//! The post-update card (updated, or rolled back) and What's new. The updater
//! itself is `updater.rs`; an install records the marker this card reads.
use std::time::Instant;

use gpui::prelude::*;
use gpui::{Context, EventEmitter, FontWeight, Render, Role, Window, div, img, px};
use onicode_update::{UpdateReport, UpdateState};

use super::updater::{INSTALLED_MARKER, ReleaseNotes};
use crate::markdown::{self, MarkdownRow, Tone};
use crate::markdown_model;
use crate::theme::*;
use crate::ui_state::UiState;
/// The id of the last update record (`onicode_update::transaction`) shown.
const REPORT_SEEN: &str = "onicode.updateReportSeen";

/// What the card says once, at launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Notice {
    /// Running the version an update installed: the card opens What's new.
    Updated(String),
    /// An update was tried and didn't hold: `version` is what runs again.
    RolledBack {
        version: String,
        reason: Option<String>,
    },
}

/// The marker names a version; only that version running proves the install
/// worked. Cleared whatever it says (an empty value is cleared).
fn consume_marker(ui_state: &UiState, running: &str) -> Option<String> {
    let raw = ui_state.get(INSTALLED_MARKER)?;
    if raw.is_empty() {
        return None;
    }
    let parsed = serde_json::from_str::<serde_json::Value>(&raw).ok();
    let version = parsed
        .as_ref()
        .and_then(|v| v["version"].as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    // A cancelled/failed installer must never claim that the app was updated.
    if version.as_deref() != Some(running) {
        return None;
    }
    ui_state.set(INSTALLED_MARKER, String::new());
    version
}

/// The card to show once at launch: a rollback the update record holds, an
/// install this app made (its marker names the running version), or one made
/// from another device. A record is shown once, by its id.
pub(crate) fn consume_startup_notice(
    ui_state: &UiState,
    report: Option<&UpdateReport>,
    running: &str,
) -> Option<Notice> {
    let installed = consume_marker(ui_state, running);
    let report = match report {
        Some(report)
            if report.state != UpdateState::InProgress
                && ui_state.get(REPORT_SEEN).as_deref() != Some(report.id.as_str()) =>
        {
            ui_state.set(REPORT_SEEN, report.id.clone());
            Some(report)
        }
        _ => None,
    };
    let from_report = report.and_then(|report| match report.state {
        UpdateState::RolledBack => Some(Notice::RolledBack {
            version: report.from_version.clone(),
            reason: report.reason.clone(),
        }),
        UpdateState::Updated if report.target_version == running => {
            Some(Notice::Updated(report.target_version.clone()))
        }
        _ => None,
    });
    from_report.or(installed.map(Notice::Updated))
}
pub(crate) enum AppUpdateEvent {
    Open(String),
    Dismiss,
}
pub(crate) struct InstalledUpdateNotice {
    notice: Option<Notice>,
}
impl EventEmitter<AppUpdateEvent> for InstalledUpdateNotice {}
impl InstalledUpdateNotice {
    pub(crate) fn new(notice: Option<Notice>) -> Self {
        Self { notice }
    }

    pub(crate) fn set_notice(&mut self, notice: Option<Notice>, cx: &mut Context<Self>) {
        if self.notice != notice {
            self.notice = notice;
            cx.notify();
        }
    }

    pub(crate) fn is_visible(&self) -> bool {
        self.notice.is_some()
    }
}
impl Render for InstalledUpdateNotice {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(notice) = self.notice.clone() else {
            return div().into_any_element();
        };
        // Updated opens What's new; a rollback has nothing new to show.
        let (label, detail, opens) = match notice {
            Notice::Updated(version) => (
                format!("Updated to {version}"),
                "What's new".to_owned(),
                Some(version),
            ),
            Notice::RolledBack { version, reason } => (
                format!("Rolled back to {version}"),
                reason.unwrap_or_else(|| {
                    "The new version didn't start, so Onicode went back.".into()
                }),
                None,
            ),
        };
        div()
            .id("installed-update-notice")
            .role(Role::Status)
            .relative()
            .overflow_hidden()
            .rounded(px(8.))
            .bg(content(0.12))
            .child(
                div()
                    .id("open-installed-update")
                    .role(Role::Button)
                    .aria_label(label.clone())
                    .debug_selector(|| "open-installed-update".into())
                    .flex()
                    .w_full()
                    .items_start()
                    .gap(px(8.))
                    .rounded(px(8.))
                    .p(px(8.))
                    .pr(px(32.))
                    .when_some(opens, |d, version| {
                        d.cursor_pointer()
                            .hover(|s| {
                                let mut accent = accent_color();
                                accent.a = 0.1;
                                s.bg(accent)
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(AppUpdateEvent::Open(version.clone()))
                            }))
                    })
                    .child(
                        div()
                            .mt(px(2.))
                            .size(px(18.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(img("monocode.png").size(px(16.))),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .mt(px(2.))
                                    .text_size(px(11.))
                                    .text_color(content(0.5))
                                    .child(detail),
                            ),
                    ),
            )
            .child(
                div()
                    .id("dismiss-installed-update")
                    .role(Role::Button)
                    .aria_label("Dismiss update notification")
                    .debug_selector(|| "dismiss-installed-update".into())
                    // The X sits over the clickable card; keep its click from opening What's new.
                    .block_mouse_except_scroll()
                    .absolute()
                    .right(px(4.))
                    .top(px(4.))
                    .size(px(24.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(content(0.45))
                    .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AppUpdateEvent::Dismiss)))
                    .child(icon("x", 14.)),
            )
            .into_any_element()
    }
}
/// `2026-11-07` → `7 Nov 2026`; anything else as it came.
fn format_date(iso: &str) -> String {
    let well_formed = iso.len() == 10
        && iso.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if !well_formed {
        return iso.into();
    }
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = iso[5..7]
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|n| months.get(n));
    match month {
        Some(month) => format!(
            "{} {month} {}",
            iso[8..10].parse::<u32>().unwrap_or(0),
            &iso[..4]
        ),
        None => iso.into(),
    }
}
pub(crate) enum WhatsNewEvent {
    Close,
}
pub(crate) struct WhatsNew {
    version: String,
    date: Option<String>,
    rows: Vec<MarkdownRow>,
}
impl EventEmitter<WhatsNewEvent> for WhatsNew {}
impl WhatsNew {
    /// Empty until the update manifest's notes arrive.
    pub(crate) fn new(version: String) -> Self {
        Self {
            version,
            date: None,
            rows: Vec::new(),
        }
    }

    pub(crate) fn set_release_notes(&mut self, notes: &ReleaseNotes, cx: &mut Context<Self>) {
        self.date = notes.date.as_deref().map(format_date);
        self.rows = markdown_model::parse_full(&notes.markdown)
            .blocks
            .iter()
            .flat_map(|b| markdown::rows(&b.block))
            .collect();
        cx.notify();
    }
}

impl Render for WhatsNew {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let description = format!(
            "Onicode {}{}",
            self.version,
            self.date
                .as_ref()
                .map(|d| format!(" · {d}"))
                .unwrap_or_default()
        );
        let mut body = div()
            .id("whats-new-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(16.));
        if self.rows.is_empty() {
            body = body.child(
                div()
                    .text_size(px(13.))
                    .text_color(content(0.6))
                    .child("Release notes for this version are not available."),
            );
        }
        for (ix, row) in self.rows.iter().enumerate() {
            body = body.child(div().pt(px(if ix == 0 { 0. } else { row.gap })).child(
                markdown::body(
                    row,
                    format!("whats-new-row-{ix}").into(),
                    Tone::Normal,
                    None,
                    Instant::now(),
                ),
            ));
        }
        div()
            .id("whats-new-dialog")
            .role(Role::Dialog)
            .aria_label("What's new")
            .relative()
            .w(px(512.))
            .max_w_full()
            .h((window.viewport_size().height * 0.72).min(px(640.)))
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(12.))
            .border_1()
            .border_color(content(0.1))
            .text_color(content(1.))
            .shadow_lg()
            .child(super::surface::backdrop(12.))
            .child(
                div()
                    .relative()
                    .px(px(20.))
                    .py(px(16.))
                    .border_b_1()
                    .border_color(content(0.1))
                    .flex()
                    .items_start()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .child(
                                div()
                                    .text_size(px(16.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("What's new"),
                            )
                            .child(
                                div()
                                    .mt(px(4.))
                                    .text_size(px(12.))
                                    .text_color(content(0.6))
                                    .child(description),
                            ),
                    )
                    .child(
                        div()
                            .id("close-whats-new")
                            .role(Role::Button)
                            .aria_label("Close")
                            .cursor_pointer()
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(WhatsNewEvent::Close)))
                            .child(icon("x", 14.)),
                    ),
            )
            .child(body.relative())
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(id: &str, state: UpdateState) -> UpdateReport {
        UpdateReport {
            id: id.into(),
            from_version: "0.1.0".into(),
            target_version: "0.2.0".into(),
            state,
            reason: Some("The new version stopped while starting.".into()),
            finished_at: Some(1),
        }
    }

    #[test]
    fn installed_notice_requires_the_running_version_and_is_consumed_once() {
        let ui_state = UiState::memory();
        ui_state.set(INSTALLED_MARKER, r#"{"version":"99.0.0-preview.1"}"#.into());
        assert!(consume_startup_notice(&ui_state, None, "0.2.0").is_none());
        ui_state.set(INSTALLED_MARKER, r#"{"version":"0.2.0"}"#.into());
        assert_eq!(
            consume_startup_notice(&ui_state, None, "0.2.0"),
            Some(Notice::Updated("0.2.0".into()))
        );
        assert!(consume_startup_notice(&ui_state, None, "0.2.0").is_none());
    }

    /// After the row's install: updated (What's new), or a trial that went
    /// back, each said once.
    #[test]
    fn the_update_record_says_updated_or_rolled_back_once() {
        let ui_state = UiState::memory();
        // Still trying it out: nothing yet, and not marked seen.
        let trying = report("a", UpdateState::InProgress);
        assert!(consume_startup_notice(&ui_state, Some(&trying), "0.1.0").is_none());

        let back = report("a", UpdateState::RolledBack);
        assert_eq!(
            consume_startup_notice(&ui_state, Some(&back), "0.1.0"),
            Some(Notice::RolledBack {
                version: "0.1.0".into(),
                reason: Some("The new version stopped while starting.".into()),
            })
        );
        assert!(consume_startup_notice(&ui_state, Some(&back), "0.1.0").is_none());

        // An update from another device that held, seen by the version it installed.
        let held = report("b", UpdateState::Updated);
        assert!(consume_startup_notice(&ui_state, Some(&held), "0.1.0").is_none());
        let again = report("c", UpdateState::Updated);
        assert_eq!(
            consume_startup_notice(&ui_state, Some(&again), "0.2.0"),
            Some(Notice::Updated("0.2.0".into()))
        );
    }

    #[test]
    fn a_rollback_outranks_a_stale_install_marker() {
        let ui_state = UiState::memory();
        ui_state.set(INSTALLED_MARKER, r#"{"version":"0.1.0"}"#.into());
        let back = report("a", UpdateState::RolledBack);
        assert!(matches!(
            consume_startup_notice(&ui_state, Some(&back), "0.1.0"),
            Some(Notice::RolledBack { .. })
        ));
        // The marker was spent with it.
        assert!(consume_startup_notice(&ui_state, None, "0.1.0").is_none());
    }

    #[test]
    fn release_dates_read_as_day_month_year() {
        assert_eq!(format_date("2026-11-07"), "7 Nov 2026");
        assert_eq!(format_date("2026-13-07"), "2026-13-07");
        assert_eq!(format_date("soon"), "soon");
        assert_eq!(format_date("２０２６-11-07"), "２０２６-11-07");
    }
}
