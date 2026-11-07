//! App-wide preferences the Settings pages edit. Defaults are the React
//! app's (`settings.ts`, `appearance.ts`, `displayPrefs.ts`, `sounds.ts`).
//!
//! The app's own look and layout are saved with the workspace in its `ui.sqlite3`. What the engine
//! does on its own behalf (providers, models, skills, idle agents, hooks) is the engine's: the
//! Settings pages edit it in [`Preferences::engine`], which the shell mirrors from the engine and
//! sends back to it, key by key (`shell/engine_settings_mirror.rs`). Each project's notification
//! preferences belong to the engine that holds the project: the Settings pages edit them in
//! [`Preferences::project_notifications`], which the shell follows from every engine and sends to
//! the one holding each project (`shell/project_notifications.rs`).
use std::collections::{BTreeMap, HashMap};

use onicode_protocol::alerts::ProjectNotifications;
use onicode_protocol::engine_settings::EngineSettings;
pub(crate) use onicode_protocol::engine_settings::ModelChoice;
use onicode_protocol::provider::Kind;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Preferences {
    // General
    pub sounds: bool,
    pub notifications: bool,
    /// Every project's notification preferences, by its folder as the app spells it (a project
    /// on another engine by its `peer://` URI), as each engine holds them: never saved with the
    /// workspace.
    #[serde(skip)]
    pub project_notifications: BTreeMap<String, ProjectNotifications>,
    pub file_tabs: String,
    pub file_word_wrap: bool,
    pub file_view_mode: String,
    pub tab_animations: bool,
    pub notes: bool,
    pub quick_composer: bool,
    /// Global search floats as a command panel instead of opening a page.
    pub search_panel: bool,
    pub working_agents: bool,
    pub close_to_tray: bool,
    /// Running an action shows the terminal it runs in.
    pub action_opens_terminal: bool,
    /// Legacy: the new layout opens the terminal as a right panel tab, not a
    /// dock. Read when `terminal_placement` is unset, and kept in step with it.
    pub terminal_in_panel: bool,
    /// Where terminals open: "bottom" (the dock), "panel" (the right panel's
    /// Terminal tab) or "title" (their own title bar tabs). Empty falls back
    /// to `terminal_in_panel`.
    pub terminal_placement: String,
    // Appearance
    pub theme: String,
    pub workspace_theme: String,
    // Retain the legacy inset-only key when reading and saving preferences.
    // Window materials now use the shared opacity and main-pane glass values.
    pub inset_glass: bool,
    pub accent_color: Option<String>,
    pub diff_palette: String,
    pub theme_hue: u32,
    pub theme_saturation: u32,
    pub theme_dark_lightness: u32,
    pub sidebar_opacity: u32,
    // Retain the saved value for compatibility; native window blur is fixed.
    pub sidebar_blur: u32,
    pub body_glass: bool,
    pub chat_background: Option<String>,
    pub chat_background_effect: String,
    pub chat_background_scope: String,
    pub chat_background_empty: u32,
    pub chat_background_session: u32,
    pub collapsed_project_rail: String,
    pub ui_scale: u32,
    pub show_excluded_files: bool,
    // Chat
    pub transcript_layout: String,
    pub anchor_prompts: bool,
    pub paced_reveal: bool,
    /// Tool groups stay closed, even while they run, until clicked open.
    pub collapse_tool_groups: bool,
    /// A finished reply folds its work under "worked for"; off keeps the
    /// text and tool groups inline.
    pub fold_finished_work: bool,
    pub follow_up: String,
    pub model_controls: String,
    pub autocorrect: bool,
    pub autosave: bool,
    pub diff_view: String,
    /// What fills an empty session: "ink" or "none".
    pub empty_session: String,
    // Providers
    pub show_remaining_usage: bool,
    pub mask_emails: bool,
    pub favorite_models: Vec<ModelChoice>,
    pub recent_models: Vec<ModelChoice>,
    pub model_picker_tab: Option<String>,
    /// The engine's settings as the shell mirrors them: never saved with the workspace.
    #[serde(skip)]
    pub engine: EngineSettings,
    /// The macOS menu-bar status item that opens the quick composer.
    pub menu_bar_icon: bool,
    // Archive
    pub show_archived: bool,
    // Keybindings: a changed shortcut, or `None` for a disabled one.
    pub keybindings: HashMap<String, Option<String>>,
}

pub(crate) const UI_SCALE_PERCENTS: [u32; 16] = [
    50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200,
];

