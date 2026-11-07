//! Agent alerts (`docs/onicode/decisions/alerts.md`): the moments an agent needs its owner,
//! decided here, in the engine that runs it, so they keep coming with every window closed.
//!
//! The [`AlertsHub`] takes in what the engine's own hubs say (the workspace's sessions and marks,
//! a session's approvals, questions and status while its turn runs, every terminal program's
//! OSC 7501 status) through [`pump()`], decides with a pure machine (four causes: needs you,
//! failed, finished unseen, stalled; a 1.5 s hold; one alert per chat or terminal), and serves the
//! result as `WatchAlerts` ([`AlertsService`]). `SetViewportFocus` tells it what each viewer has on
//! screen and whether the viewer presents the alerts; while none presents, the hub posts plain
//! banners itself ([`Banners`]). The projects' mutes are the engine's own settings
//! (`SetProjectNotifications`), applied to every alert and banner with or without a viewer.
mod alerter;
mod banners;
mod hub;
mod pump;
mod service;
#[cfg(test)]
mod tests;

pub use banners::{Banners, NoBanners, SystemBanners};
pub use hub::AlertsHub;
pub use pump::pump;
pub use service::AlertsService;
