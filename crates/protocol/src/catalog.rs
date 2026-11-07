//! The models a provider offers. Finding them is the engine's.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One model a provider offers, with the controls its pickers show.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    pub name: String,
    /// Concrete id behind a moving provider alias, retained for comparisons.
    #[serde(default)]
    pub alias_target: Option<String>,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
    pub fast: bool,
    pub thinking: bool,
    pub default: bool,
    pub tiers: Vec<(String, String)>,
    #[serde(default)]
    pub controls: Vec<ModelControl>,
    /// The token limits the provider reports for this model, each on its own: a provider that
    /// reports only some, or none, still lists the model (opencode #62240).
    #[serde(default)]
    pub limits: ModelLimits,
}
/// A model's context and output limits in tokens. Every field is optional and read on its own, so
/// a missing or malformed one never hides the model; [`ModelLimits::or`] fills a missing field
/// from a fallback, field by field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelLimits {
    pub context: Option<u64>,
    pub output: Option<u64>,
}
impl ModelLimits {
    /// Each field this one lacks, taken from `fallback`.
    #[must_use]
    pub fn or(self, fallback: Self) -> Self {
        Self {
            context: self.context.or(fallback.context),
            output: self.output.or(fallback.output),
        }
    }

    /// The limits a catalog row reports, in any of the shapes providers use: `limit.context`
    /// and `limit.output` (models.dev, OpenCode), `contextWindow` and `maxOutputTokens`, or their
    /// snake-case forms. A field missing or not a positive whole number is `None`.
    #[must_use]
    pub fn of(row: &Value) -> Self {
        let first = |paths: &[&[&str]]| {
            paths.iter().find_map(|path| {
                let value = path.iter().try_fold(row, |value, key| value.get(*key))?;
                value.as_u64().filter(|n| *n > 0)
            })
        };
        Self {
            context: first(&[
                &["limit", "context"],
                &["limits", "context"],
                &["contextWindow"],
                &["context_window"],
                &["contextLength"],
            ]),
            output: first(&[
                &["limit", "output"],
                &["limits", "output"],
                &["maxOutputTokens"],
                &["max_output_tokens"],
                &["outputLimit"],
            ]),
        }
    }
}
/// A provider-advertised control beyond the common effort and speed switches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelControl {
    pub id: String,
    pub name: String,
    pub current: String,
    pub choices: Vec<(String, String)>,
}
