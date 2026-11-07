use gpui::App;
use onicode_protocol::accounts::Profile;
use onicode_protocol::agents::{ForgetThread, TransferThread};
use onicode_protocol::session::ResetThread;
use onicode_protocol::usage;
use onicode_protocol::usage::Limit;

use super::*;
impl Conversation {
    /// Update notifications refresh the account-specific picker after a CLI update.
    pub(crate) fn refresh_models_after_harness_update(&mut self, cx: &mut Context<Self>) {
        let reload = self.engine.call::<onicode_protocol::agents::Catalog>(
            None,
            onicode_protocol::agents::Catalog {
                provider: Kind::from_id(&self.summary.provider),
                account: self.settings.account.id.clone(),
                read: onicode_protocol::agents::CatalogRead::Reload,
            },
        );
        cx.spawn(async move |this, cx| {
            let _ = reload.await;
            let _ = this.update(cx, |this, cx| {
                this.refresh_catalog(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Claude reports its limits only during turns, so a finished turn is when its meter moves.
    pub(super) fn refresh_turn_usage(&self, cx: &mut Context<Self>) {
        let kind = Kind::from_id(&self.summary.provider);
        if !usage::from_turns(kind) {
            return;
        }
        let key =
            crate::accounts::Key::new(kind, self.settings.account.clone(), self.account_model());
        if let Some(center) = crate::accounts::get(cx) {
            center.update(cx, |center, cx| center.refresh(key, true, cx));
        }
    }

    pub(super) fn auth_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let kind = Kind::from_id(&self.summary.provider);
        let action = onicode_protocol::accounts::AuthAction::SignIn;
        if self.busy || onicode_protocol::accounts::login_arguments(kind, action).is_none() {
            return None;
        }
        let status = self.status.to_ascii_lowercase();
        if ![
            "not signed in",
            "not authenticated",
            "authentication required",
            "please log in",
            "please login",
            "no longer signed in",
        ]
        .iter()
        .any(|s| status.contains(s))
        {
            return None;
        }
        let id = self.settings.account.id.clone();
        Some(
            div()
                .mx_2()
                .mb_2()
                .p_3()
                .rounded_lg()
                .bg(content(0.04))
                .flex()
                .items_center()
                .gap_2()
                .child(provider_icon(kind.id(), 20.))
                .child(
                    div()
                        .flex_1()
                        .child(format!("Sign in to {} to continue", kind.label())),
                )
                .child(
                    crate::settings::widgets::secondary_button(
                        "conversation-sign-in",
                        "Sign in",
                        true,
                        false,
                    )
                    .child("Sign in")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(center) = crate::accounts::get(cx) {
                            center.update(cx, |center, cx| {
                                center.begin_auth(kind, &id, action, None, cx)
                            });
                        }
                    })),
                )
                .into_any_element(),
        )
    }

    /// A remote session runs on its host's default account. A local one names its account and
    /// the engine finds its home.
    pub(crate) fn attach_account(&mut self, _: &App) {
        if self.is_remote() {
            self.settings.account = Default::default();
        }
    }

    pub(crate) fn account_model(&self) -> Option<&str> {
        self.model.as_deref().or(self.resolved_model.as_deref())
    }

    pub(crate) fn set_blank_account(&mut self, profile: Profile, cx: &mut Context<Self>) {
        if self.busy || self.summary.has_messages {
            return;
        }
        self.settings.account = profile;
        self.catalog.clear();
        self.resolved_model = None;
        self.refresh_catalog(cx);
        self.save_settings(cx);
        cx.emit(SessionChanged);
        cx.notify();
    }

    pub(super) fn detect_usage_limit(&mut self, cx: &mut Context<Self>) {
        if let Some(limit) = self
            .settings
            .usage_limit
            .clone()
            .or_else(|| Limit::from_error(&self.status))
        {
            if let Some(pending) = &mut self.settings.pending_provider_switch {
                pending.skip_outgoing_recap = true;
            }
            self.settings.usage_limit = Some(limit);
            self.save_settings(cx);
            self.follow_ups.state.paused = true;
            self.save_follow_ups(cx);
            let kind = Kind::from_id(&self.summary.provider);
            if let Some(center) = crate::accounts::get(cx) {
                let key = crate::accounts::Key::new(
                    kind,
                    self.settings.account.clone(),
                    self.account_model(),
                );
                center.update(cx, |center, cx| center.refresh(key, true, cx));
            }
        }
    }

    /// Account recovery also works for saved conversations from before native
    /// limit metadata was recorded, and after dismissing the banner. Inspect
    /// only the final error/notice: an older limit must not affect a later turn.
    pub(crate) fn account_recovery_limit(&self, cx: &App) -> Option<Limit> {
        self.settings
            .usage_limit
            .clone()
            .or_else(|| Limit::from_error(&self.status))
            .or_else(|| {
                let rows = self.transcript.read(cx).source_rows();
                let last = rows.last()?;
                if !matches!(last.role.as_str(), "error" | "system") {
                    return None;
                }
                let start = rows.iter().rposition(|row| row.first).unwrap_or(0);
                let text: String = rows[start..].iter().map(|row| row.text.as_str()).collect();
                Limit::from_error(&text)
            })
    }

    /// Follows the engine's hold on this session's usage limit while it shows one: the reset the
    /// engine learned from the account's usage, and the resume the engine sent once an armed
    /// limit came due. The engine alone resumes; this view only stops showing what it took.
    pub(super) fn sync_usage_limit(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.is_remote() || self.settings.usage_limit.is_none() {
            return;
        }
        let record = self.session_call::<ReadSession>(None, |session| ReadSession { session }, cx);
        cx.spawn(async move |this, cx| {
            let Ok(record) = record.await else {
                return;
            };
            let held = record.settings.and_then(|settings| settings.usage_limit);
            let _ = this.update(cx, |this, cx| {
                if this.busy {
                    return;
                }
                let Some(limit) = this.settings.usage_limit.as_mut() else {
                    return;
                };
                match held {
                    Some(held) if limit.resets_at.is_none() && held.resets_at.is_some() => {
                        limit.resets_at = held.resets_at;
                        cx.notify();
                    }
                    None if limit.resume_at_reset && !limit.waiting(usage::now_ms()) => {
                        this.settings.usage_limit = None;
                        cx.emit(SessionChanged);
                        cx.notify();
                    }
                    _ => {}
                }
            });
        })
        .detach();
    }

    /// The person's Continue: a prompt like any other, which the engine runs and which lifts the
    /// limit as every turn does.
    pub(crate) fn resume_usage(&mut self, cx: &mut Context<Self>) {
        if self.needs_history_restore() {
            self.defer_history_action(|this, cx| this.resume_usage(cx), cx);
            return;
        }
        if self.busy
            || self.stopping
            || self.account_switching
            || self.settings.usage_limit.take().is_none()
        {
            return;
        }
        self.status = "Ready".into();
        self.save_settings(cx);
        self.launch_prompt("Continue from where you left off.".into(), cx);
        cx.emit(SessionChanged);
        cx.notify();
    }

    pub(super) fn recover_usage_after_model_change(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.stopping {
            return;
        }
        let Some(limit) = self.settings.usage_limit.as_mut() else {
            return;
        };
        // The engine holds the limit until a turn continues past it: the new model stops its
        // resume.
        limit.resume_at_reset = false;
        self.save_settings(cx);
        self.settings.usage_limit = None;
        self.status = "Ready".into();
        self.follow_ups.state.paused = false;
        for message in &mut self.follow_ups.state.messages {
            message.error = None;
        }
        self.save_follow_ups(cx);
        cx.emit(SessionChanged);
        cx.notify();
    }

    /// Moves this conversation to another account in place. A limited
    /// session resumes on the new account; an idle one just waits for the
    /// next message there.
    pub(crate) fn switch_account(&mut self, profile: Profile, cx: &mut Context<Self>) {
        if self.editing_last_turn.pending() {
            return;
        }
        if self.needs_history_restore() {
            self.defer_history_action(move |this, cx| this.switch_account(profile, cx), cx);
            return;
        }
        if self.busy
            || self.stopping
            || self.account_switching
            || self.settings.account.id == profile.id
        {
            return;
        }
        let limit = self.account_recovery_limit(cx);
        let limited = limit.is_some();
        if limited {
            self.settings.usage_limit = limit;
        }
        self.attach_account(cx);
        let from = self.settings.account.clone();
        let thread = self.summary.provider_id.clone();
        let expected = thread.clone();
        let kind = Kind::from_id(&self.summary.provider);
        self.account_switching = true;
        let transfer = thread.map(|thread| {
            self.engine.call::<TransferThread>(
                Some(OpId::new()),
                TransferThread {
                    provider: kind,
                    from: from.id.clone(),
                    to: profile.id.clone(),
                    thread,
                },
            )
        });
        let work = cx.background_executor().spawn(async move {
            match transfer {
                Some(transfer) => transfer.await.unwrap_or(false),
                None => false,
            }
        });
        let old = self.settings.account.id.clone();
        cx.spawn(async move |this, cx| {
            let carried = work.await;
            let _ = this.update(cx, |this, cx| {
                this.account_switching = false;
                if this.busy
                    || (limited && this.settings.usage_limit.is_none())
                    || this.settings.account.id != old
                    || this.summary.provider_id != expected
                    || crate::accounts::get(cx)
                        .is_some_and(|center| !center.read(cx).book.exists(kind, &profile.id))
                {
                    return;
                }
                if !carried {
                    if this.settings.pending_provider_switch.is_none() {
                        this.settings.pending_provider_switch =
                            Some(onicode_protocol::agent::ProviderSwitch {
                                from: this.summary.provider.clone(),
                                model_label: this.model_label().to_string(),
                                cwd: this.execution_cwd().to_owned(),
                                model: this.model.clone(),
                                model_settings: this.settings.model_settings.clone(),
                                effort: this.settings.effort.clone(),
                                fast: this.settings.fast,
                                service_tier: this.settings.service_tier.clone(),
                                thinking: this.settings.thinking,
                                skip_outgoing_recap: limited,
                                account: this.settings.account.clone(),
                                provider_id: this.summary.provider_id.clone(),
                            });
                    }
                    this.summary.provider_id = None;
                    let expected = expected.clone();
                    this.session_call::<ResetThread>(
                        Some(OpId::new()),
                        move |session| ResetThread { session, expected },
                        cx,
                    )
                    .detach();
                    this.settings.handoff = Some(onicode_protocol::agent::Handoff {
                        from: this.summary.provider.clone(),
                        brief: onicode_protocol::response::handoff(
                            this.transcript.read(cx).source_rows(),
                        ),
                    });
                } else if let Some(thread) = this.summary.provider_id.clone() {
                    // The thread moved; the old account's agent lets it go.
                    let forget = this.engine.call::<ForgetThread>(
                        Some(OpId::new()),
                        ForgetThread {
                            provider: kind,
                            thread,
                            account: old.clone(),
                            cwd: this.execution_cwd().to_owned(),
                        },
                    );
                    cx.background_executor().spawn(forget).detach();
                }
                this.settings.account = profile;
                this.catalog.clear();
                this.refresh_catalog(cx);
                this.save_settings(cx);
                if limited {
                    this.resume_usage(cx);
                } else {
                    cx.emit(SessionChanged);
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn account_unavailable(&mut self, cx: &mut Context<Self>) {
        self.stop_if_busy(cx);
        self.follow_ups.state.paused = true;
        self.follow_ups.requested = None;
        self.save_follow_ups(cx);
        self.catalog_epoch += 1;
        self.catalog.clear();
        self.status =
            "This provider account is no longer signed in. Choose another account to continue."
                .into();
        cx.notify();
    }

    pub(crate) fn account_authenticated(&mut self, cx: &mut Context<Self>) {
        self.attach_account(cx);
        self.refresh_catalog(cx);
        cx.notify();
    }

    pub(super) fn usage_notice(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let limit = self.settings.usage_limit.as_ref()?;
        let now = usage::now_ms();
        let waiting = limit.waiting(now);
        let armed = limit.resume_at_reset;
        let switching = self.account_switching;
        let kind = Kind::from_id(&self.summary.provider);
        let reset = usage_reset_label(limit, now);
        let mut controls = div().flex().flex_shrink_0().items_center().gap(px(8.));
        if onicode_protocol::accounts::supports_profiles(kind)
            && crate::accounts::get(cx).is_some_and(|center| {
                center
                    .read(cx)
                    .book
                    .list(kind)
                    .iter()
                    .any(|a| a.id != self.settings.account.id)
            })
        {
            controls = controls.child(
                usage_button(
                    "usage-account-picker",
                    "Choose another account",
                    false,
                    switching,
                )
                .relative()
                .gap(px(4.))
                .child(self.account_anchor.probe())
                .child(if switching {
                    "Switching…"
                } else {
                    "Switch account"
                })
                .child(icon("chevron-down", 12.).text_color(content(0.5)))
                .when(!switching, |d| {
                    d.on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_menu(ComposerMenu::Account, window, cx)
                    }))
                }),
            );
        }
        let resume = if waiting {
            if armed {
                "Resuming at reset"
            } else {
                "Resume at reset"
            }
        } else {
            "Resume"
        };
        controls = controls
            .child(
                usage_button(
                    "resume-usage-limit",
                    if waiting && armed {
                        "Cancel the automatic resume"
                    } else {
                        resume
                    },
                    waiting && armed,
                    switching,
                )
                .child(
                    icon(if waiting { "clock" } else { "play" }, 14.)
                        .text_color(if waiting && armed {
                            gpui::rgb(AMBER_400).into()
                        } else {
                            content(0.55)
                        })
                        .group_hover("resume-usage-limit", |s| s.text_color(content(1.))),
                )
                .child(resume)
                .when(!switching, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        if waiting {
                            if let Some(limit) = &this.settings.usage_limit {
                                let enabled = !limit.resume_at_reset;
                                this.set_usage_resume_at_reset(enabled, cx);
                            }
                        } else {
                            this.resume_usage(cx);
                        }
                    }))
                }),
            )
            .child(
                usage_button(
                    "dismiss-usage-limit",
                    "Dismiss usage limit notice",
                    false,
                    false,
                )
                .w(px(24.))
                .px_0()
                .child(
                    icon("close", 14.)
                        .text_color(content(0.55))
                        .group_hover("dismiss-usage-limit", |s| s.text_color(content(1.))),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    // The engine holds the limit until a turn continues past it: dismissing the
                    // notice stops its resume.
                    if let Some(limit) = this.settings.usage_limit.as_mut() {
                        limit.resume_at_reset = false;
                    }
                    this.save_settings(cx);
                    this.settings.usage_limit = None;
                    cx.notify();
                })),
            );
        let heading = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(8.))
            .whitespace_nowrap()
            .child(icon("gauge", 14.).text_color(gpui::rgb(AMBER_400)))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(content(0.85))
                    .child("Usage limit reached"),
            );
        let notice = div()
            .id("usage-limit-notice")
            .debug_selector(|| "usage-limit-notice".into())
            .role(Role::Status)
            .aria_label("Usage limit reached")
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(content(0.55));
        Some(
            notice
                .px(px(8.))
                .child(
                    div()
                        .id("usage-limit-banner")
                        .debug_selector(|| "usage-limit-banner".into())
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded_t(px(10.))
                        .border_1()
                        .border_b_0()
                        .border_color(tint(AMBER_400, 0.25))
                        .bg(tint(AMBER_400, 0.1))
                        .px(px(8.))
                        .child(heading)
                        .child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .child(reset.unwrap_or_default()),
                        )
                        .child(controls),
                )
                .into_any_element(),
        )
    }
}

