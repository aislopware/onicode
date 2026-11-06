//! The transfers tray: every job on every engine this window watches, with its
//! progress and controls, and a conflict's question when one waits.
//!
//! It is its own view, so the updates a running job sends every 100 ms redraw
//! the tray alone, not the listing above it.

use std::collections::{HashMap, HashSet};

use chrono::Local;
use gpui::prelude::*;
use gpui::{Context, Entity, MouseButton, SharedString, Task, Window, div, px, relative};
use onicode_proto::files::{
    ConflictChoice, ResolveConflictRequest, TransferAction, TransferState, TransferUpdate,
};

use super::client::{ExplorerClient, TransferFrame};
use super::format;
use crate::icons::{self, icon};
use crate::state::AppState;
use crate::theme::Theme;

const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// A conflict's two sides, as the tray prints them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConflictLabels {
    pub name: SharedString,
    pub existing: SharedString,
    pub incoming: SharedString,
}

/// One job with its labels.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TransferRow {
    /// The engine whose stream reports it: the destination, which owns the job.
    pub owner: String,
    pub update: TransferUpdate,
    pub title: SharedString,
    pub detail: SharedString,
    /// Done, from 0 to 1.
    pub fraction: f32,
    pub conflict: Option<ConflictLabels>,
}

fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 {
        return 0.0;
    }
    let permille = done.saturating_mul(1000) / total;
    f32::from(u16::try_from(permille.min(1000)).unwrap_or(1000)) / 1000.0
}

impl TransferRow {
    pub(crate) fn new(owner: String, update: TransferUpdate, state: &AppState) -> Self {
        let now = Local::now();
        let what = match update.from.paths.as_slice() {
            [one] if update.items <= 1 => format::file_name(one).to_owned(),
            _ => format!("{} items", update.items.max(1)),
        };
        let device = state
            .device_name(&update.to.device_id)
            .unwrap_or("another device")
            .to_owned();
        let title = match update.state {
            TransferState::Done => format!("Copied {what} to {device}"),
            _ => format!("Copying {what} to {device}"),
        };
        let done = format::size_label(update.bytes_done);
        let total = format::size_label(update.bytes);
        let detail = match update.state {
            TransferState::Queued => "Waiting to start…".to_string(),
            TransferState::Running => {
                let mut parts = vec![format!("{done} of {total}")];
                if update.bytes_per_sec > 0 {
                    parts.push(format::speed_label(update.bytes_per_sec));
                }
                if let Some(eta) = format::eta_label(
                    update.bytes.saturating_sub(update.bytes_done),
                    update.bytes_per_sec,
                ) {
                    parts.push(eta);
                }
                if let Some(current) = &update.current {
                    parts.push(format::file_name(current).to_owned());
                }
                parts.join(" · ")
            }
            TransferState::Paused => format!("Paused · {done} of {total}"),
            TransferState::Conflict => "An item with the same name already exists".to_string(),
            TransferState::Done => format!("Done · {total}"),
            TransferState::Failed => format!(
                "Failed: {}",
                update.error.as_deref().unwrap_or("unknown error")
            ),
            TransferState::Cancelled => "Cancelled".to_string(),
        };
        let conflict = update
            .conflict
            .as_ref()
            .filter(|_| update.state == TransferState::Conflict)
            .map(|conflict| ConflictLabels {
                name: format::file_name(&conflict.path).to_owned().into(),
                existing: format!(
                    "{} · {}",
                    format::size_label(conflict.existing_size),
                    format::date_label(conflict.existing_modified_ms, now)
                )
                .into(),
                incoming: format!(
                    "{} · {}",
                    format::size_label(conflict.incoming_size),
                    format::date_label(conflict.incoming_modified_ms, now)
                )
                .into(),
            });
        Self {
            owner,
            fraction: fraction(update.bytes_done, update.bytes),
            update,
            title: title.into(),
            detail: detail.into(),
            conflict,
        }
    }
}

pub(crate) struct TransferTray {
    state: Entity<AppState>,
    pub(crate) rows: Vec<TransferRow>,
    watches: HashMap<String, Task<()>>,
    pub(crate) expanded: bool,
    /// Jobs whose "Apply to all" box is ticked.
    pub(crate) apply_to_all: HashSet<String>,
    summary: SharedString,
}

impl TransferTray {
    pub(crate) fn new(state: Entity<AppState>) -> Self {
        Self {
            state,
            rows: Vec::new(),
            watches: HashMap::new(),
            expanded: false,
            apply_to_all: HashSet::new(),
            summary: SharedString::default(),
        }
    }

