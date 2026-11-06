//! Every privilege Onicode asks for, as `docs/onicode/decisions/onboarding-grants.md` tables them:
//! who holds it, how it is asked, how its state is read, and where System Settings lists it.

use serde::{Deserialize, Serialize};

/// The deep link's base: System Settings' Privacy & Security extension on macOS 27.
pub(crate) const PRIVACY_URL: &str =
    "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension";

/// Login Items & Extensions, whose "Background App Activity" section lists the helper.
const LOGIN_ITEMS_URL: &str = "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";

/// Notifications. Its `?id=` anchor lands on the root, so the root it is.
const NOTIFICATIONS_URL: &str =
    "x-apple.systempreferences:com.apple.Notifications-Settings.extension";

/// The bundle a privilege belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Holder {
    /// Onicode.app, which its engine and every agent it spawns inherit.
    App,
    /// Onicode Desktop.app, the remote-desktop helper: its rows are read and asked by running it.
    Helper,
}

/// The checklist's sections, in order.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum Group {
    /// The helper that streams and drives this Mac.
    RemoteDesktop,
    /// This Mac viewing another.
    Viewer,
    /// The agents Onicode runs.
    Agents,
    /// Voice.
    Voice,
    /// Banners.
    Alerts,
    /// The clipboard that follows the owner.
    Clipboard,
}

impl Group {
    /// Every group, in the checklist's order.
    pub const ALL: [Self; 6] = [
        Self::RemoteDesktop,
        Self::Viewer,
        Self::Agents,
        Self::Voice,
        Self::Alerts,
        Self::Clipboard,
    ];

    /// The section's title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::RemoteDesktop => "Remote desktop",
            Self::Viewer => "Viewer",
            Self::Agents => "Agents",
            Self::Voice => "Voice",
            Self::Alerts => "Alerts",
            Self::Clipboard => "Clipboard",
        }
    }
}

/// How a privilege is asked for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Ask {
    /// An API raises the system alert, once. After an answer, only System Settings changes it.
    Prompt,
    /// System Settings' list, with the drag panel beside it.
    Drag,
    /// `SMAppService` registration; Login Items when it waits for approval.
    Register,
}

/// How a privilege's state is read. No read ever prompts.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Read {
    /// An API that answers without asking.
    Api,
    /// The helper, started fresh, prints its state.
    Helper,
    /// `opendir` of protected folders; their contents are never read.
    SpotCheck,
    /// No API: the person says it is on, and that is kept.
    Confirmed,
}

/// Whether a grant takes effect only in a new process.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Relaunch {
    /// It is live.
    No,
    /// The app quits and reopens.
    App,
    /// The helper restarts itself (`restart_when_granted`).
    Helper,
}

/// One row of the checklist.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Privilege {
    /// Screen & System Audio Recording, the helper's.
    ScreenRecording,
    /// Accessibility ("Device Control and Data Access" on 27), the helper's.
    Accessibility,
    /// Posting events, the helper's. Listed under Accessibility.
    PostEvent,
    /// The helper as a login item ("Background App Activity").
    BackgroundActivity,
    /// Input Monitoring, for the viewer's keyboard capture.
    InputMonitoring,
    /// Full Disk Access, which agents inherit.
    FullDiskAccess,
    /// Desktop, Documents, Downloads and network and removable volumes, when Full Disk Access is
    /// off.
    FilesAndFolders,
    /// Apple Events to System Events and Finder.
    Automation,
    /// App Management: updating and deleting other apps.
    AppManagement,
    /// Developer Tools: spawned programs skip XProtect's first-run scan.
    DeveloperTools,
    /// The microphone, for voice.
    Microphone,
    /// Banners.
    Notifications,
    /// Paste from Other Apps, for clipboard sync.
    Pasteboard,
}

impl Privilege {
    /// Every privilege, in the checklist's order.
    pub const ALL: [Self; 13] = [
        Self::ScreenRecording,
        Self::Accessibility,
        Self::PostEvent,
        Self::BackgroundActivity,
        Self::InputMonitoring,
        Self::FullDiskAccess,
        Self::FilesAndFolders,
        Self::Automation,
        Self::AppManagement,
        Self::DeveloperTools,
        Self::Microphone,
        Self::Notifications,
        Self::Pasteboard,
    ];
    /// The helper's privileges: the ones its probe prints and its request raises.
    pub const HELPER: [Self; 3] = [Self::ScreenRecording, Self::Accessibility, Self::PostEvent];

    /// Who holds it. Background App Activity is the helper's entry, registered by the app.
    #[must_use]
    pub const fn holder(self) -> Holder {
        match self {
            Self::ScreenRecording
            | Self::Accessibility
            | Self::PostEvent
            | Self::BackgroundActivity => Holder::Helper,
            _ => Holder::App,
        }
    }

