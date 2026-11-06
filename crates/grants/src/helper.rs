//! The helper's command line for its grants: what the app and desktopd itself start it with.
//! desktopd reads these first thing in `main`; [`crate::probe_helper`] and
//! [`crate::request_in_helper`] start it with them.

use crate::Privilege;

/// The argument that makes the helper print its grants' line and exit.
pub const PROBE_ARG: &str = "--probe-grants";

/// The argument, followed by a privilege's key (`screen-recording`, `accessibility`,
/// `post-event`), that makes the helper raise that one alert, then print its grants' line and
/// exit.
pub const REQUEST_ARG: &str = "--request-grant";

/// The arguments that have the helper raise `privilege`'s alert.
#[must_use]
pub const fn request_args(privilege: Privilege) -> [&'static str; 2] {
    [REQUEST_ARG, privilege.key()]
}
