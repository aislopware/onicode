//! Provider-owned sign-ins. Metadata never contains credentials.
pub(crate) mod login;
mod status;
pub use status::claude_auth_status;
mod transfer;
pub use login::{authenticate, sign_out_for_removal};
mod processes;
#[cfg(test)]
mod tests;
use std::path::PathBuf;
use std::process::Command;

pub use onicode_protocol::accounts::*;
pub use processes::stop_processes;
pub(crate) use processes::{Tracked, track};

use crate::provider::Kind;

/// What a [`Profile`] finds and changes on the disk: its private home, and the CLI it runs.
pub trait ProfileExt {
    fn directory(&self, kind: Kind) -> Result<Option<PathBuf>, String>;
    fn home(&self, kind: Kind) -> Result<PathBuf, String>;
    fn apply(&self, kind: Kind, command: &mut Command) -> Result<(), String>;
    fn transfer(&self, to: &Self, kind: Kind, thread: &str) -> Result<(), String>;
    fn remove_credentials(&self, kind: Kind) -> Result<(), String>;
}

impl ProfileExt for Profile {
    fn directory(&self, kind: Kind) -> Result<Option<PathBuf>, String> {
        if self.id == DEFAULT {
            return Ok(None);
        }
        if !supports_profiles(kind) {
            return Err("Provider account profiles are not supported for this provider".into());
        }
        validate_id(&self.id)?;
        let root = self
            .root
            .as_ref()
            .ok_or("Provider account storage is unavailable")?;
        Ok(Some(
            root.join("provider-accounts")
                .join(kind.id())
                .join(&self.id),
        ))
    }

    fn home(&self, kind: Kind) -> Result<PathBuf, String> {
        if let Some(dir) = self.directory(kind)? {
            return Ok(dir);
        }
        let (var, dir) = match kind {
            Kind::Codex => ("CODEX_HOME", ".codex"),
            Kind::Claude => ("CLAUDE_CONFIG_DIR", ".claude"),
            _ => {
                return Err("Native transcript transfer is not supported for this provider".into());
            }
        };
        std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home().map(|p| p.join(dir)))
            .ok_or_else(|| format!("Could not find the {} home directory", kind.label()))
    }

    fn apply(&self, kind: Kind, command: &mut Command) -> Result<(), String> {
        let Some(dir) = self.directory(kind)? else {
            return Ok(());
        };
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Could not create the account directory: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        match kind {
            Kind::Claude => {
                command
                    .env("CLAUDE_CONFIG_DIR", &dir)
                    .env("CLAUDE_SECURESTORAGE_CONFIG_DIR", &dir)
                    .env_remove("ANTHROPIC_API_KEY")
                    .env_remove("ANTHROPIC_AUTH_TOKEN")
                    .env_remove("CLAUDE_CODE_OAUTH_TOKEN");
            }
            Kind::Codex => {
                command
                    .env("CODEX_HOME", &dir)
                    .env_remove("OPENAI_API_KEY")
                    .env_remove("CODEX_API_KEY")
                    .env_remove("CODEX_ACCESS_TOKEN");
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn transfer(&self, to: &Self, kind: Kind, thread: &str) -> Result<(), String> {
        transfer::transfer(self, to, kind, thread)
    }

    fn remove_credentials(&self, kind: Kind) -> Result<(), String> {
        let dir = self
            .directory(kind)?
            .ok_or("The default provider account cannot be removed")?;
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                std::fs::remove_dir_all(dir).map_err(|e| e.to_string())
            }
            Ok(_) => std::fs::remove_file(dir).map_err(|e| e.to_string()),
        }
    }
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}