    fn client(&self, cx: &gpui::App) -> Option<ExplorerClient> {
        let state = self.state.read(cx);
        Some(ExplorerClient::new(
            state.engine()?.clone(),
            state.local_device_id.clone(),
        ))
    }

    /// Watches exactly these engines' jobs.
    pub(crate) fn sync_watches(&mut self, devices: &[String], cx: &mut Context<Self>) {
        let before = self.rows.len();
        self.watches
            .retain(|device, _| devices.iter().any(|wanted| wanted == device));
        self.rows.retain(|row| devices.contains(&row.owner));
        for device in devices {
            self.watch(device.clone(), cx);
        }
        if self.rows.len() != before {
            self.refresh_summary();
            cx.notify();
        }
    }

    /// Watches `device`'s jobs, if not already.
    pub(crate) fn watch(&mut self, device: String, cx: &mut Context<Self>) {
        if self.watches.contains_key(&device) {
            return;
        }
        let Some(client) = self.client(cx) else {
            return;
        };
        let watched = device.clone();
        let task = cx.spawn(async move |this, cx| {
            loop {
                match client.watch_transfers(&watched).await {
                    Ok(mut frames) => {
                        while let Some(value) = frames.recv().await {
                            let Some(frame) = TransferFrame::parse(value) else {
                                continue;
                            };
                            let applied = this.update(cx, |tray, cx| {
                                tray.apply(&watched, frame, cx);
                            });
                            if applied.is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::debug!(%error, device = %watched, "transfer watch unavailable");
                    }
                }
                if this.update(cx, |_, _| {}).is_err() {
                    return;
                }
                cx.background_executor().timer(RETRY_DELAY).await;
            }
        });
        self.watches.insert(device, task);
    }

    /// Takes in one frame of `owner`'s stream.
    pub(crate) fn apply(&mut self, owner: &str, frame: TransferFrame, cx: &mut Context<Self>) {
        let state = self.state.read(cx);
        match frame {
            TransferFrame::List(list) => {
                self.rows.retain(|row| row.owner != owner);
                self.rows.extend(
                    list.into_iter()
                        .map(|update| TransferRow::new(owner.to_owned(), update, state)),
                );
            }
            TransferFrame::One(update) => {
                let row = TransferRow::new(owner.to_owned(), *update, state);
                match self
                    .rows
                    .iter_mut()
                    .find(|existing| existing.update.transfer_id == row.update.transfer_id)
                {
                    Some(existing) if *existing == row => return,
                    Some(existing) => *existing = row,
                    None => self.rows.push(row),
                }
            }
        }
        // A question waits: show it without being asked.
        if self
            .rows
            .iter()
            .any(|row| row.update.state == TransferState::Conflict)
        {
            self.expanded = true;
        }
        self.refresh_summary();
        cx.notify();
    }

    fn refresh_summary(&mut self) {
        let active: Vec<&TransferRow> = self
            .rows
            .iter()
            .filter(|row| !row.update.state.is_final())
            .collect();
        let waiting = active
            .iter()
            .filter(|row| row.update.state == TransferState::Conflict)
            .count();
        self.summary = if active.is_empty() {
            match self.rows.len() {
                0 => String::new(),
                1 => "1 transfer finished".to_string(),
                n => format!("{n} transfers finished"),
            }
        } else {
            let bytes: u64 = active.iter().map(|row| row.update.bytes).sum();
            let done: u64 = active.iter().map(|row| row.update.bytes_done).sum();
            let rate: u64 = active.iter().map(|row| row.update.bytes_per_sec).sum();
            let percent = (fraction(done, bytes) * 100.0).round();
            let mut text = match active.len() {
                1 => format!("1 transfer · {percent}%"),
                n => format!("{n} transfers · {percent}%"),
            };
            if rate > 0 {
                text.push_str(" · ");
                text.push_str(&format::speed_label(rate));
            }
            if waiting > 0 {
                text.push_str(" · needs an answer");
            }
            text
        }
        .into();
    }

    fn row(&self, transfer_id: &str) -> Option<&TransferRow> {
        self.rows
            .iter()
            .find(|row| row.update.transfer_id == transfer_id)
    }

    /// Pauses, resumes or cancels a job on the engine that owns it.
    pub(crate) fn control(
        &mut self,
        transfer_id: &str,
        action: TransferAction,
        cx: &mut Context<Self>,
    ) {
        let (Some(row), Some(client)) = (self.row(transfer_id), self.client(cx)) else {
            return;
        };
        let owner = row.owner.clone();
        let transfer_id = transfer_id.to_owned();
        cx.spawn(async move |_, cx| {
            if let Err(error) = client.control(&owner, transfer_id, action).await {
                cx.update(|cx| crate::toast::error(cx, format!("Transfer: {error:#}")));
            }
        })
        .detach();
    }

    /// Answers a job's conflict, for every later one too when its box is ticked.
    pub(crate) fn resolve(
        &mut self,
        transfer_id: &str,
        choice: ConflictChoice,
        cx: &mut Context<Self>,
    ) {
        let (Some(row), Some(client)) = (self.row(transfer_id), self.client(cx)) else {
            return;
        };
        let owner = row.owner.clone();
        let request = ResolveConflictRequest {
            transfer_id: transfer_id.to_owned(),
            choice,
            apply_to_all: self.apply_to_all.contains(transfer_id),
        };
        cx.spawn(async move |_, cx| {
            if let Err(error) = client.resolve(&owner, &request).await {
                cx.update(|cx| crate::toast::error(cx, format!("Transfer: {error:#}")));
            }
        })
        .detach();
    }

    fn toggle_apply_to_all(&mut self, transfer_id: &str, cx: &mut Context<Self>) {
        if !self.apply_to_all.remove(transfer_id) {
            self.apply_to_all.insert(transfer_id.to_owned());
        }
        cx.notify();
    }

    fn clear_finished(&mut self, cx: &mut Context<Self>) {
        self.rows.retain(|row| !row.update.state.is_final());
        self.refresh_summary();
        cx.notify();
    }

    fn render_row(
        &self,
        row: &TransferRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let id = row.update.transfer_id.clone();
        let state = row.update.state;
        let mut actions = div().flex().items_center().gap(px(4.0));
        match state {
            TransferState::Running | TransferState::Queued => {
                actions = actions.child(text_button(
                    format!("transfer-pause-{id}"),
                    "Pause",
                    theme,
                    cx.listener({
                        let id = id.clone();
                        move |tray, _, _, cx| tray.control(&id, TransferAction::Pause, cx)
                    }),
                ));
            }
            TransferState::Paused => {
                actions = actions.child(text_button(
                    format!("transfer-resume-{id}"),
                    "Resume",
                    theme,
                    cx.listener({
                        let id = id.clone();
                        move |tray, _, _, cx| tray.control(&id, TransferAction::Resume, cx)
                    }),
                ));
            }
            TransferState::Conflict
            | TransferState::Done
            | TransferState::Failed
            | TransferState::Cancelled => {}
        }
        if !state.is_final() {
            actions = actions.child(
                div()
                    .id(SharedString::from(format!("transfer-cancel-{id}")))
                    .debug_selector({
                        let id = id.clone();
                        move || format!("transfer-cancel-{id}")
                    })
                    .role(gpui::Role::Button)
                    .aria_label("Cancel transfer")
                    .size(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .hover(|style| style.bg(crate::theme::wash(0.1)))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |tray, _, _, cx| tray.control(&id, TransferAction::Cancel, cx)
                    }))
                    .child(
                        icon(icons::CLOSE)
                            .size(px(12.0))
                            .text_color(theme.text_muted),
                    ),
            );
        }
        let bar_color = match state {
            TransferState::Failed => theme.danger,
            TransferState::Conflict | TransferState::Paused => theme.warning,
            TransferState::Done => theme.success,
            TransferState::Queued | TransferState::Running | TransferState::Cancelled => {
                theme.accent
            }
        };
        let mut body = div()
            .id(SharedString::from(format!("transfer-{id}")))
            .debug_selector({
                let id = id.clone();
                move || format!("transfer-{id}")
            })
            .w_full()
            .px(px(12.0))
            .py(px(8.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .text_color(theme.text)
                            .child(row.title.clone()),
                    )
                    .child(actions),
            )
            .child(
                div()
                    .h(px(3.0))
                    .w_full()
                    .rounded(px(2.0))
                    .bg(crate::theme::wash(0.08))
                    .child(
                        div()
                            .h_full()
                            .w(relative(row.fraction))
                            .rounded(px(2.0))
                            .bg(bar_color),
                    ),
            )
            .child(
                div()
                    .truncate()
                    .text_size(px(11.0))
                    .text_color(if state == TransferState::Failed {
                        theme.danger
                    } else {
                        theme.text_muted
                    })
                    .child(row.detail.clone()),
            );
        if let Some(conflict) = &row.conflict {
            body = body.child(self.render_conflict(&id, conflict, theme, cx));
        }
        body.into_any_element()
    }

    fn render_conflict(
        &self,
        id: &str,
        conflict: &ConflictLabels,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let side = |label: &'static str, value: SharedString| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(theme.text_faint)
                        .child(label),
                )
                .child(
                    div()
                        .truncate()
                        .text_size(px(11.5))
                        .text_color(theme.text)
                        .child(value),
                )
        };
        let checked = self.apply_to_all.contains(id);
        let choice = |key: &str, label: &'static str, choice: ConflictChoice| {
            let id = id.to_owned();
            text_button(
                format!("transfer-{key}-{id}"),
                label,
                theme,
                cx.listener(move |tray, _, _, cx| tray.resolve(&id, choice, cx)),
            )
        };
        div()
            .mt(px(2.0))
            .p(px(8.0))
            .rounded(px(8.0))
            .bg(crate::theme::wash(0.05))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text)
                    .child(SharedString::from(format!(
                        "“{}” already exists in this folder.",
                        conflict.name
                    ))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(12.0))
                    .child(side("Existing", conflict.existing.clone()))
                    .child(side("Incoming", conflict.incoming.clone())),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .id(SharedString::from(format!("transfer-apply-all-{id}")))
                            .debug_selector({
                                let id = id.to_owned();
                                move || format!("transfer-apply-all-{id}")
                            })
                            .role(gpui::Role::CheckBox)
                            .aria_label("Apply to all")
                            .aria_toggled(if checked {
                                gpui::Toggled::True
                            } else {
                                gpui::Toggled::False
                            })
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .cursor_pointer()
                            .on_click(cx.listener({
                                let id = id.to_owned();
                                move |tray, _, _, cx| tray.toggle_apply_to_all(&id, cx)
                            }))
                            .child(
                                div()
                                    .size(px(13.0))
                                    .rounded(px(3.0))
                                    .border_1()
                                    .border_color(if checked {
                                        theme.accent
                                    } else {
                                        theme.border_strong
                                    })
                                    .bg(if checked {
                                        theme.accent
                                    } else {
                                        gpui::transparent_black()
                                    })
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(checked, |el| {
                                        el.child(
                                            icon(icons::CHECK)
                                                .size(px(10.0))
                                                .text_color(theme.on_accent),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(theme.text_muted)
                                    .child("Apply to all"),
                            ),
                    )
                    .child(div().flex_1())
                    .child(choice("skip", "Skip", ConflictChoice::Skip))
                    .child(choice("keep-both", "Keep Both", ConflictChoice::KeepBoth))
                    .child(choice("replace", "Replace", ConflictChoice::Replace)),
            )
            .into_any_element()
    }
}

