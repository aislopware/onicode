//! Opening what a transcript names (`OpenPath`: a link, a tool's file, a commit card's file): a
//! file in a review tab, as monocode did, and a folder in the explorer, revealed and expanded.
//! What a path is comes from a `Stat` on the engine that holds it, so a folder on another engine
//! opens as one on this Mac does. A path with a line is a file; one the engine cannot answer for
//! opens as a file, whose tab then says what is wrong.
use onicode_protocol::files::{PathKind, Stat};

use super::*;

/// How a path opens, by what its engine said it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Opens {
    File,
    Folder,
}

impl Opens {
    /// A folder only when the engine said so; anything else (a file, nothing there, an engine
    /// that did not answer) opens as a file.
    pub(super) fn from_stat(stat: &Result<PathKind, String>) -> Self {
        match stat {
            Ok(PathKind::Dir) => Self::Folder,
            Ok(PathKind::File | PathKind::Missing | PathKind::Other) | Err(_) => Self::File,
        }
    }
}

impl Shell {
    /// Opens `target`: a path as the app spells it (a `peer://` URI for another engine's),
    /// relative to the current project, with an optional `:line[:column]`.
    pub(super) fn open_path(&mut self, target: &str, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = self.git.read(cx).project().to_owned();
        let (path, location) = crate::file_editor::file_location(target, &cwd);
        if location.is_some() {
            self.open_file_path(cwd, path, location, window, cx);
            return;
        }
        let (engine, named) = crate::engine_client::for_project(&path, cx);
        let stat = engine.call::<Stat>(None, Stat { path: named });
        cx.spawn(async move |this, cx| {
            let opens = Opens::from_stat(&stat.await.map(|stat| stat.kind));
            let _ = this.update_in(cx, |this, window, cx| match opens {
                Opens::Folder => this.show_folder(&path, window, cx),
                Opens::File => this.open_file_path(cwd, path, None, window, cx),
            });
        })
        .detach();
    }

    /// Opens the file `path` of the project `cwd` in a review tab, at `location`.
    fn open_file_path(
        &mut self,
        cwd: String,
        path: String,
        location: Option<(usize, usize)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_review(crate::review::ReviewTab::File { cwd, path }, window, cx);
        if let Some((line, column)) = location
            && let Some(pane) = self.active_review(cx)
        {
            pane.update(cx, |pane, cx| pane.navigate_file(line, column, cx));
        }
    }

