//! Every privilege Onicode asks for, asked once at onboarding
//! (`docs/onicode/decisions/onboarding-grants.md`).
//!
//! - **Pure, everywhere:** [`Privilege`] (who holds it, its group, how it is asked and read,
//!   whether it needs a relaunch, its System Settings link), the [`Checklist`] and its kept
//!   [`Progress`], the [`Install`] check, the helper's [`Grants`] and their probe line, the
//!   helper's command line ([`helper`]), the bundle layout ([`bundle`]), and the drag guide's state
//!   machine and geometry ([`guide::Guide`], [`guide::place`]).
//! - **macOS:** [`access`] reads the app's own rows without a prompt and asks them on a click;
//!   [`probe_helper`] and [`request_in_helper`] do the same for the helper's rows by running the
//!   helper through Launch Services; [`agent`] registers the helper as a login item;
//!   `restart_when_granted` restarts desktopd once a new process sees Screen Recording;
//!   `guide::show` puts the drag panel beside System Settings; [`microphone`] and [`paste`] are
//!   shared with voice and clipboard sync.
//!
//! Nothing prompts unasked: every request runs from the person's click in the checklist (or in
//! the feature that needs it), never from a test or a background process on its own.

#![warn(unreachable_pub)]

pub mod bundle;
mod checklist;
mod grants;
pub mod guide;
pub mod helper;
mod install;
#[cfg(target_os = "macos")]
mod macos;
pub mod paste;
mod privilege;

pub use checklist::{Action, Checklist, Progress, Status};
pub use grants::{Grants, ParseError, needs_restart};
pub use install::Install;
#[cfg(target_os = "macos")]
pub use macos::probe::{
    RESTART_EXIT_CODE, answer_probe, answer_request, probe_fresh, probe_helper, probe_with,
    request_in_helper, restart_when_granted,
};
#[cfg(target_os = "macos")]
pub use macos::{access, agent, microphone};
#[cfg(target_os = "macos")]
pub use macos::{activate_settings, install_of, open_settings, reopen_after_quit};
pub use privilege::{Ask, Group, Holder, Privilege, Read, Relaunch};
