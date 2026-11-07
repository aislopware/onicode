//! Compact interaction state survives an inactive transcript cache eviction.
use super::*;

/// Keep source row indices intact for persistence, tools and live patches.
/// Only the displayed window needs Markdown and presentation objects.
pub(crate) struct PreparedHistory {
    rows: Vec<Row>,
    parsers: HashMap<usize, MarkdownParser>,
}
impl PreparedHistory {
    /// Run on the history loader, before handing its result to the UI.
    pub(crate) fn new(rows: Vec<Row>) -> Self {
        let parsers = prepare_parsers(&rows);
        Self { rows, parsers }
    }
}

fn prepare_parsers(rows: &[Row]) -> HashMap<usize, MarkdownParser> {
    let mut parsers = HashMap::new();
    let mut at = 0;
    while at < rows.len() {
        let end = (at + 1..rows.len())
            .find(|&ix| rows[ix].first)
            .unwrap_or(rows.len());
        if matches!(rows[at].role.as_str(), "assistant" | "reasoning") {
            let text: String = rows[at..end].iter().map(|row| row.text.as_str()).collect();
            let mut parser = MarkdownParser::new();
            parser.set_text(&text);
            parsers.insert(at, parser);
        }
        at = end;
    }
    parsers
}

pub(super) struct HistoryPosition {
    key: Option<(usize, usize)>,
    offset: ListOffset,
    following: bool,
}
impl TranscriptView {
    pub(crate) fn install_prepared_history(
        &mut self,
        history: PreparedHistory,
        cx: &mut Context<Self>,
    ) {
        // A view evicted while inactive restores where the reader left it.
        if self.history_position.is_some() {
            self.restore_prepared_history(history, cx);
            return;
        }
        self.prepared_parsers = history.parsers;
        self.patch(0, history.rows, cx);
    }

    /// Saved turn receipts, keyed by row.
    pub(crate) fn set_turns(&mut self, turns: HashMap<usize, onicode_protocol::agent::TurnInfo>) {
        self.turns = turns;
    }

    /// The turn receipts, keyed by row, as settings save them.
    pub(crate) fn window_turns(&self) -> HashMap<usize, onicode_protocol::agent::TurnInfo> {
        self.turns.clone()
    }

    pub(crate) fn replace_history(&mut self, rows: Vec<Row>, cx: &mut Context<Self>) {
        let len = rows.len();
        self.selection.clear();
        self.history_position = None;
        self.prepared_parsers.clear();
        self.turn_groups.get_mut().clear();
        self.active_provider_turn_id = None;
        self.search_match = None;
        self.anchor_turn = None;
        self.prompt_rise = None;
        self.prompt_rise_pending = false;
        self.introduced_prompt = None;
        self.turns.retain(|&start, _| start < len);
        self.elapsed.retain(|&start, _| start < len);
        self.expanded.retain(|&start| start < len);
        self.phases.retain(|&start, _| start < len);
        self.phase_layouts.clear();
        self.attachment_images.clear();
        self.visuals.clear();
        self.reveals.clear();
        self.markdown_cache = Default::default();
        self.measurements = Default::default();
        self.motions.clear();
        self.hovered_prompt = None;
        self.hovered_prompt_actions = None;
        self.outline = Default::default();
        self.reflow = Default::default();
        self.follow = Default::default();
        self.highlight_epoch = self.highlight_epoch.wrapping_add(1);
        self.highlights.clear();
        self.highlighting.clear();
        self.highlight_queue.clear();
        self.highlight_active = false;
        self.highlight_order.clear();
        self.highlight_bytes = 0;
        self.restore_history(rows, cx);
    }

    /// A conservative payload estimate, including the source, parsed markdown
    /// and presentation copies. The inactive cache also has a session count cap.
    pub(crate) fn history_weight(&self) -> usize {
        self.source_bytes
            .saturating_mul(4)
            .saturating_add(
                self.source
                    .capacity()
                    .saturating_mul(std::mem::size_of::<Row>()),
            )
            .saturating_add(self.highlight_bytes)
            .saturating_add(self.markdown_cache.retained_bytes())
            // Charge each retained interpreter's cap to the inactive cache.
            .saturating_add(
                self.visuals
                    .len()
                    .saturating_mul(onicode_protocol::visual::MEMORY_LIMIT_BYTES),
            )
    }