    /// Shows the explorer at `folder`: the Files tab (or the classic sidebar's) opened, the
    /// folder revealed, expanded and selected. A folder outside the current project, which the
    /// tree cannot show, says so where file actions report.
    pub(super) fn show_folder(
        &mut self,
        folder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings.is_some() {
            self.close_settings(window, cx);
        }
        self.leave_app_views(cx);
        self.ensure_right_panel(window, cx);
        if self.workspace.classic_layout {
            self.select_classic_sidebar_tab(classic_sidebar::ClassicSidebarTab::Files, window, cx);
        } else {
            self.set_right_panel_open(true, window, cx);
            if let Some(panel) = &self.right_panel {
                panel.update(cx, |panel, cx| {
                    panel.select(crate::right_panel::RightTab::Files, cx);
                });
            }
        }
        let Some(explorer) = self
            .right_panel
            .as_ref()
            .map(|panel| panel.read(cx).explorer.clone())
        else {
            return;
        };
        let shown = explorer.update(cx, |explorer, cx| explorer.reveal_folder(folder, cx));
        if !shown {
            // Named as the machine holding it names it.
            let named = onicode_protocol::ProjectRef::parse_uri(folder)
                .map_or_else(|| folder.to_owned(), |project| project.path);
            let message = format!("{named} is outside this project");
            self.notification_state
                .file_error
                .update(cx, |notice, cx| notice.set_message(Some(message), cx));
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    /// The files open in the active tab's review pane.
    fn file_tabs(shell: &Shell, cx: &App) -> Vec<String> {
        shell
            .workspace
            .active
            .and_then(|id| shell.reviews.get(&id))
            .map(|pane| {
                pane.read(cx)
                    .state
                    .tabs
                    .iter()
                    .filter_map(|tab| match tab {
                        crate::review::ReviewTab::File { path, .. } => Some(path.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn only_a_folder_the_engine_names_opens_the_explorer() {
        assert_eq!(Opens::from_stat(&Ok(PathKind::Dir)), Opens::Folder);
        for kind in [PathKind::File, PathKind::Missing, PathKind::Other] {
            assert_eq!(Opens::from_stat(&Ok(kind)), Opens::File);
        }
        assert_eq!(
            Opens::from_stat(&Err("This engine is offline".into())),
            Opens::File
        );
    }

    /// A temp folder of its own, holding `sub/inner/notes.md`, gone with the test.
    struct Folder(std::path::PathBuf);

    impl Folder {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "onicode-folder-link-{}-{}",
                std::process::id(),
                onicode_protocol::OpId::new()
            ));
            std::fs::create_dir_all(root.join("sub/inner")).unwrap();
            std::fs::write(root.join("sub/inner/notes.md"), "# Notes\n").unwrap();
            Self(root)
        }

        fn path(&self, under: &str) -> String {
            let path = if under.is_empty() {
                self.0.clone()
            } else {
                self.0.join(under)
            };
            path.to_string_lossy().into_owned()
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A link to a folder of the project opens the explorer there, revealed and expanded,
    /// rather than a file tab; a link to a file in it still opens the file.
    #[gpui::test]
    fn a_folder_link_opens_the_explorer_at_the_folder(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let folder = Folder::new();
        let root = folder.path("");
        let (window, _, _, id) = super::super::tests::seeded_window(cx, |store| {
            store.create("claude", &root).unwrap().id
        });
        let cx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        window
            .update(cx, |shell, w, cx| shell.open(id, w, cx))
            .unwrap();
        cx.run_until_parked();
        let sub = folder.path("sub/inner");
        window
            .update(cx, |shell, w, cx| {
                assert_eq!(shell.git.read(cx).project(), root);
                shell.open_path(&sub, w, cx);
            })
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let panel = shell.right_panel.clone().expect("the panel opened");
                if !shell.workspace.classic_layout {
                    assert_eq!(panel.read(cx).tab(), crate::right_panel::RightTab::Files);
                }
                let explorer = panel.read(cx).explorer.clone();
                assert!(
                    explorer.read(cx).is_revealed(&sub),
                    "the folder shows expanded and selected, with what it holds"
                );
                assert!(file_tabs(shell, cx).is_empty(), "no file tab for a folder");
            })
            .unwrap();
        let notes = folder.path("sub/inner/notes.md");
        window
            .update(cx, |shell, w, cx| shell.open_path(&notes, w, cx))
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                assert_eq!(file_tabs(shell, cx), vec![notes.clone()]);
            })
            .unwrap();
    }

    /// A link to a folder on another engine asks that engine, in its own spelling; one that
    /// does not answer (not reached here) opens as a file, as before. Its answer that it is a
    /// folder opens the explorer there, in the app's spelling.
    #[gpui::test]
    fn a_peer_folder_link_asks_the_engine_holding_it(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, _, _, ()) = super::super::tests::seeded_window(cx, |_| ());
        let cx = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        let peer = "peer://nStudio/Users/me/api/src";
        window
            .update(cx, |shell, w, cx| shell.open_path(peer, w, cx))
            .unwrap();
        cx.run_until_parked();
        window
            .update(cx, |shell, _, cx| {
                let stat = crate::engine_client::calls_for_test(&shell.engine, cx)
                    .into_iter()
                    .find(|(name, _)| *name == "Stat")
                    .map(|(_, params)| params)
                    .expect("the path's engine is asked what it is");
                assert_eq!(stat["engine"], "nStudio");
                assert_eq!(stat["body"]["path"], "/Users/me/api/src");
                assert_eq!(file_tabs(shell, cx), vec![peer.to_owned()]);
            })
            .unwrap();
    }
}
