//! The app's own rows: each read without a prompt, each asked only from the person's click.
//!
//! Both are blocking (a folder prompt or an Apple Events check waits for the person, and
//! `AEDeterminePermissionToAutomateTarget` must not run on the main thread): call them off it.
//! The helper's rows are read and asked by the helper ([`crate::probe_helper`],
//! [`crate::request_in_helper`]); the login item through [`super::agent`].

use objc2_core_graphics::CGRequestListenEventAccess;

use super::{automation, files, microphone, notifications};
use crate::{Privilege, Status, paste};

/// `kIOHIDRequestTypeListenEvent`.
const LISTEN_EVENT: u32 = 1;

// `IOHIDCheckAccess` (IOKit/hid/IOHIDLib.h), declared here rather than through a feature of
// objc2-io-kit that pulls in all of HID.
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
}

/// Reads `privilege` for this app without prompting. `asked` says whether the checklist raised
/// its alert before: the folders are only opened once they were, since opening them unasked
/// would prompt. The helper's rows, the login item and the rows with no status API read
/// [`Status::Unknown`] here.
#[must_use]
pub fn read(privilege: Privilege, asked: bool) -> Status {
    match privilege {
        Privilege::InputMonitoring => {
            // SAFETY: takes a request type by value and returns an access type.
            match unsafe { IOHIDCheckAccess(LISTEN_EVENT) } {
                0 => Status::Granted,
                1 => Status::Denied,
                _ => Status::NotAsked,
            }
        }
        Privilege::FullDiskAccess => files::full_disk(),
        Privilege::FilesAndFolders => {
            if files::full_disk() == Status::Granted {
                Status::Granted
            } else if asked {
                files::folders()
            } else {
                Status::Off
            }
        }
        Privilege::Automation => automation::status(false),
        Privilege::Microphone => microphone::status(),
        Privilege::Notifications => notifications::status(),
        Privilege::Pasteboard => paste::behavior().status(),
        Privilege::ScreenRecording
        | Privilege::Accessibility
        | Privilege::PostEvent
        | Privilege::BackgroundActivity
        | Privilege::AppManagement
        | Privilege::DeveloperTools => Status::Unknown,
    }
}

/// Raises `privilege`'s system alert for this app, and waits for it where the API does. Only
/// from the person's click: never from a test, never unasked. Rows asked another way (the
/// helper's, the login item, the drag rows) do nothing here.
pub fn ask(privilege: Privilege) {
    match privilege {
        Privilege::InputMonitoring => {
            let _on = CGRequestListenEventAccess();
        }
        Privilege::FilesAndFolders => files::open_folders(),
        Privilege::Automation => {
            let _status = automation::status(true);
        }
        Privilege::Microphone => {
            let (tx, rx) = std::sync::mpsc::channel();
            if microphone::request(move |granted| {
                let _sent = tx.send(granted);
            }) {
                let _answer = rx.recv();
            }
        }
        Privilege::Notifications => notifications::request(),
        Privilege::Pasteboard => paste::ask_once(),
        Privilege::ScreenRecording
        | Privilege::Accessibility
        | Privilege::PostEvent
        | Privilege::BackgroundActivity
        | Privilege::FullDiskAccess
        | Privilege::AppManagement
        | Privilege::DeveloperTools => {}
    }
}
