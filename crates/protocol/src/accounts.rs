//! Provider accounts: which account a session or a background chore runs on, and the book of
//! every account a person added. Where an account keeps its files is the engine's.
mod desktop;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use self::desktop::DesktopOverrides;
use crate::provider::Kind;

pub const DEFAULT: &str = "default";
pub const PROVIDERS: [Kind; 3] = [Kind::Claude, Kind::Codex, Kind::Devin];
fn default_id() -> String {
    DEFAULT.into()
}
/// A fresh random (v4) UUID, hyphenated and lowercase.
pub fn request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    /// Supplied by the native store, never accepted from persisted settings.
    #[serde(skip)]
    pub root: Option<PathBuf>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            id: default_id(),
            root: None,
        }
    }
}
impl Profile {
    pub fn new(id: &str, root: &Path) -> Self {
        if id == DEFAULT {
            return Self::default();
        }
        Self {
            id: id.into(),
            root: Some(root.into()),
        }
    }
}

/// Who an account is signed in as, as its provider reports it.
#[derive(Clone, Serialize, Deserialize, Default, Debug, PartialEq, Eq)]
pub struct Identity {
    pub email: Option<String>,
    pub name: Option<String>,
    pub plan: Option<String>,
    pub organization: Option<String>,
}

/// Signing in or out of a provider's own CLI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthAction {
    SignIn,
    SignOut,
}

/// The provider CLI's arguments for `action`, when it has a command for it.
pub fn login_arguments(kind: Kind, action: AuthAction) -> Option<&'static [&'static str]> {
    match (kind, action) {
        (Kind::Claude | Kind::Devin, AuthAction::SignIn) => Some(&["auth", "login"]),
        (Kind::Claude | Kind::Devin, AuthAction::SignOut) => Some(&["auth", "logout"]),
        (Kind::Codex | Kind::Cursor, AuthAction::SignIn) => Some(&["login"]),
        (Kind::Codex | Kind::Cursor, AuthAction::SignOut) => Some(&["logout"]),
        (Kind::Grok, AuthAction::SignIn) => Some(&["login", "--oauth"]),
        (Kind::Fx, AuthAction::SignIn) => Some(&["login", "vercel"]),
        _ => None,
    }
}

pub fn supports_profiles(kind: Kind) -> bool {
    matches!(kind, Kind::Codex | Kind::Claude)
}
pub fn supports_accounts(kind: Kind) -> bool {
    PROVIDERS.contains(&kind)
}
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 80
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        Err("Invalid provider account id".into())
    } else {
        Ok(())
    }
}
fn clean_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(48)
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub provider: String,
    pub label: String,
}
impl Account {
    pub fn is_default(&self) -> bool {
        self.id == DEFAULT
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Book {
    pub accounts: Vec<Account>,
    pub selections: HashMap<String, HashMap<String, String>>,
}
impl Book {
    pub fn list(&self, kind: Kind) -> Vec<Account> {
        if !supports_accounts(kind) {
            return Vec::new();
        }
        let label = self
            .accounts
            .iter()
            .find(|a| a.provider == kind.id() && a.is_default())
            .map(|a| clean_label(&a.label))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Default account".into());
        let mut rows = vec![Account {
            id: DEFAULT.into(),
            provider: kind.id().into(),
            label,
        }];
        if supports_profiles(kind) {
            for a in self
                .accounts
                .iter()
                .filter(|a| a.provider == kind.id() && !a.is_default())
            {
                let label = clean_label(&a.label);
                if validate_id(&a.id).is_ok()
                    && !label.is_empty()
                    && !rows.iter().any(|row| row.id == a.id)
                {
                    rows.push(Account { label, ..a.clone() });
                }
            }
        }
        rows
    }

    pub fn add(&mut self, kind: Kind, label: &str) -> Result<Account, String> {
        if !supports_profiles(kind) {
            return Err("This provider supports only its default account".into());
        }
        let label = clean_label(label);
        let account = Account {
            id: format!("account-{}", request_id()),
            provider: kind.id().into(),
            label: if label.is_empty() {
                format!("Account {}", self.list(kind).len() + 1)
            } else {
                label
            },
        };
        self.accounts.push(account.clone());
        Ok(account)
    }

    pub fn rename(&mut self, kind: Kind, id: &str, label: &str) -> Result<(), String> {
        let label = clean_label(label);
        if label.is_empty() || !self.exists(kind, id) {
            return Err("Account name is empty or account no longer exists".into());
        }
        if let Some(account) = self
            .accounts
            .iter_mut()
            .find(|a| a.provider == kind.id() && a.id == id)
        {
            account.label = label;
        } else {
            self.accounts.push(Account {
                id: id.into(),
                provider: kind.id().into(),
                label,
            });
        }
        Ok(())
    }

    pub fn exists(&self, kind: Kind, id: &str) -> bool {
        self.list(kind).iter().any(|a| a.id == id)
    }

    pub fn remove(&mut self, kind: Kind, id: &str) -> Result<(), String> {
        if id == DEFAULT || !self.exists(kind, id) {
            return Err("This account cannot be removed".into());
        }
        self.accounts
            .retain(|a| a.provider != kind.id() || a.id != id);
        for selections in self.selections.values_mut() {
            if selections.get(kind.id()).is_some_and(|v| v == id) {
                selections.remove(kind.id());
            }
        }
        Ok(())
    }

    pub fn selected(&self, kind: Kind, project: &str) -> String {
        self.selections
            .get(&project_key(project))
            .and_then(|s| s.get(kind.id()))
            .filter(|id| self.exists(kind, id))
            .cloned()
            .unwrap_or_else(default_id)
    }

    pub fn select(&mut self, kind: Kind, project: &str, id: &str) -> Result<(), String> {
        if !self.exists(kind, id) {
            return Err("Provider account no longer exists".into());
        }
        self.selections
            .entry(project_key(project))
            .or_default()
            .insert(kind.id().into(), id.into());
        Ok(())
    }
}
fn project_key(project: &str) -> String {
    let key = project.trim().replace('\\', "/");
    let key = key.trim_end_matches('/');
    let key = if key.is_empty() { "~" } else { key };
    key.into()
}
