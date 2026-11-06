//! Application storage paths. Provider credentials keep their own locations.

use std::ffi::OsString;
use std::path::PathBuf;

pub fn data_dir() -> PathBuf {
    resolve_data_dir(|name| std::env::var_os(name))
}

/// [`data_dir`] from `env` (`ONICODE_DATA_DIR`, `HOME`): for an app resolving
/// it as Onicode does.
pub fn resolve_data_dir(mut env: impl FnMut(&str) -> Option<OsString>) -> PathBuf {
    if let Some(dir) = env("ONICODE_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = PathBuf::from(env("HOME").expect("HOME not set"));
    let dir = home.join(".onicode");
    // One-shot 0.2.0 migration: adopt the pre-rename data dir.
    if !dir.exists() {
        let old = home.join(".comet-native");
        if old.exists() && std::fs::rename(&old, &dir).is_ok() {
            eprintln!("migrated data dir {} -> {}", old.display(), dir.display());
        }
    }
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(vars: &[(&str, &str)]) -> PathBuf {
        resolve_data_dir(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        })
    }

    #[test]
    fn explicit_data_dir_needs_no_home() {
        assert_eq!(
            resolve(&[("ONICODE_DATA_DIR", "custom data")]),
            PathBuf::from("custom data")
        );
    }
}
