//! The composer's model controls: the model list beside the composer and the
//! setting pills, with the access and model-and-settings menus.
use super::*;

#[derive(Default)]
pub(super) struct ModelControlsState {
    /// ModelPicker with `hideSettings`: the model list opens on its own.
    pub(super) models: Option<(Entity<ModelList>, Vec<Subscription>)>,
    /// The setting pills' anchors, by `ComposerMenu::Setting` index.
    pub(super) setting_anchors: Vec<AnchorSlot>,
}
impl Conversation {
    pub(super) fn access_options(&self) -> Vec<MenuItem> {
        let provider = Kind::from_id(&self.summary.provider);
        let mut items = vec![
            MenuAction::new("access:read-only", "Read-only")
                .icon("square-lock")
                .description("Inspect without changing files.")
                .checked(self.settings.access == AccessMode::ReadOnly)
                .into(),
        ];
        items.extend(ACCESS_MODES.iter().map(|(id, label, hint, glyph)| {
                    let hint = match (provider,*id) {
                        (Kind::Cursor,"supervised") => "Confirm commands using Cursor's permission policy; Cursor may accept edits automatically.",
                        (Kind::Pi | Kind::Omp,_) => "Tool confirmations follow this provider's configured extensions.",
                        _ => *hint,
                    };
                    let action = MenuAction::new(format!("access:{id}"), *label)
                        .icon(glyph)
                        .description(hint)
                        .checked(self.settings.access.id() == *id);
                    MenuItem::from(if *id == "full-access" {
                        action.icon_color(tint(AMBER_400, 0.9))
                    } else {
                        action
                    })
                }));
        if self.busy {
            items.push(MenuItem::Note("Changes apply to the next turn.".into()));
        }
        items
    }

    pub(super) fn model_options(&self) -> Vec<MenuItem> {
        let mut items: Vec<MenuAction> = Vec::new();
        if self.model_info().is_some_and(|m| !m.efforts.is_empty()) {
            items.push(
                MenuAction::new("reasoning", self.effort_setting_label())
                    .value(self.effort_label())
                    .submenu(self.effort_items()),
            );
        }
        if self.model_info().is_some_and(|m| m.fast) {
            items.push(MenuAction::new("fast", "Fast").toggle(self.settings.fast));
        }
        if let Some(model) = self.model_info().filter(|m| !m.tiers.is_empty()) {
            let mut tiers =
                vec![MenuAction::new("tier:", "Standard").checked(self.selected_tier().is_none())];
            tiers.extend(model.tiers.iter().map(|(id, name)| {
                MenuAction::new(format!("tier:{id}"), name.clone())
                    .checked(self.selected_tier() == Some(id.as_str()))
            }));
            items.push(
                MenuAction::new("service-tier", "Service Tier")
                    .value(
                        self.selected_tier()
                            .and_then(|id| model.tiers.iter().find(|(key, _)| key == id))
                            .map(|(_, label)| label.as_str())
                            .unwrap_or_else(|| self.selected_tier().unwrap_or("Standard"))
                            .to_owned(),
                    )
                    .submenu(tiers),
            );
        }
        if self.model_info().is_some_and(|m| m.thinking) {
            items.push(
                MenuAction::new("thinking", "Thinking")
                    .toggle(self.settings.thinking.unwrap_or(false)),
            );
        }
        if let Some(model) = self.model_info() {
            for control in &model.controls {
                if Kind::from_id(&self.summary.provider) == Kind::OpenCode && control.id == "agent"
                {
                    continue;
                }
                let selected = self
                    .settings
                    .model_settings
                    .get(&control.id)
                    .unwrap_or(&control.current);
                let label = match control.id.as_str() {
                    "effort" | "reasoning" => "Effort",
                    _ => &control.name,
                };
                if model_control_toggle(control) {
                    items.push(
                        MenuAction::new(format!("model-toggle:{}", control.id), label.to_owned())
                            .toggle(selected == "true"),
                    );
                    continue;
                }
                let choices = control
                    .choices
                    .iter()
                    .map(|(value, name)| {
                        let key =
                            serde_json::to_string(&(control.id.clone(), value.clone())).unwrap();
                        MenuAction::new(format!("model-setting:{key}"), name.clone())
                            .checked(selected == value)
                    })
                    .collect();
                items.push(
                    MenuAction::new(format!("model-control:{}", control.id), label.to_owned())
                        .value(
                            control
                                .choices
                                .iter()
                                .find(|(value, _)| value == selected)
                                .map(|(_, name)| name)
                                .unwrap_or(selected)
                                .clone(),
                        )
                        .submenu(choices),
                );
            }
        }
        items.sort_by_key(|action| {
            let key = action
                .id
                .strip_prefix("model-control:")
                .or_else(|| action.id.strip_prefix("model-toggle:"))
                .unwrap_or(&action.id);
            match key {
                "fast" => 0,
                "lead" => 1,
                "effort" => 2,
                "sidekick" => 3,
                "reasoning" if Kind::from_id(&self.summary.provider) == Kind::Codex => 5,
                "reasoning" => 4,
                "reasoningEffort" | "reasoning_effort" => 5,
                "service-tier" | "serviceTier" => 6,
                "thinking" => 7,
                "variant" => 8,
                "agent" => 9,
                "context" => 10,
                _ => 99,
            }
        });
        items.into_iter().map(MenuItem::from).collect()
    }

