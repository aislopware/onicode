//! The shell's mirror of the engine's settings (`WatchEngineSettings`), as `workspace_mirror` is
//! of its workspace. The Settings pages and the pickers edit [`Preferences::engine`] as they edit
//! the rest of the preferences; when the shell saves, each key that moved goes to the engine as a
//! `SetEngineSetting` under an op the shell mints, already shown. The engine's events lay its
//! values over every key with no call in flight; a refused call puts the engine's value back.
//!
//! [`Preferences::engine`]: crate::settings::Preferences::engine
use onicode_protocol::engine_settings::{
    EngineSetting, EngineSettings, EngineSettingsEvent, SetEngineSetting, WatchEngineSettings,
};
use onicode_protocol::{OpId, Outcome};

use super::*;
use crate::engine_client::{WatchUpdate, Watching};

pub(super) struct Mirror {
    /// The settings as the engine last showed them.
    held: EngineSettings,
    /// What the shell last sent or took from the engine: what an edit is told apart from.
    sent: EngineSettings,
    /// The keys sent and not yet answered, each with its latest op.
    pending: HashMap<&'static str, OpId>,
    _watch: Option<(
        Watching<EngineSettings, EngineSettingsEvent>,
        gpui::Task<()>,
    )>,
}

impl Mirror {
    /// A mirror that starts from `now`, the engine's settings as the window opens.
    pub(super) fn new(now: &EngineSettings) -> Self {
        Self {
            held: now.clone(),
            sent: now.clone(),
            pending: HashMap::new(),
            _watch: None,
        }
    }

    /// The engine's value of `key`.
    fn held(&self, key: &str) -> Option<EngineSetting> {
        self.held
            .all()
            .into_iter()
            .find(|setting| setting.key() == key)
    }
}