    pub(crate) fn suspend_history(&mut self, cx: &mut Context<Self>) -> bool {
        if self.visible
            || self.streaming
            || self.history_position.is_some()
            || self.saved_draft.is_some()
            || !self.note_pending.is_empty()
            || self.target_action.is_some()
        {
            return false;
        }
        let offset = self.list.logical_scroll_top();
        self.selection.clear();
        self.history_position = Some(HistoryPosition {
            key: self.rows.get(offset.item_ix).map(|row| row.key),
            offset,
            following: self.list.is_following_tail(),
        });
        self.source = Vec::new();
        self.source_bytes = 0;
        self.prepared_parsers = HashMap::new();
        self.turn_groups = Default::default();
        self.attachment_images = HashMap::new();
        self.visuals = HashMap::new();
        self.messages = Vec::new();
        self.rows = Vec::new();
        self.reveals = HashMap::new();
        self.markdown_cache = Default::default();
        self.measurements = Default::default();
        self.phase_layouts = HashMap::new();
        self.motions = HashMap::new();
        self.hovered_prompt = None;
        self.hovered_prompt_actions = None;
        self.action_anchors = HashMap::new();
        self.outline = Default::default();
        self.reflow = Default::default();
        self.follow = Default::default();
        // A running syntax task owns its input until completion; its old epoch
        // cannot repopulate this cache after suspension or after a quick reopen.
        self.highlight_epoch = self.highlight_epoch.wrapping_add(1);
        self.highlights = HashMap::new();
        self.highlighting = HashSet::new();
        self.highlight_queue = VecDeque::new();
        self.highlight_active = false;
        self.highlight_order = VecDeque::new();
        self.highlight_bytes = 0;
        self.list.reset(1);
        cx.notify();
        true
    }

    pub(crate) fn restore_history(&mut self, rows: Vec<Row>, cx: &mut Context<Self>) {
        self.restore_prepared_history(PreparedHistory::new(rows), cx);
    }

    pub(crate) fn restore_prepared_history(
        &mut self,
        history: PreparedHistory,
        cx: &mut Context<Self>,
    ) {
        let position = self.history_position.take();
        // Reopening also handles a source that became empty in another window.
        self.source = Vec::new();
        self.source_bytes = 0;
        self.messages = Vec::new();
        self.rows = Vec::new();
        self.turn_groups.get_mut().clear();
        self.list.reset(1);
        self.install_prepared_history(history, cx);
        if let Some(position) = position {
            if position.following {
                self.list.set_follow_mode(FollowMode::Tail);
                self.list.scroll_to_end();
            } else {
                self.list.pause_following_tail();
                let item_ix = position
                    .key
                    .and_then(|key| self.rows.iter().position(|row| row.key == key))
                    .unwrap_or(position.offset.item_ix.min(self.rows.len()));
                self.list.scroll_to(ListOffset {
                    item_ix,
                    offset_in_item: position.offset.offset_in_item,
                });
            }
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn rows() -> Vec<Row> {
        vec![
            Row {
                role: "user".into(),
                text: "Find the needle".into(),
                first: true,
                sent_at: Some(7),
                id: None,
            },
            Row {
                role: "assistant".into(),
                text: "needle answer\n\nsecond paragraph".into(),
                first: true,
                sent_at: Some(8),
                id: None,
            },
        ]
    }

    #[gpui::test]
    fn suspended_history_releases_payload_and_restores_search_folds_and_scroll(
        cx: &mut TestAppContext,
    ) {
        let view = cx.new(|_| TranscriptView::new());
        view.update(cx, |view, cx| {
            view.visible(false, cx);
            let original = rows();
            view.patch(0, original.clone(), cx);
            view.reveal_search(1, "needle", cx);
            view.phases.insert(1, true);
            let offset = view.list.logical_scroll_top();
            let key = view.rows.get(offset.item_ix).unwrap().key;
            assert!(view.history_weight() > 0);
            let epoch = view.highlight_epoch;
            assert!(view.suspend_history(cx));
            assert!(view.source.is_empty() && view.rows.is_empty() && view.messages.is_empty());
            assert_eq!(view.history_weight(), 0);
            assert_ne!(view.highlight_epoch, epoch);
            assert!(!view.suspend_history(cx));
            view.restore_history(original.clone(), cx);
            assert_eq!(view.source.len(), original.len());
            assert_eq!(view.source[0].sent_at, Some(7));
            assert_eq!(view.search_match, Some((1, "needle".into())));
            assert!(view.expanded.contains(&1));
            assert_eq!(view.phases.get(&1), Some(&true));
            assert!(!view.list.is_following_tail());
            let restored = view.list.logical_scroll_top();
            assert_eq!(view.rows[restored.item_ix].key, key);
            assert_eq!(restored.offset_in_item, offset.offset_in_item);
        });
    }

    #[gpui::test]
    fn visible_running_and_unsaved_action_histories_are_pinned(cx: &mut TestAppContext) {
        let view = cx.new(|_| TranscriptView::new());
        view.update(cx, |view, cx| {
            view.patch(0, rows(), cx);
            assert!(!view.suspend_history(cx));
            view.visible(false, cx);
            view.streaming = true;
            assert!(!view.suspend_history(cx));
            view.streaming = false;
            view.note_pending.insert(1);
            assert!(!view.suspend_history(cx));
            view.note_pending.clear();
            assert!(view.suspend_history(cx));
            view.restore_history(Vec::new(), cx);
            assert!(view.source.is_empty());
            assert!(view.history_position.is_none());
        });
    }
}
