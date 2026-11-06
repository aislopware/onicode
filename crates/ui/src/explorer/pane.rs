//! One pane of the explorer: a folder on one device, its rows, the selection and the cursor.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use gpui::{FocusHandle, ScrollStrategy, SharedString, Task, UniformListScrollHandle};
use onicode_proto::files::{PathKind, PathListing};

use super::format;

/// A pause this long between typed letters starts a new type-to-select prefix.
const TYPE_SELECT_RESET: Duration = Duration::from_millis(1000);

/// One listed entry, its labels ready for render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub path: String,
    pub name: SharedString,
    pub kind: PathKind,
    pub size: SharedString,
    pub modified: SharedString,
    pub kind_label: SharedString,
    /// The name lowercased, for type-to-select.
    folded: String,
}

impl Row {
    pub(crate) fn new(
        dir: &str,
        entry: &onicode_proto::files::PathEntry,
        now: DateTime<Local>,
    ) -> Self {
        let size = match entry.kind {
            PathKind::Dir | PathKind::Package => "--".to_string(),
            PathKind::File | PathKind::Symlink => format::size_label(entry.size),
        };
        Self {
            path: format::join(dir, &entry.name),
            name: entry.name.clone().into(),
            kind: entry.kind,
            size: size.into(),
            modified: format::date_label(entry.modified_ms, now).into(),
            kind_label: format::kind_label(&entry.name, entry.kind).into(),
            folded: entry.name.to_lowercase(),
        }
    }

    /// Whether Enter goes into it rather than previewing it.
    pub(crate) fn is_folder(&self) -> bool {
        matches!(self.kind, PathKind::Dir)
    }

    /// Whether it copies as a tree: folders and packages.
    pub(crate) fn is_tree(&self) -> bool {
        matches!(self.kind, PathKind::Dir | PathKind::Package)
    }
}

/// Where a pane's listing stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Listing {
    Loading,
    Ready,
    Failed(SharedString),
}

pub(crate) struct Pane {
    /// The device this pane browses; empty until this Mac's id is known.
    pub device_id: String,
    /// The folder shown: as asked (`~`) until the listing names it absolutely.
    pub path: String,
    pub parent: Option<String>,
    pub rows: Vec<Row>,
    pub listing: Listing,
    pub cursor: Option<usize>,
    pub selected: BTreeSet<usize>,
    anchor: Option<usize>,
    typed: String,
    typed_at: Option<Instant>,
    pub scroll: UniformListScrollHandle,
    pub focus: FocusHandle,
    /// Bumped by each navigation, so a late listing for an older folder is dropped.
    pub generation: u64,
    pub _load: Option<Task<()>>,
}

impl Pane {
    pub(crate) fn new(device_id: String, path: String, focus: FocusHandle) -> Self {
        Self {
            device_id,
            path,
            parent: None,
            rows: Vec::new(),
            listing: Listing::Loading,
            cursor: None,
            selected: BTreeSet::new(),
            anchor: None,
            typed: String::new(),
            typed_at: None,
            scroll: UniformListScrollHandle::new(),
            focus,
            generation: 0,
            _load: None,
        }
    }

    /// Points the pane at another folder and returns the generation its listing must carry.
    pub(crate) fn begin_navigation(&mut self, device_id: String, path: String) -> u64 {
        if self.device_id != device_id || self.path != path {
            self.rows.clear();
            self.cursor = None;
            self.selected.clear();
            self.anchor = None;
        }
        self.device_id = device_id;
        self.path = path;
        self.listing = Listing::Loading;
        self.generation += 1;
        self.generation
    }

    /// Takes a listing in, keeping the cursor on the same name across a reload.
    /// `came_from` puts the cursor on the folder just left, as Finder does going up.
    pub(crate) fn apply_listing(
        &mut self,
        listing: &PathListing,
        came_from: Option<&str>,
        now: DateTime<Local>,
    ) {
        let kept = self
            .cursor
            .and_then(|ix| self.rows.get(ix))
            .map(|row| row.path.clone());
        let selected_paths: Vec<String> = self
            .selected
            .iter()
            .filter_map(|ix| self.rows.get(*ix).map(|row| row.path.clone()))
            .collect();
        self.path.clone_from(&listing.path);
        self.parent.clone_from(&listing.parent);
        self.rows = listing
            .entries
            .iter()
            .map(|entry| Row::new(&listing.path, entry, now))
            .collect();
        self.listing = Listing::Ready;
        let find = |path: &str| self.rows.iter().position(|row| row.path == path);
        self.cursor = came_from
            .and_then(find)
            .or_else(|| kept.as_deref().and_then(find));
        self.selected = selected_paths
            .iter()
            .filter_map(|path| find(path))
            .collect();
        if let Some(cursor) = self.cursor {
            if self.selected.is_empty() {
                self.selected.insert(cursor);
            }
            self.scroll.scroll_to_item(cursor, ScrollStrategy::Nearest);
        }
        self.anchor = self.cursor;
    }

    pub(crate) fn fail(&mut self, message: impl Into<SharedString>) {
        self.listing = Listing::Failed(message.into());
    }

    #[cfg(test)]
    pub(crate) fn cursor_row(&self) -> Option<&Row> {
        self.cursor.and_then(|ix| self.rows.get(ix))
    }

    /// Selects `ix` alone and moves the cursor there.
    pub(crate) fn select_only(&mut self, ix: usize) {
        if ix >= self.rows.len() {
            return;
        }
        self.selected.clear();
        self.selected.insert(ix);
        self.cursor = Some(ix);
        self.anchor = Some(ix);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
    }

