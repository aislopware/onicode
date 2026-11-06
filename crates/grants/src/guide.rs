//! The guide that has the person drag a bundle into a System Settings privacy list: Onicode.app
//! for the app's rows, Onicode Desktop.app for the helper's.
//!
//! The Onicode app opens the list with a deep link, brings System Settings forward, and shows a
//! small non-activating panel at the bottom of System Settings' window with the bundle's icon to
//! drag. The panel shows only while System Settings or its authentication prompt is in
//! front, follows the window, and walks [`Guide`]'s phases: drag, confirm, granted; or Settings
//! closed, the list left, authentication cancelled.
//!
//! [`Guide`] and [`place`] are pure and run everywhere; `show`, `report`, `phase`, `privilege`
//! and `close` drive the AppKit panel on macOS's main thread. Whether the grant is on comes from
//! the checklist's poll (the helper's probe for its rows), handed to `report`.

mod machine;
pub mod place;

pub use machine::{
    Accessory, Effect, End, Event, GRANTED_LINGER, Guide, Look, PROMPT_SETTLE, PROMPT_WAIT, Phase,
    SETTINGS_GONE, SETTINGS_WAIT, SettingsView, TITLE_SETTLE,
};

#[cfg(target_os = "macos")]
pub use crate::macos::panel::{ShowError, close, is_open, phase, privilege, report, show};
#[cfg(target_os = "macos")]
pub use crate::macos::settings::settings_windows;