impl Shell {
    /// Starts mirroring the engine's settings.
    pub(super) fn mirror_engine_settings(&mut self, cx: &mut Context<Self>) {
        let watching = self
            .engine
            .watch::<WatchEngineSettings>(|since| WatchEngineSettings { since });
        let updates = watching.updates.clone();
        let task = cx.spawn(async move |this, cx| {
            while let Ok(update) = updates.recv().await {
                if this
                    .update(cx, |this, cx| this.apply_engine_settings(update, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.engine_settings._watch = Some((watching, task));
    }

    fn apply_engine_settings(
        &mut self,
        update: WatchUpdate<EngineSettings, EngineSettingsEvent>,
        cx: &mut Context<Self>,
    ) {
        let mirror = &mut self.engine_settings;
        // The engine's values to show, of keys no call of the shell's is changing.
        let mut shown = Vec::new();
        match update {
            WatchUpdate::Snapshot(state) => {
                mirror.held = *state;
                shown.extend(mirror.held.all());
            }
            WatchUpdate::Events(events) => {
                for event in events {
                    match event {
                        EngineSettingsEvent::Set(setting) => {
                            mirror.held.apply(&setting);
                            shown.push(setting);
                        }
                        EngineSettingsEvent::Receipt { op, outcome } => {
                            let Some(key) = mirror
                                .pending
                                .iter()
                                .find(|(_, pending)| **pending == op)
                                .map(|(key, _)| *key)
                            else {
                                continue;
                            };
                            mirror.pending.remove(key);
                            // Applied, its value is the engine's already; refused, the
                            // engine's value comes back.
                            if matches!(outcome, Outcome::Rejected(_)) {
                                shown.extend(mirror.held(key));
                            }
                        }
                    }
                }
            }
        }
        shown.retain(|setting| !mirror.pending.contains_key(setting.key()));
        if shown.is_empty() {
            return;
        }
        for setting in &shown {
            mirror.sent.apply(setting);
        }
        let before = self.workspace.preferences.engine.clone();
        for setting in &shown {
            self.workspace.preferences.engine.apply(setting);
        }
        if self.workspace.preferences.engine != before {
            self.show_engine_settings(cx);
        }
    }

    /// Shows the engine settings the shell holds in every view that reads them.
    fn show_engine_settings(&mut self, cx: &mut Context<Self>) {
        cx.set_global(self.workspace.preferences.clone());
        self.sync_settings(cx);
        self.invalidate_chrome(cx);
        cx.notify();
    }

    /// Sends the engine every key the preferences hold differently from what it was last sent.
    /// Called whenever the shell saves its state, after any edit.
    pub(super) fn send_engine_settings(&mut self, cx: &mut Context<Self>) {
        let changes = self
            .workspace
            .preferences
            .engine
            .changes_from(&self.engine_settings.sent);
        if !changes.is_empty() {
            // Every view shows the edit in the frame it was made, before the engine answers.
            cx.set_global(self.workspace.preferences.clone());
        }
        for setting in changes {
            let op = OpId::new();
            let key = setting.key();
            self.engine_settings.pending.insert(key, op);
            self.engine_settings.sent.apply(&setting);
            let call = self
                .engine
                .call::<SetEngineSetting>(Some(op), SetEngineSetting { setting });
            cx.spawn(async move |this, cx| {
                let outcome = call.await;
                let _ = this.update(cx, |this, cx| {
                    this.settle_engine_setting(key, op, outcome, cx);
                });
            })
            .detach();
        }
    }

    /// Settles the call that sent `key` under `op`: the receipt usually came first; a call that
    /// never reached the engine, or that it refused, puts the engine's value back.
    fn settle_engine_setting(
        &mut self,
        key: &'static str,
        op: OpId,
        outcome: Result<Outcome, String>,
        cx: &mut Context<Self>,
    ) {
        let mirror = &mut self.engine_settings;
        if mirror.pending.get(key) != Some(&op) {
            return;
        }
        mirror.pending.remove(key);
        if matches!(outcome, Ok(Outcome::Applied)) {
            return;
        }
        let Some(held) = mirror.held(key) else {
            return;
        };
        mirror.sent.apply(&held);
        self.workspace.preferences.engine.apply(&held);
        if let Err(error) = outcome {
            self.error = Some(format!("Could not save the setting: {error}"));
        }
        self.show_engine_settings(cx);
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn an_engine_setting_shows_at_once_and_reaches_the_engine(cx: &mut TestAppContext) {
        let (window, _, _, ()) = super::super::tests::seeded_window(cx, |_| ());
        window
            .update(cx, |shell, _, cx| {
                shell.workspace.preferences.engine.idle_agents = 1;
                shell.workspace.preferences.engine.default_provider = Some("claude".into());
                shell.save_state(cx);
                // The shell shows it in the frame it made it.
                assert_eq!(shell.workspace.preferences.engine.idle_agents, 1);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert_eq!(shell.engine.settings_now().idle_agents, 1);
                assert_eq!(
                    shell.engine.settings_now().default_provider.as_deref(),
                    Some("claude")
                );
                assert!(shell.engine_settings.pending.is_empty());
                assert_eq!(
                    cx.global::<crate::settings::Preferences>()
                        .engine
                        .idle_agents,
                    1
                );
                let sent: Vec<_> = crate::engine_client::calls_for_test(&shell.engine, cx)
                    .into_iter()
                    .filter(|(name, _)| *name == "SetEngineSetting")
                    .collect();
                assert_eq!(sent.len(), 2, "one call per key that moved");
            })
            .unwrap();
    }

    #[gpui::test]
    fn another_viewers_change_reaches_the_preferences(cx: &mut TestAppContext) {
        let (window, _, _, ()) = super::super::tests::seeded_window(cx, |_| ());
        let engine = window
            .update(cx, |shell, _, _| shell.engine.clone())
            .unwrap();
        let set = engine.call::<SetEngineSetting>(
            Some(OpId::new()),
            SetEngineSetting {
                setting: EngineSetting::ClaudeHooks(false),
            },
        );
        cx.executor().spawn(set).detach();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, _| {
                assert!(!shell.workspace.preferences.engine.claude_hooks);
            })
            .unwrap();
    }
}