fn text_button(
    id: String,
    label: &'static str,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id.clone()))
        .debug_selector(move || id.clone())
        .role(gpui::Role::Button)
        .aria_label(label)
        .h(px(22.0))
        .px(px(8.0))
        .flex()
        .items_center()
        .rounded(px(6.0))
        .cursor_pointer()
        .text_size(px(11.5))
        .text_color(theme.text)
        .bg(crate::theme::wash(0.07))
        .hover(|style| style.bg(crate::theme::wash(0.12)))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
        .on_click(on_click)
        .child(label)
}

impl Render for TransferTray {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            return gpui::Empty.into_any_element();
        }
        let theme = Theme::of(cx).clone();
        let any_final = self.rows.iter().any(|row| row.update.state.is_final());
        let header = div()
            .id("transfer-tray-header")
            .debug_selector(|| "transfer-tray-header".into())
            .role(gpui::Role::Button)
            .aria_label("Transfers")
            .aria_expanded(self.expanded)
            .h(px(32.0))
            .px(px(12.0))
            .flex()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .on_click(cx.listener(|tray, _, _, cx| {
                tray.expanded = !tray.expanded;
                cx.notify();
            }))
            .child(
                icon(icons::ARROW_DOWN)
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(12.0))
                    .text_color(theme.text)
                    .child(self.summary.clone()),
            )
            .when(any_final, |el| {
                el.child(text_button(
                    "transfer-tray-clear".into(),
                    "Clear",
                    &theme,
                    cx.listener(|tray, _, _, cx| {
                        cx.stop_propagation();
                        tray.clear_finished(cx);
                    }),
                ))
            })
            .child(
                icon(if self.expanded {
                    icons::ALT_ARROW_DOWN
                } else {
                    icons::ALT_ARROW_UP
                })
                .size(px(12.0))
                .text_color(theme.text_faint),
            );
        let mut tray = div()
            .id("transfer-tray")
            .debug_selector(|| "transfer-tray".into())
            .role(gpui::Role::Region)
            .aria_label("Transfers")
            .w_full()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .flex_col()
            .child(header);
        if self.expanded {
            let rows: Vec<gpui::AnyElement> = self
                .rows
                .iter()
                .map(|row| self.render_row(row, &theme, cx))
                .collect();
            tray = tray.child(
                div()
                    .id("transfer-tray-rows")
                    .max_h(px(260.0))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows),
            );
        }
        tray.into_any_element()
    }
}