    /// Its section.
    #[must_use]
    pub const fn group(self) -> Group {
        match self {
            Self::ScreenRecording
            | Self::Accessibility
            | Self::PostEvent
            | Self::BackgroundActivity => Group::RemoteDesktop,
            Self::InputMonitoring => Group::Viewer,
            Self::FullDiskAccess
            | Self::FilesAndFolders
            | Self::Automation
            | Self::AppManagement
            | Self::DeveloperTools => Group::Agents,
            Self::Microphone => Group::Voice,
            Self::Notifications => Group::Alerts,
            Self::Pasteboard => Group::Clipboard,
        }
    }

    /// How it is asked. Paste from Other Apps lists the app only after its first alert, so its
    /// first click raises that alert ([`Self::first_prompts`]) and the drag panel follows.
    #[must_use]
    pub const fn ask(self) -> Ask {
        match self {
            Self::ScreenRecording
            | Self::Accessibility
            | Self::PostEvent
            | Self::InputMonitoring
            | Self::FilesAndFolders
            | Self::Automation
            | Self::Microphone
            | Self::Notifications => Ask::Prompt,
            Self::BackgroundActivity => Ask::Register,
            Self::FullDiskAccess
            | Self::AppManagement
            | Self::DeveloperTools
            | Self::Pasteboard => Ask::Drag,
        }
    }

    /// Whether the first click raises an alert even though the row is a drag: the app enters
    /// Paste from Other Apps' list only once it was asked.
    #[must_use]
    pub const fn first_prompts(self) -> bool {
        matches!(self, Self::Pasteboard)
    }

    /// Whether its list takes a bundle dragged in (it has a "+"), so "Open Settings" shows the
    /// drag panel. The other lists already hold the app once it was asked, with a switch or a
    /// menu to change.
    #[must_use]
    pub const fn takes_drag(self) -> bool {
        matches!(
            self,
            Self::ScreenRecording
                | Self::Accessibility
                | Self::PostEvent
                | Self::InputMonitoring
                | Self::FullDiskAccess
                | Self::AppManagement
                | Self::DeveloperTools
        )
    }

    /// How its state is read.
    #[must_use]
    pub const fn read(self) -> Read {
        match self {
            Self::ScreenRecording | Self::Accessibility | Self::PostEvent => Read::Helper,
            Self::FullDiskAccess | Self::FilesAndFolders => Read::SpotCheck,
            Self::AppManagement | Self::DeveloperTools => Read::Confirmed,
            _ => Read::Api,
        }
    }

    /// Whether a grant takes effect only in a new process. System Settings 27.2 says so for
    /// Screen Recording, Listen Event, All Files, App Bundles and Pasteboard; Developer Tools
    /// applies to programs started after it.
    #[must_use]
    pub const fn relaunch(self) -> Relaunch {
        match self {
            Self::ScreenRecording => Relaunch::Helper,
            Self::InputMonitoring
            | Self::FullDiskAccess
            | Self::AppManagement
            | Self::DeveloperTools
            | Self::Pasteboard => Relaunch::App,
            _ => Relaunch::No,
        }
    }