    /// Adds `ix` to the selection, or takes it out.
    pub(crate) fn toggle(&mut self, ix: usize) {
        if ix >= self.rows.len() {
            return;
        }
        if !self.selected.remove(&ix) {
            self.selected.insert(ix);
        }
        self.cursor = Some(ix);
        self.anchor = Some(ix);
    }

    /// Selects from the anchor to `ix`.
    pub(crate) fn extend_to(&mut self, ix: usize) {
        if ix >= self.rows.len() {
            return;
        }
        let anchor = self.anchor.unwrap_or(ix);
        self.selected = (anchor.min(ix)..=anchor.max(ix)).collect();
        self.cursor = Some(ix);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
    }

    pub(crate) fn clear_selection(&mut self) -> bool {
        let changed = !self.selected.is_empty();
        self.selected.clear();
        changed
    }

    /// Moves the cursor by `delta` rows, extending the selection when `extend`.
    pub(crate) fn step(&mut self, delta: isize, extend: bool) -> bool {
        let Some(last) = self.rows.len().checked_sub(1) else {
            return false;
        };
        let next = match self.cursor {
            None if delta < 0 => last,
            None => 0,
            Some(ix) => ix.saturating_add_signed(delta).min(last),
        };
        if extend {
            self.extend_to(next);
        } else {
            self.select_only(next);
        }
        true
    }

    /// Moves to the first row whose name starts with what was typed lately.
    pub(crate) fn type_select(&mut self, text: &str, now: Instant) -> bool {
        if self
            .typed_at
            .is_none_or(|at| now.saturating_duration_since(at) > TYPE_SELECT_RESET)
        {
            self.typed.clear();
        }
        self.typed_at = Some(now);
        self.typed.push_str(&text.to_lowercase());
        let found = self
            .rows
            .iter()
            .position(|row| row.folded.starts_with(&self.typed));
        if let Some(ix) = found {
            self.select_only(ix);
        }
        found.is_some()
    }

    /// What actions apply to: the selection when the cursor is in it, else the cursor.
    pub(crate) fn action_rows(&self) -> Vec<usize> {
        match self.cursor {
            Some(cursor) if self.selected.contains(&cursor) => {
                self.selected.iter().copied().collect()
            }
            Some(cursor) => vec![cursor],
            None => self.selected.iter().copied().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use chrono::Local;
    use gpui::TestAppContext;
    use onicode_proto::files::{PathEntry, PathKind, PathListing};

    use super::{Listing, Pane};

    fn listing(path: &str, names: &[(&str, PathKind)]) -> PathListing {
        PathListing {
            path: path.into(),
            parent: super::format::parent(path).map(str::to_owned),
            entries: names
                .iter()
                .map(|(name, kind)| PathEntry {
                    name: (*name).into(),
                    kind: *kind,
                    size: 1,
                    modified_ms: 0,
                    mode: 0o644,
                    target: None,
                })
                .collect(),
        }
    }

    fn pane(cx: &mut TestAppContext) -> Pane {
        let focus = cx.update(|cx| cx.focus_handle());
        let mut pane = Pane::new("mac".into(), "~".into(), focus);
        pane.begin_navigation("mac".into(), "~".into());
        pane.apply_listing(
            &listing(
                "/Users/me",
                &[
                    ("Desktop", PathKind::Dir),
                    ("Documents", PathKind::Dir),
                    ("notes.txt", PathKind::File),
                ],
            ),
            None,
            Local::now(),
        );
        pane
    }

    #[gpui::test]
    fn arrows_walk_and_shift_extends(cx: &mut TestAppContext) {
        let mut pane = pane(cx);
        assert_eq!(pane.listing, Listing::Ready);
        assert_eq!(pane.path, "/Users/me");
        assert!(pane.step(1, false));
        assert_eq!(pane.cursor, Some(0));
        assert!(pane.step(1, true));
        assert_eq!(pane.action_rows(), [0, 1]);
        assert!(pane.step(5, false));
        assert_eq!(pane.cursor, Some(2));
        assert_eq!(pane.action_rows(), [2]);
        assert_eq!(
            pane.cursor_row().map(|row| row.path.as_str()),
            Some("/Users/me/notes.txt")
        );
    }

    #[gpui::test]
    fn typing_selects_by_prefix_and_a_pause_starts_over(cx: &mut TestAppContext) {
        let mut pane = pane(cx);
        let start = Instant::now();
        assert!(pane.type_select("d", start));
        assert_eq!(pane.cursor, Some(0));
        assert!(pane.type_select("o", start + Duration::from_millis(200)));
        assert_eq!(pane.cursor, Some(1));
        assert!(pane.type_select("n", start + Duration::from_secs(3)));
        assert_eq!(pane.cursor, Some(2));
        assert!(!pane.type_select("z", start + Duration::from_secs(9)));
        assert_eq!(pane.cursor, Some(2));
    }

    #[gpui::test]
    fn going_up_lands_on_the_folder_left(cx: &mut TestAppContext) {
        let mut pane = pane(cx);
        pane.begin_navigation("mac".into(), "/Users".into());
        pane.apply_listing(
            &listing(
                "/Users",
                &[("me", PathKind::Dir), ("shared", PathKind::Dir)],
            ),
            Some("/Users/me"),
            Local::now(),
        );
        assert_eq!(pane.cursor, Some(0));
        assert_eq!(pane.action_rows(), [0]);
    }
}
