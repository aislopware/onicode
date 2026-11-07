//! The Engine settings group: what an engine does on its own behalf, set once for every viewer
//! ([`WatchEngineSettings`], [`SetEngineSetting`]). A viewer's own look and layout (theme,
//! panels, keybindings, sounds, the window) are the viewer's preferences and never come here.
//!
//! Each [`EngineSetting`] is one typed key with its whole value: setting it replaces that value,
//! so a viewer echoes a change in the frame it makes it and the event that follows says the same.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{Call, Cursor, OpId, Outcome, Watch};

/// A provider and, optionally, one of its models: the background model, or a picker's choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    pub provider: String,
    pub model: Option<String>,
}

/// What a project overrides of the new-session defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderDefaults {
    pub provider: Option<String>,
    /// By provider.
    pub models: BTreeMap<String, String>,
    /// Providers this project's pickers leave out.
    pub hidden: Vec<String>,
}

/// Everything an engine is set to do. Defaults are monocode's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    /// Hold off idle system sleep while a turn runs or a terminal runs a program in the
    /// foreground (`docs/onicode/decisions/keep-awake.md`).
    pub keep_awake_while_agents_run: bool,
    /// Claude Code runs the person's own hooks.
    pub claude_hooks: bool,
    /// The model for session titles, branch names, commit messages and pull requests; `None`
    /// lets each use its provider's quick model.
    pub background_model: Option<ModelChoice>,
    /// A provider CLI's path, by provider, over the one the engine finds.
    pub binary_paths: BTreeMap<String, String>,
    /// Skill files Settings turned off.
    pub disabled_skill_paths: Vec<String>,
    /// Extra folders skills are read from.
    pub skill_source_paths: Vec<String>,
    /// How many idle agents stay parked, warm for their sessions.
    pub idle_agents: u32,
    /// The provider a new chat starts on.
    pub default_provider: Option<String>,
    /// The model a new chat starts on, by provider.
    pub default_models: BTreeMap<String, String>,
    /// Providers every picker leaves out.
    pub hidden_providers: Vec<String>,
    /// Each project's overrides, by project path.
    pub project_providers: BTreeMap<String, ProviderDefaults>,
    /// The access mode new chats start with: the last one a composer picked.
    pub default_access: Option<String>,
    /// The last effort picked for each provider, shared across projects.
    pub provider_efforts: BTreeMap<String, String>,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            keep_awake_while_agents_run: true,
            claude_hooks: true,
            background_model: None,
            binary_paths: BTreeMap::new(),
            disabled_skill_paths: Vec::new(),
            skill_source_paths: Vec::new(),
            idle_agents: 3,
            default_provider: None,
            default_models: BTreeMap::new(),
            hidden_providers: Vec::new(),
            project_providers: BTreeMap::new(),
            default_access: None,
            provider_efforts: BTreeMap::new(),
        }
    }
}

/// One setting with its whole value: the typed keys of [`SetEngineSetting`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineSetting {
    KeepAwakeWhileAgentsRun(bool),
    ClaudeHooks(bool),
    BackgroundModel(Option<ModelChoice>),
    BinaryPaths(BTreeMap<String, String>),
    DisabledSkillPaths(Vec<String>),
    SkillSourcePaths(Vec<String>),
    IdleAgents(u32),
    DefaultProvider(Option<String>),
    DefaultModels(BTreeMap<String, String>),
    HiddenProviders(Vec<String>),
    ProjectProviders(BTreeMap<String, ProviderDefaults>),
    DefaultAccess(Option<String>),
    ProviderEfforts(BTreeMap<String, String>),
}

impl EngineSetting {
    /// The key's name, the same for every value: two settings of one key replace each other.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Self::KeepAwakeWhileAgentsRun(_) => "keep-awake-while-agents-run",
            Self::ClaudeHooks(_) => "claude-hooks",
            Self::BackgroundModel(_) => "background-model",
            Self::BinaryPaths(_) => "binary-paths",
            Self::DisabledSkillPaths(_) => "disabled-skill-paths",
            Self::SkillSourcePaths(_) => "skill-source-paths",
            Self::IdleAgents(_) => "idle-agents",
            Self::DefaultProvider(_) => "default-provider",
            Self::DefaultModels(_) => "default-models",
            Self::HiddenProviders(_) => "hidden-providers",
            Self::ProjectProviders(_) => "project-providers",
            Self::DefaultAccess(_) => "default-access",
            Self::ProviderEfforts(_) => "provider-efforts",
        }
    }
}