fn usage_button(
    id: &'static str,
    label: &'static str,
    primary: bool,
    disabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .group(id)
        .role(Role::Button)
        .aria_label(label)
        .h(px(24.))
        .px(px(6.))
        .rounded_md()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .text_size(px(12.))
        .line_height(px(16.))
        .whitespace_nowrap()
        .text_color(if primary {
            gpui::rgb(AMBER_400).into()
        } else {
            content(0.55)
        })
        .when(disabled, |d| d.opacity(0.45))
        .when(!disabled, |d| {
            d.cursor_pointer()
                .hover(move |s| s.bg(content(0.1)).text_color(content(1.)))
        })
}

/// Shared reset wording for desktop and its paired phone.
pub(super) fn usage_reset_label(limit: &Limit, now: i64) -> Option<String> {
    limit.resets_at.map(|at| {
        if at <= now {
            return "Limit has reset".to_owned();
        }
        let when = chrono::DateTime::from_timestamp_millis(at)
            .map(|d| {
                let local = d.with_timezone(&chrono::Local);
                let today = chrono::Local::now().date_naive() == local.date_naive();
                local
                    .format(if today {
                        "%-I:%M %p"
                    } else {
                        "%b %-d, %-I:%M %p"
                    })
                    .to_string()
            })
            .unwrap_or_default();
        format!(
            "Resets {when} · in {}",
            usage::Window {
                used_percent: Some(100.),
                minutes: 300,
                resets_at: Some(at),
                status: None,
            }
            .countdown(now)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn account_recovery_uses_the_terminal_limit_without_reviving_an_older_failure(
        cx: &mut gpui::TestAppContext,
    ) {
        use onicode_protocol::transcript::Row;
        let engine = cx.update(crate::engine_client::Engine::memory);
        let summary = engine.seed().create("claude", "/fixture").unwrap();
        let view = cx.new(|cx| Conversation::new(summary, engine, cx));
        view.update(cx, |view, cx| {
            let row = |role: &str, text: &str| Row {
                role: role.into(),
                text: text.into(),
                first: true,
                sent_at: None,
                id: None,
            };
            for role in ["error", "system"] {
                view.transcript.update(cx, |v, cx| {
                    v.patch(
                        0,
                        vec![row("user", "Keep going"), row(role, "rate_limit")],
                        cx,
                    );
                });
                assert!(view.account_recovery_limit(cx).is_some());
            }
            view.transcript.update(cx, |v, cx| {
                v.patch(
                    2,
                    vec![row("user", "Try again"), row("assistant", "Completed")],
                    cx,
                );
            });
            assert!(view.account_recovery_limit(cx).is_none());
            view.transcript.update(cx, |v, cx| {
                v.patch(4, vec![row("system", "Network timeout")], cx);
            });
            assert!(view.account_recovery_limit(cx).is_none());
        });
    }

    #[gpui::test]
    fn claude_limit_code_opens_recovery_and_preserves_the_reported_reset(
        cx: &mut gpui::TestAppContext,
    ) {
        let engine = cx.update(crate::engine_client::Engine::memory);
        let summary = engine.seed().create("claude", "/fixture").unwrap();
        let view = cx.new(|cx| Conversation::new(summary, engine, cx));
        view.update(cx, |view, cx| {
            view.status = "rate_limit".into();
            view.detect_usage_limit(cx);
            assert!(view.settings.usage_limit.is_some());
            assert!(view.follow_ups.state.paused);
            let reported = Limit {
                resets_at: Some(usage::now_ms() + 3_600_000),
                resume_at_reset: false,
            };
            view.settings.usage_limit = Some(reported.clone());
            view.detect_usage_limit(cx);
            assert_eq!(view.settings.usage_limit, Some(reported));
        });
    }

    #[gpui::test]
    fn usage_banner_matches_the_composer_and_can_arm_cancel_and_dismiss(
        cx: &mut gpui::TestAppContext,
    ) {
        let engine = cx.update(crate::engine_client::Engine::memory);
        let mut summary = engine.seed().create("claude", "/fixture").unwrap();
        summary.has_messages = true;
        let id = summary.id;
        let saved = engine.seed();
        let limit = Limit {
            resets_at: Some(usage::now_ms() + 3_600_000),
            resume_at_reset: false,
        };
        // The engine holds the limit the last turn ended at; the banner arms that one.
        saved
            .save_agent_settings(
                id,
                &Settings {
                    usage_limit: Some(limit.clone()),
                    ..Settings::default()
                },
            )
            .unwrap();
        let window = cx.update(|cx| {
            crate::accounts::install(engine.clone(), cx);
            crate::accounts::get(cx).unwrap().update(cx, |center, cx| {
                let mut book = center.book.clone();
                book.add(Kind::Claude, "Other account").unwrap();
                center.script(
                    onicode_protocol::agents::AccountsState {
                        book,
                        ..Default::default()
                    },
                    cx,
                );
            });
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|cx| {
                    let mut view = Conversation::new(summary, engine, cx);
                    view.loaded = true;
                    view.status = "Claude Code reached its usage limit.".into();
                    view.settings.usage_limit = Some(limit);
                    view
                })
            })
            .unwrap()
        });
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.refresh().unwrap();
        let banner = visual.debug_bounds("usage-limit-banner").unwrap();
        let composer = visual.debug_bounds("composer-box").unwrap();
        let resume = visual.debug_bounds("resume-usage-limit").unwrap();
        let account = visual.debug_bounds("usage-account-picker").unwrap();
        let dismiss = visual.debug_bounds("dismiss-usage-limit").unwrap();
        assert_eq!(banner.size.height, px(32.));
        assert_eq!(resume.size.height, px(24.));
        assert_eq!(dismiss.size, gpui::size(px(24.), px(24.)));
        assert_eq!(banner.bottom(), composer.top());
        assert_eq!(banner.left(), composer.left() + px(8.));
        assert_eq!(banner.right(), composer.right() - px(8.));
        assert_eq!(account.right() + px(8.), resume.left());
        assert_eq!(resume.right() + px(8.), dismiss.left());
        assert!(visual.debug_bounds("usage-model-picker").is_none());
        assert!(visual.debug_bounds("provider-status").is_none());

        visual.simulate_click(resume.center(), Default::default());
        assert!(
            window
                .update(visual, |v, _, _| v
                    .settings
                    .usage_limit
                    .as_ref()
                    .unwrap()
                    .resume_at_reset)
                .unwrap()
        );
        visual.run_until_parked();
        assert!(
            saved
                .agent_settings(id)
                .unwrap()
                .unwrap()
                .usage_limit
                .unwrap()
                .resume_at_reset
        );
        visual.refresh().unwrap();
        let resume = visual.debug_bounds("resume-usage-limit").unwrap();
        visual.simulate_click(resume.center(), Default::default());
        assert!(
            !window
                .update(visual, |v, _, _| v
                    .settings
                    .usage_limit
                    .as_ref()
                    .unwrap()
                    .resume_at_reset)
                .unwrap()
        );

        // Unknown or elapsed reset times keep a manual Resume control and the
        // same right-aligned actions, as in React's composer variant.
        for resets_at in [None, Some(usage::now_ms() - 1_000)] {
            window
                .update(visual, |v, _, cx| {
                    v.settings.usage_limit.as_mut().unwrap().resets_at = resets_at;
                    cx.notify();
                })
                .unwrap();
            visual.refresh().unwrap();
            assert_eq!(
                visual.debug_bounds("resume-usage-limit").unwrap().right(),
                resume.right()
            );
        }
        visual.simulate_click(dismiss.center(), Default::default());
        visual.refresh().unwrap();
        assert!(visual.debug_bounds("usage-limit-notice").is_none());
        assert!(
            window
                .update(visual, |v, _, _| v.settings.usage_limit.is_none())
                .unwrap()
        );
    }
}
