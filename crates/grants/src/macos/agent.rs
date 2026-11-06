//! Registering desktopd as the app's launchd agent with `SMAppService`, so it starts at login,
//! shows under the app in Login Items, and runs from the helper bundle that holds its grants.
//!
//! The checklist's "Run in the background" row calls [`register`] with
//! [`crate::bundle::agent_plist`] of the app's bundle id, from the person's click and nowhere
//! else; the plist sits in `Contents/Library/LaunchAgents` and names the helper's executable with
//! `BundleProgram` and the app with `AssociatedBundleIdentifiers`. Registering never prompts;
//! macOS posts its own "background item added" notification once.

use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{NSError, NSString};

// SMAppService lives in ServiceManagement, which nothing else links.
#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

/// `SMAppServiceStatus`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Status {
    /// Not registered.
    NotRegistered,
    /// Registered and allowed to run.
    Enabled,
    /// Registered, waiting for the person to allow it in Login Items.
    RequiresApproval,
    /// The plist is not in the bundle.
    NotFound,
}

impl Status {
    const fn from_raw(raw: isize) -> Self {
        match raw {
            1 => Self::Enabled,
            2 => Self::RequiresApproval,
            3 => Self::NotFound,
            _ => Self::NotRegistered,
        }
    }

    /// The checklist's status for the login item: registering is its "Allow…", and one waiting
    /// for approval is allowed in Login Items.
    #[must_use]
    pub const fn checklist(self) -> crate::Status {
        match self {
            Self::NotRegistered => crate::Status::NotAsked,
            Self::Enabled => crate::Status::Granted,
            Self::RequiresApproval => crate::Status::Denied,
            Self::NotFound => crate::Status::Unavailable,
        }
    }
}

/// Why registering or unregistering failed.
#[derive(Clone, PartialEq, Eq, Debug, thiserror::Error)]
pub enum AgentError {
    /// This macOS has no `SMAppService` (never on 27.2+).
    #[error("SMAppService is missing")]
    Unavailable,
    /// `SMAppService` refused, in its words.
    #[error("{0}")]
    Refused(String),
}

fn service(plist: &str) -> Result<Retained<AnyObject>, AgentError> {
    let class = AnyClass::get(c"SMAppService").ok_or(AgentError::Unavailable)?;
    let name = NSString::from_str(plist);
    // SAFETY: `+[SMAppService agentServiceWithPlistName:]` takes an NSString and returns a
    // non-null service.
    let service: Option<Retained<AnyObject>> =
        unsafe { msg_send![class, agentServiceWithPlistName: &*name] };
    service.ok_or(AgentError::Unavailable)
}

/// The agent's status for the plist named `plist`.
#[must_use]
pub fn status(plist: &str) -> Status {
    service(plist).map_or(Status::NotFound, |service| {
        // SAFETY: `-[SMAppService status]` returns an `NSInteger`.
        let raw: isize = unsafe { msg_send![&*service, status] };
        Status::from_raw(raw)
    })
}

/// Registers the agent `plist` and answers its status after. Registering one already registered
/// is harmless.
///
/// # Errors
///
/// `SMAppService` is missing or refuses (for one, a plist the bundle does not hold).
pub fn register(plist: &str) -> Result<Status, AgentError> {
    let service = service(plist)?;
    // SAFETY: `-[SMAppService registerAndReturnError:]` returns `BOOL` with an `NSError` out
    // parameter.
    let registered: Result<(), Retained<NSError>> =
        unsafe { msg_send![&*service, registerAndReturnError: _] };
    registered.map_err(|err| AgentError::Refused(err.localizedDescription().to_string()))?;
    Ok(status(plist))
}

/// Unregisters the agent `plist`; launchd stops it.
///
/// # Errors
///
/// `SMAppService` is missing or refuses.
pub fn unregister(plist: &str) -> Result<(), AgentError> {
    let service = service(plist)?;
    // SAFETY: `-[SMAppService unregisterAndReturnError:]` returns `BOOL` with an `NSError` out
    // parameter.
    let unregistered: Result<(), Retained<NSError>> =
        unsafe { msg_send![&*service, unregisterAndReturnError: _] };
    unregistered.map_err(|err| AgentError::Refused(err.localizedDescription().to_string()))
}

/// Opens System Settings at Login Items, where a [`Status::RequiresApproval`] agent is allowed.
pub fn open_login_items() {
    if let Some(class) = AnyClass::get(c"SMAppService") {
        // SAFETY: `+[SMAppService openSystemSettingsLoginItems]` takes and returns nothing.
        let () = unsafe { msg_send![class, openSystemSettingsLoginItems] };
    }
}