impl EngineSettings {
    /// Sets one key, on the host and in every viewer's mirror alike.
    pub fn apply(&mut self, setting: &EngineSetting) {
        match setting.clone() {
            EngineSetting::KeepAwakeWhileAgentsRun(value) => {
                self.keep_awake_while_agents_run = value;
            }
            EngineSetting::ClaudeHooks(value) => self.claude_hooks = value,
            EngineSetting::BackgroundModel(value) => self.background_model = value,
            EngineSetting::BinaryPaths(value) => self.binary_paths = value,
            EngineSetting::DisabledSkillPaths(value) => self.disabled_skill_paths = value,
            EngineSetting::SkillSourcePaths(value) => self.skill_source_paths = value,
            EngineSetting::IdleAgents(value) => self.idle_agents = value,
            EngineSetting::DefaultProvider(value) => self.default_provider = value,
            EngineSetting::DefaultModels(value) => self.default_models = value,
            EngineSetting::HiddenProviders(value) => self.hidden_providers = value,
            EngineSetting::ProjectProviders(value) => self.project_providers = value,
            EngineSetting::DefaultAccess(value) => self.default_access = value,
            EngineSetting::ProviderEfforts(value) => self.provider_efforts = value,
        }
    }

    /// Every key this one holds differently from `before`, as the settings that make it so: what
    /// a viewer sends after an edit.
    #[must_use]
    pub fn changes_from(&self, before: &Self) -> Vec<EngineSetting> {
        self.all()
            .into_iter()
            .zip(before.all())
            .filter(|(now, then)| now != then)
            .map(|(now, _)| now)
            .collect()
    }

    /// Every key with its value, in one order.
    #[must_use]
    pub fn all(&self) -> Vec<EngineSetting> {
        vec![
            EngineSetting::KeepAwakeWhileAgentsRun(self.keep_awake_while_agents_run),
            EngineSetting::ClaudeHooks(self.claude_hooks),
            EngineSetting::BackgroundModel(self.background_model.clone()),
            EngineSetting::BinaryPaths(self.binary_paths.clone()),
            EngineSetting::DisabledSkillPaths(self.disabled_skill_paths.clone()),
            EngineSetting::SkillSourcePaths(self.skill_source_paths.clone()),
            EngineSetting::IdleAgents(self.idle_agents),
            EngineSetting::DefaultProvider(self.default_provider.clone()),
            EngineSetting::DefaultModels(self.default_models.clone()),
            EngineSetting::HiddenProviders(self.hidden_providers.clone()),
            EngineSetting::ProjectProviders(self.project_providers.clone()),
            EngineSetting::DefaultAccess(self.default_access.clone()),
            EngineSetting::ProviderEfforts(self.provider_efforts.clone()),
        ]
    }

    /// The providers `project`'s pickers leave out: the hidden ones, and the project's own.
    #[must_use]
    pub fn hidden_for(&self, project: &str) -> Vec<String> {
        let mut hidden = self.hidden_providers.clone();
        if let Some(settings) = self.project_providers.get(project) {
            hidden.extend(settings.hidden.iter().cloned());
        }
        hidden.sort();
        hidden.dedup();
        hidden
    }
}

/// One change to the engine settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EngineSettingsEvent {
    /// A key took this value.
    Set(EngineSetting),
    /// What became of a mutating call: its effect, if any, came before.
    Receipt { op: OpId, outcome: Outcome },
}

/// Watch the engine settings: a snapshot (or, with a `since` the engine's ring still covers, only
/// the events after it), then every event.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchEngineSettings {
    pub since: Option<Cursor>,
}

impl Watch for WatchEngineSettings {
    type Event = EngineSettingsEvent;
    type Params = Self;
    type State = EngineSettings;

    const NAME: &'static str = "WatchEngineSettings";
}

/// Set one key. A value equal to the one held changes nothing and is still `Applied`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetEngineSetting {
    pub setting: EngineSetting,
}

impl Call for SetEngineSetting {
    type Params = Self;
    type Reply = Outcome;

    const NAME: &'static str = "SetEngineSetting";
}

/// What the engine's store kept for a viewer before viewers kept their own: the saved layout and
/// preferences (`workspace`), and the markers the app kept beside them. The engine hands them over
/// once and forgets them, so a viewer moves them into its own state at its first launch and a
/// later launch finds nothing to move. A repeat of the op gets the same reply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeViewerState {}

/// [`TakeViewerState`]'s reply: each kept value under the key the store held it by.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewerState {
    pub values: BTreeMap<String, String>,
}

impl Call for TakeViewerState {
    type Params = Self;
    type Reply = ViewerState;

    const NAME: &'static str = "TakeViewerState";
}
