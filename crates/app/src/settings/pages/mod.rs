//! One file per Settings page, each a port of its SettingsView.tsx function.
mod appearance;
mod archive;
mod chat;
mod connections;
mod general;
mod inbox;
mod keybindings;
mod mcp;
mod providers;
mod skills;
mod worktrees;

pub(super) use appearance::parse_hex;
pub(crate) use appearance::{ACCENTS, swatch};
pub(super) use connections::ConnectionsState;
use gpui::{AnyElement, Context, Window};
pub(crate) use keybindings::command_shortcut;

use super::{Section, SettingsView};

impl SettingsView {
    pub(super) fn page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match self.section {
            Section::General => self.general_page(cx),
            // Settings → Connections holds the machines; phones return with step 13.
            Section::Connections => self.connections_page(),
            Section::Appearance => self.appearance_page(window, cx),
            Section::Keybindings => self.keybindings_page(cx),
            Section::Chat => self.chat_page(cx),
            Section::Providers => self.providers_page(window, cx),
            Section::Mcp => self.mcp_page(cx),
            Section::Skills => self.skills_page(cx),
            Section::Inbox => self.inbox_page(window, cx),
            Section::Archive => self.archive_page(cx),
            Section::Worktrees => self.worktrees_page(cx),
        }
    }
}
