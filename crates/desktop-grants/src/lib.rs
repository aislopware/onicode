//! What `onicode-desktopd` may do on its Mac, and the guide that gets it turned on.
//!
//! desktopd runs from its own helper bundle, `Onicode.app/Contents/Library/LoginItems/Onicode
//! Desktop.app`, as an `SMAppService` agent of the app, so the Screen Recording and Accessibility
//! grants belong to that bundle's signing identity and survive rebuilds and updates
//! (`docs/onicode/decisions/desktop-grants.md`).
//!
//! - **Pure, everywhere:** [`Grants`] and [`Pane`] with their probe line, [`needs_restart`], the
//!   bundle layout ([`bundle`]), and the guide's state machine and panel geometry
//!   ([`guide::Guide`], [`guide::place`]).
//! - **macOS:** `Grants::current` reads the preflights and never asks; `probe_fresh` reads them in
//!   a new process, because a running process keeps seeing the Screen Recording state it started
//!   with; `restart_when_granted` exits desktopd for launchd to start it again once the probe sees
//!   a grant this process cannot; `guide::show` puts the drag panel beside System Settings; `agent`
//!   registers desktopd with `SMAppService`.
//!
//! Nothing here calls `CGRequestScreenCaptureAccess` or `CGRequestPostEventAccess`: a request
//! prompts, and a process that only ever preflights is never listed, so the guide has the person
//! drag the helper into the list instead.

#![warn(unreachable_pub)]

pub mod bundle;
mod grants;
pub mod guide;
#[cfg(target_os = "macos")]
mod macos;

pub use grants::{Grants, Pane, ParseError, needs_restart};
#[cfg(target_os = "macos")]
pub use macos::agent;
#[cfg(target_os = "macos")]
pub use macos::probe::{
    PROBE_ARG, RESTART_EXIT_CODE, answer_probe_if_asked, probe_fresh, probe_with,
    restart_when_granted,
};
#[cfg(target_os = "macos")]
pub use macos::{activate_settings, open_pane};
