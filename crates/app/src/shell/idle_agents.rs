//! The desktop idle policy spares every conversation currently on screen.
use onicode_protocol::agents::KeepIdleAgents;

use super::*;

/// The conversations the engine was last told to spare, so a frame that shows the same ones
/// sends nothing.
struct Kept(Vec<i64>);
impl gpui::Global for Kept {}

impl Shell {
    pub(crate) fn keep_visible_idle_agents(&self, cx: &mut App) {
        let mut ids = Vec::new();
        if !self.app_view_open()
            && let Some(tab) = self.workspace.active_tab()
        {
            ids.extend(
                self.workspace
                    .layout(tab)
                    .leaf_ids()
                    .into_iter()
                    .map(|pane| self.workspace.session_for_pane(pane)),
            );
        }
        if let Some(preview) = &self.session_preview {
            ids.push(preview.id);
        }
        // Read, not `default_global`: that writes the global, and a global
        // written as the shell renders counts as changed on every frame.
        if cx.try_global::<Kept>().is_some_and(|kept| kept.0 == ids) {
            return;
        }
        cx.set_global(Kept(ids.clone()));
        let keep = self
            .engine
            .call::<KeepIdleAgents>(None, KeepIdleAgents { rows: ids });
        cx.background_executor().spawn(keep).detach();
    }
}