    /// `ModelPicker` with `hideSettings`: its trigger opens the model list.
    pub(super) fn toggle_model_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.controls_ui.models.is_some() {
            self.close_model_list(window, cx);
            return;
        }
        self.menu = None;
        self.completion
            .update(cx, |completion, cx| completion.dismiss(cx));
        self.git_picker = None;
        self.git_events = None;
        let provider = Kind::from_id(&self.summary.provider);
        let list = cx.new(|cx| {
            let hidden = cx
                .try_global::<crate::settings::Preferences>()
                .map(|p| p.hidden_for(&self.summary.cwd))
                .unwrap_or_default();
            ModelList::new_for(
                provider,
                self.model.clone(),
                &self.summary.cwd,
                self.settings.account.clone(),
                cx,
            )
            .hide_providers(hidden)
            .allow_provider_switch(true)
        });
        let events = vec![cx.subscribe_in(
            &list,
            window,
            |this, _, picked: &ModelPicked, window, cx| {
                if this.apply_model_pick(picked, cx) {
                    this.close_model_list(window, cx);
                }
            },
        )];
        window.focus(&list.read(cx).search_focus(cx), cx);
        self.controls_ui.models = Some((list, events));
        cx.notify();
    }

    pub(super) fn model_list_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (list, _) = self.controls_ui.models.as_ref()?;
        let this = cx.entity().downgrade();
        Some(
            popover("composer-models", Anchor::Slot(self.model_anchor.clone()))
                .side(Side::Top)
                .align(Align::End)
                .width(310.)
                .height(442.)
                .on_dismiss(move |window, cx| {
                    let _ = this.update(cx, |this, cx| this.close_model_list(window, cx));
                })
                .child(list.clone())
                .into_any_element(),
        )
    }

    fn close_model_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.controls_ui.models = None;
        window.focus(&self.input.focus_handle(cx), cx);
        cx.notify();
    }

    /// ModelControlPills' select menus: effort (with fast and service tier
    /// grouped under it), then each provider control.
    pub(super) fn setting_menu(&self, ix: usize) -> (&'static str, Vec<MenuItem>, f32, MenuStyle) {
        let Some(model) = self.model_info() else {
            return ("Model setting", Vec::new(), 210., MenuStyle::Explorer);
        };
        let mut items = Vec::new();
        if ix == 0 {
            let group = |items: &mut Vec<MenuItem>, label: &str, rows: Vec<MenuAction>| {
                if !items.is_empty() {
                    items.push(MenuItem::Separator);
                }
                items.push(MenuItem::Header(label.to_owned().into()));
                items.extend(rows.into_iter().map(MenuItem::from));
            };
            if !model.efforts.is_empty() {
                group(&mut items, self.effort_setting_label(), self.effort_items());
            }
            if model.fast {
                group(
                    &mut items,
                    "Fast",
                    vec![
                        MenuAction::new("fast:on", "On").checked(self.settings.fast),
                        MenuAction::new("fast:off", "Off").checked(!self.settings.fast),
                    ],
                );
            }
            if !model.tiers.is_empty() {
                let mut tiers = vec![
                    MenuAction::new("tier:", "Standard").checked(self.selected_tier().is_none()),
                ];
                tiers.extend(model.tiers.iter().map(|(id, name)| {
                    MenuAction::new(format!("tier:{id}"), name.clone())
                        .checked(self.selected_tier() == Some(id.as_str()))
                }));
                group(&mut items, "Service Tier", tiers);
            }
            if items
                .iter()
                .filter(|i| matches!(i, MenuItem::Header(_)))
                .count()
                == 1
            {
                items.retain(|i| !matches!(i, MenuItem::Header(_)));
            }
            return ("Effort", items, 210., MenuStyle::Explorer);
        }
        if let Some(control) = model.controls.get(ix - 1) {
            let selected = self
                .settings
                .model_settings
                .get(&control.id)
                .unwrap_or(&control.current);
            items.extend(control.choices.iter().map(|(value, name)| {
                let key = serde_json::to_string(&(control.id.clone(), value.clone())).unwrap();
                MenuItem::from(
                    MenuAction::new(format!("model-setting:{key}"), name.clone())
                        .checked(selected == value),
                )
            }));
        }
        ("Model setting", items, 210., MenuStyle::Explorer)
    }

    pub(super) fn model_controls(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let text_control = |id: SharedString, open: bool| {
            div()
                .id(id)
                .role(Role::Button)
                .flex_shrink_0()
                .h(px(26.))
                .min_w_0()
                .px(px(10.))
                .gap_1p5()
                .rounded_full()
                .flex()
                .items_center()
                .text_size(px(12.))
                .text_color(content(if open { 1. } else { 0.55 }))
                .when(open, |d| d.bg(content(0.08)))
                .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        };
        let model = self.model_info().cloned();
        let mut pills: Vec<AnyElement> = Vec::new();
        if let Some(model) = &model {
            let anchors = 1 + model.controls.len();
            if self.controls_ui.setting_anchors.len() < anchors {
                self.controls_ui
                    .setting_anchors
                    .resize_with(anchors, AnchorSlot::default);
            }
            let grouped = !model.efforts.is_empty() || !model.tiers.is_empty();
            if grouped {
                let open = self.menu_is(ComposerMenu::Setting(0));
                let (glyph, label, value) = if model.efforts.is_empty() {
                    (
                        "zap",
                        "Service Tier",
                        self.selected_tier()
                            .and_then(|id| model.tiers.iter().find(|(t, _)| t == id))
                            .map_or("Standard".into(), |(_, name)| name.clone()),
                    )
                } else {
                    (
                        "gauge",
                        self.effort_setting_label(),
                        self.effort_label().to_string(),
                    )
                };
                pills.push(
                    text_control("composer-effort".into(), open)
                        .max_w(px(112.))
                        .aria_label(format!("{label}: {value}"))
                        .aria_expanded(open)
                        .child(self.controls_ui.setting_anchors[0].probe())
                        .child(icon(glyph, 14.).text_color(content(if open { 1. } else { 0.55 })))
                        .child(div().min_w_0().truncate().child(value))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_menu(ComposerMenu::Setting(0), window, cx)
                        }))
                        .into_any_element(),
                );
            }
            if model.fast && !grouped {
                let on = self.settings.fast;
                pills.push(
                    text_control("composer-fast".into(), false)
                        .aria_label(format!("Fast mode: {}", if on { "On" } else { "Off" }))
                        .child(div().when(!on, |d| d.opacity(0.5)).child("Fast"))
                        .on_click(cx.listener(|this, _, _, cx| this.menu_choice("fast", cx)))
                        .into_any_element(),
                );
            }
            if model.thinking {
                let on = self.settings.thinking.unwrap_or(false);
                pills.push(
                    text_control("composer-thinking".into(), false)
                        .aria_label(format!("Thinking: {}", if on { "On" } else { "Off" }))
                        .child(div().when(!on, |d| d.opacity(0.5)).child("Thinking"))
                        .on_click(cx.listener(|this, _, _, cx| this.menu_choice("thinking", cx)))
                        .into_any_element(),
                );
            }
            for (ix, control) in model.controls.iter().enumerate() {
                if Kind::from_id(&self.summary.provider) == Kind::OpenCode && control.id == "agent"
                {
                    continue;
                }
                let kind = ComposerMenu::Setting(ix + 1);
                let toggle = model_control_toggle(control);
                let open = !toggle && self.menu_is(kind);
                let selected = self
                    .settings
                    .model_settings
                    .get(&control.id)
                    .unwrap_or(&control.current);
                let value = control
                    .choices
                    .iter()
                    .find(|(value, _)| value == selected)
                    .map_or(selected.clone(), |(_, name)| name.clone());
                let toggle_id = format!("model-toggle:{}", control.id);
                let on = selected == "true";
                let title = if toggle {
                    format!("{}: {}", control.name, if on { "On" } else { "Off" })
                } else {
                    format!("{}: {value}", control.name)
                };
                pills.push(
                    text_control(format!("composer-control-{ix}").into(), open)
                        .max_w(px(112.))
                        .aria_label(title)
                        .when(!toggle, |d| {
                            d.aria_expanded(open)
                                .child(self.controls_ui.setting_anchors[ix + 1].probe())
                        })
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .when(toggle && !on, |d| d.opacity(0.5))
                                .child(if toggle { control.name.clone() } else { value }),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if toggle {
                                this.menu_choice(&toggle_id, cx);
                            } else {
                                this.toggle_menu(kind, window, cx);
                            }
                        }))
                        .into_any_element(),
                );
            }
        }
        pills
    }
}
