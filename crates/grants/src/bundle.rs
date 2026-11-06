//! Where desktopd lives inside `Onicode.app`, as `scripts/bundle-macos.sh`,
//! `scripts/package-macos.sh` and `scripts/run-macos-dev.sh` lay it out.
//!
//! ```text
//! Onicode.app/Contents/
//!   Library/LoginItems/Onicode Desktop.app      the helper: desktopd, its icon, LSUIElement
//!   Library/LaunchAgents/<app id>.desktop.plist  SMAppService's agent: BundleProgram, KeepAlive
//! ```

use std::path::{Path, PathBuf};

/// The helper bundle's folder name, the same in release and development builds; a development
/// build's helper shows as "Onicode Dev Desktop" through its display name only.
pub const HELPER_APP: &str = "Onicode Desktop.app";

/// The helper's executable, `onicode-desktopd` renamed for Activity Monitor.
pub const HELPER_EXECUTABLE: &str = "Onicode Desktop";

/// What the helper's bundle id and launchd label add to the app's bundle id.
pub const ID_SUFFIX: &str = ".desktop";

/// The helper bundle inside the app bundle at `app`.
#[must_use]
pub fn helper_bundle(app: &Path) -> PathBuf {
    app.join("Contents/Library/LoginItems").join(HELPER_APP)
}

/// The helper's bundle id, which is also desktopd's launchd label and code-signing identifier:
/// `dev.aislopware.onicode.desktop` for the app `dev.aislopware.onicode`.
#[must_use]
pub fn helper_id(app_id: &str) -> String {
    format!("{app_id}{ID_SUFFIX}")
}

/// The agent plist's file name in `Contents/Library/LaunchAgents`, as
/// `SMAppService.agent(plistName:)` takes it.
#[must_use]
pub fn agent_plist(app_id: &str) -> String {
    format!("{}.plist", helper_id(app_id))
}

/// The app bundle around a helper bundle at `helper`, when it sits where [`helper_bundle`] puts
/// it.
#[must_use]
pub fn app_of_helper(helper: &Path) -> Option<&Path> {
    let login_items = helper.parent()?;
    let library = login_items.parent()?;
    let contents = library.parent()?;
    let fits = login_items.file_name()? == "LoginItems"
        && library.file_name()? == "Library"
        && contents.file_name()? == "Contents";
    fits.then(|| contents.parent()).flatten()
}

/// The name to show for a helper bundle: its folder name without `.app`.
#[must_use]
pub fn display_name(helper: &Path) -> String {
    helper.file_stem().map_or_else(
        || "Onicode Desktop".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn the_helper_sits_in_login_items() {
        let app = Path::new("/Applications/Onicode.app");
        let helper = helper_bundle(app);
        assert_eq!(
            helper,
            Path::new("/Applications/Onicode.app/Contents/Library/LoginItems/Onicode Desktop.app")
        );
        assert_eq!(app_of_helper(&helper), Some(app));
        assert_eq!(app_of_helper(Path::new("/tmp/Onicode Desktop.app")), None);
        assert_eq!(display_name(&helper), "Onicode Desktop");
    }

    #[test]
    fn ids_follow_the_app() {
        assert_eq!(
            helper_id("dev.aislopware.onicode"),
            "dev.aislopware.onicode.desktop"
        );
        assert_eq!(
            agent_plist("dev.aislopware.onicode.dev"),
            "dev.aislopware.onicode.dev.desktop.plist"
        );
    }
}