    /// The System Settings anchor, for the rows that have one that works.
    #[must_use]
    pub const fn anchor(self) -> Option<&'static str> {
        match self {
            Self::ScreenRecording => Some("Privacy_ScreenCapture"),
            // Post Event's own anchor lands on the root; it is listed under Accessibility.
            Self::Accessibility | Self::PostEvent => Some("Privacy_Accessibility"),
            Self::InputMonitoring => Some("Privacy_ListenEvent"),
            Self::FullDiskAccess => Some("Privacy_AllFiles"),
            Self::FilesAndFolders => Some("Privacy_FilesAndFolders"),
            Self::Automation => Some("Privacy_Automation"),
            Self::AppManagement => Some("Privacy_AppBundles"),
            Self::DeveloperTools => Some("Privacy_DevTools"),
            Self::Microphone => Some("Privacy_Microphone"),
            Self::Pasteboard => Some("Privacy_Pasteboard"),
            Self::BackgroundActivity | Self::Notifications => None,
        }
    }

    /// The `x-apple.systempreferences:` link that opens its list.
    #[must_use]
    pub fn settings_url(self) -> String {
        match (self, self.anchor()) {
            (_, Some(anchor)) => format!("{PRIVACY_URL}?{anchor}"),
            (Self::BackgroundActivity, None) => LOGIN_ITEMS_URL.to_owned(),
            (_, None) => NOTIFICATIONS_URL.to_owned(),
        }
    }

    /// The list's name as System Settings 27 shows it, for the drag panel's words.
    #[must_use]
    pub const fn list_name(self) -> &'static str {
        match self {
            Self::ScreenRecording => "Screen & System Audio Recording",
            Self::Accessibility | Self::PostEvent => "Device Control and Data Access",
            Self::BackgroundActivity => "Background App Activity",
            Self::InputMonitoring => "Input Monitoring",
            Self::FullDiskAccess => "Full Disk Access",
            Self::FilesAndFolders => "Files & Folders",
            Self::Automation => "Automation",
            Self::AppManagement => "App Management",
            Self::DeveloperTools => "Developer Tools",
            Self::Microphone => "Microphone",
            Self::Notifications => "Notifications",
            Self::Pasteboard => "Paste from Other Apps",
        }
    }

    /// The row's title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::ScreenRecording => "Screen Recording",
            Self::Accessibility => "Accessibility",
            Self::PostEvent => "Post Events",
            Self::BackgroundActivity => "Run in the background",
            Self::InputMonitoring => "Input Monitoring",
            Self::FullDiskAccess => "Full Disk Access",
            Self::FilesAndFolders => "Desktop, Documents and Downloads",
            Self::Automation => "Automation",
            Self::AppManagement => "App Management",
            Self::DeveloperTools => "Developer Tools",
            Self::Microphone => "Microphone",
            Self::Notifications => "Notifications",
            Self::Pasteboard => "Paste from Other Apps",
        }
    }

    /// The row's one-line reason.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::ScreenRecording => {
                "Onicode Desktop shows this Mac's screen on your other devices."
            }
            Self::Accessibility => {
                "Onicode Desktop reads the focused field and the pointer's shape."
            }
            Self::PostEvent => "Onicode Desktop types and clicks for the device in control.",
            Self::BackgroundActivity => {
                "Onicode Desktop waits for your devices while Onicode is closed."
            }
            Self::InputMonitoring => {
                "Shortcuts such as ⌘Tab reach the Mac you're viewing, not this one."
            }
            Self::FullDiskAccess => "Agents read and build anywhere without stopping to ask.",
            Self::FilesAndFolders => "Agents can work in these folders and on external drives.",
            Self::Automation => "Agents can script System Events and Finder.",
            Self::AppManagement => "Agents can update and replace the apps they build.",
            Self::DeveloperTools => "Programs agents build skip the malware scan on first run.",
            Self::Microphone => "Talk to your agents.",
            Self::Notifications => "Hear when an agent finishes or needs you.",
            Self::Pasteboard => "Copies here reach your other devices without a prompt each time.",
        }
    }

    /// The name the helper's request argument takes: `screen-recording`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::ScreenRecording => "screen-recording",
            Self::Accessibility => "accessibility",
            Self::PostEvent => "post-event",
            Self::BackgroundActivity => "background-activity",
            Self::InputMonitoring => "input-monitoring",
            Self::FullDiskAccess => "full-disk-access",
            Self::FilesAndFolders => "files-and-folders",
            Self::Automation => "automation",
            Self::AppManagement => "app-management",
            Self::DeveloperTools => "developer-tools",
            Self::Microphone => "microphone",
            Self::Notifications => "notifications",
            Self::Pasteboard => "pasteboard",
        }
    }

    /// Reads [`Self::key`] back.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|privilege| privilege.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn every_row_links_to_its_list() {
        assert_eq!(
            Privilege::ScreenRecording.settings_url(),
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture"
        );
        assert_eq!(
            Privilege::PostEvent.settings_url(),
            Privilege::Accessibility.settings_url(),
            "Post Event's own anchor lands on the root"
        );
        assert_eq!(
            Privilege::BackgroundActivity.settings_url(),
            "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"
        );
        assert_eq!(
            Privilege::Pasteboard.settings_url(),
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Pasteboard"
        );
        for privilege in Privilege::ALL {
            let url = privilege.settings_url();
            assert!(
                url.starts_with("x-apple.systempreferences:com.apple."),
                "{url}"
            );
            assert!(!url.contains("Privacy_PostEvent"), "{url}");
        }
    }

    #[test]
    fn keys_read_back_and_are_unique() {
        for privilege in Privilege::ALL {
            assert_eq!(Privilege::from_key(privilege.key()), Some(privilege));
            let json = serde_json::to_string(&privilege).unwrap_or_default();
            assert_eq!(json, format!("\"{}\"", privilege.key()));
        }
        assert_eq!(Privilege::from_key("camera"), None);
    }

    #[test]
    fn the_order_follows_the_groups() {
        let groups: Vec<Group> = Privilege::ALL.into_iter().map(Privilege::group).collect();
        let mut sorted = groups.clone();
        sorted.sort();
        assert_eq!(groups, sorted);
        assert!(Group::ALL.iter().all(|group| groups.contains(group)));
    }

    #[test]
    fn the_helper_holds_the_remote_desktop_rows() {
        for privilege in Privilege::ALL {
            assert_eq!(
                privilege.holder() == Holder::Helper,
                privilege.group() == Group::RemoteDesktop,
                "{privilege:?}"
            );
            assert_eq!(
                Privilege::HELPER.contains(&privilege),
                privilege.read() == Read::Helper,
                "{privilege:?}"
            );
        }
    }

    #[test]
    fn the_rows_without_a_status_are_dragged_and_relaunch() {
        for privilege in Privilege::ALL {
            if privilege.read() == Read::Confirmed {
                assert_eq!(privilege.ask(), Ask::Drag);
                assert!(privilege.takes_drag());
                assert_eq!(privilege.relaunch(), Relaunch::App);
            }
            // A drag row with no first alert must have a list to drag into.
            if privilege.ask() == Ask::Drag && !privilege.first_prompts() {
                assert!(privilege.takes_drag(), "{privilege:?}");
            }
        }
    }
}
