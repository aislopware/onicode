//! The guide that has the person drag Onicode Desktop into a System Settings privacy list.
//!
//! The Onicode app opens the list with a deep link, brings System Settings forward, and shows a
//! small non-activating panel at the bottom of System Settings' window with the helper bundle's
//! icon to drag. The panel shows only while System Settings or its authentication prompt is in
//! front, follows the window, and walks [`Guide`]'s phases: drag, confirm, granted; or Settings
//! closed, the list left, authentication cancelled.
//!
//! [`Guide`] and [`place`] are pure and run everywhere; `show`, `report`, `phase` and
//! `close` drive the AppKit panel on macOS's main thread. Whether the grant is on comes from
//! desktopd, not the app: the app polls desktopd's grants through the engine while the guide is
//! open and hands each answer to `report`.

mod machine;
pub mod place;

pub use machine::{
    Accessory, Effect, End, Event, GRANTED_LINGER, Guide, Look, PROMPT_SETTLE, PROMPT_WAIT, Phase,
    SETTINGS_GONE, SETTINGS_WAIT, SettingsView, TITLE_SETTLE,
};

#[cfg(target_os = "macos")]
pub use crate::macos::panel::{ShowError, close, is_open, phase, report, show};
#[cfg(target_os = "macos")]
pub use crate::macos::settings::settings_windows;