impl Default for Preferences {
    fn default() -> Self {
        Self {
            sounds: true,
            notifications: false,
            project_notifications: BTreeMap::new(),
            file_tabs: "pane".into(),
            file_word_wrap: true,
            file_view_mode: "preview".into(),
            tab_animations: false,
            notes: true,
            quick_composer: true,
            search_panel: false,
            working_agents: true,
            close_to_tray: true,
            action_opens_terminal: true,
            terminal_in_panel: false,
            terminal_placement: String::new(),
            theme: "dark".into(),
            workspace_theme: "default".into(),
            inset_glass: true,
            accent_color: None,
            diff_palette: "default".into(),
            theme_hue: 240,
            theme_saturation: 0,
            theme_dark_lightness: 9,
            sidebar_opacity: 85,
            sidebar_blur: 24,
            body_glass: true,
            chat_background: None,
            chat_background_effect: "none".into(),
            chat_background_scope: "all".into(),
            chat_background_empty: 24,
            chat_background_session: 24,
            collapsed_project_rail: "compact".into(),
            ui_scale: 100,
            show_excluded_files: false,
            transcript_layout: "chat".into(),
            anchor_prompts: true,
            paced_reveal: true,
            collapse_tool_groups: false,
            fold_finished_work: true,
            follow_up: "steer".into(),
            model_controls: "menu".into(),
            autocorrect: false,
            autosave: false,
            diff_view: "editor".into(),
            empty_session: "ink".into(),
            show_remaining_usage: false,
            mask_emails: true,
            favorite_models: Vec::new(),
            recent_models: Vec::new(),
            model_picker_tab: None,
            engine: EngineSettings::default(),
            menu_bar_icon: true,
            show_archived: false,
            keybindings: HashMap::new(),
        }
    }
}

impl Preferences {
    /// "bottom", "panel" or "title".
    pub(crate) fn terminal_placement(&self) -> &str {
        match self.terminal_placement.as_str() {
            placement @ ("bottom" | "panel" | "title") => placement,
            _ if self.terminal_in_panel => "panel",
            _ => "bottom",
        }
    }

    pub(crate) fn set_terminal_placement(&mut self, placement: &str) {
        self.terminal_placement = placement.to_owned();
        self.terminal_in_panel = placement == "panel";
    }

    pub(crate) fn apply_workspace_theme(&mut self, id: String) {
        let color: gpui::Hsla = gpui::rgb(super::palette(&id, false).surface).into();
        self.theme_hue = (color.h * 360.).round() as u32;
        self.theme_saturation = (color.s * 100.).round() as u32;
        self.theme_dark_lightness = ((color.l * 100.).round() as u32).min(30);
        self.workspace_theme = id;
    }

    /// The color settings a project copies when it gets its own theme.
    pub(crate) fn project_theme(&self) -> onicode_protocol::project_appearance::ProjectTheme {
        onicode_protocol::project_appearance::ProjectTheme {
            workspace_theme: self.workspace_theme.clone(),
            accent_color: self.accent_color.clone(),
            hue: self.theme_hue,
            saturation: self.theme_saturation,
            dark_lightness: self.theme_dark_lightness,
        }
    }

    /// SettingsView's Restore defaults: the Appearance page only.
    pub(crate) fn restore_appearance(&mut self) {
        let d = Self::default();
        self.theme = d.theme;
        self.accent_color = d.accent_color;
        self.workspace_theme = d.workspace_theme;
        self.sidebar_opacity = d.sidebar_opacity;
        self.sidebar_blur = d.sidebar_blur;
        self.theme_hue = d.theme_hue;
        self.theme_saturation = d.theme_saturation;
        self.theme_dark_lightness = d.theme_dark_lightness;
        self.body_glass = d.body_glass;
        self.inset_glass = d.inset_glass;
        self.show_excluded_files = d.show_excluded_files;
        self.chat_background_empty = d.chat_background_empty;
        self.chat_background_session = d.chat_background_session;
        self.chat_background_scope = d.chat_background_scope;
        self.diff_palette = d.diff_palette;
        self.chat_background_effect = d.chat_background_effect;
        self.chat_background = d.chat_background;
        self.ui_scale = d.ui_scale;
        self.collapsed_project_rail = d.collapsed_project_rail;
    }
}

impl gpui::Global for Preferences {}
impl Preferences {
    /// The background model picked in Settings, if any.
    pub(crate) fn background_text_model(&self) -> Option<(Kind, Option<String>)> {
        let choice = self.engine.background_model.as_ref()?;
        Some((Kind::parse(&choice.provider)?, choice.model.clone()))
    }

    /// The providers `project`'s pickers leave out.
    pub(crate) fn hidden_for(&self, project: &str) -> Vec<String> {
        self.engine.hidden_for(project)
    }
}

#[cfg(test)]
mod tests {
    use super::Preferences;

    #[test]
    fn engine_settings_stay_out_of_the_saved_preferences() {
        let mut prefs = Preferences::default();
        prefs.engine.idle_agents = 1;
        prefs.engine.default_provider = Some("claude".into());
        let saved = serde_json::to_value(&prefs).unwrap();
        assert!(saved.get("engine").is_none());
        assert!(saved.get("idle_agents").is_none());
        // monocode saved them with the preferences; the engine took them, the app leaves them.
        let restored: Preferences =
            serde_json::from_str(r#"{"idle_agents":1,"default_provider":"claude","sounds":false}"#)
                .unwrap();
        assert_eq!(
            restored.engine,
            onicode_protocol::engine_settings::EngineSettings::default()
        );
        assert!(!restored.sounds);
    }
}
