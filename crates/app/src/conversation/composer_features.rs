use std::path::PathBuf;

use onicode_protocol::files::{Bytes, FileContents, NewAttachment, Read, SaveAttachments};

use super::*;

impl Conversation {
    pub(crate) fn request_folder(&mut self, cx: &mut Context<Self>) {
        self.completion
            .update(cx, |completion, cx| completion.dismiss(cx));
        cx.emit(ChooseFolder(self.composer_anchor.clone()));
        cx.notify();
    }

    pub(crate) fn folder_chosen(&mut self, cx: &mut Context<Self>) {
        self.folder_selected = true;
        let text = self.input.read(cx).content.clone();
        if text.trim().eq_ignore_ascii_case("/add-to-folder") {
            self.input
                .update(cx, |input, cx| input.set_text(format!("{text} "), cx));
        }
    }

    pub(crate) fn enable_mode(&mut self, id: &str, cx: &mut Context<Self>) {
        let enabled = match id {
            "draft" => self.follow_ups.draft_selected,
            "plan" => self.settings.plan,
            _ => true,
        };
        if !enabled {
            self.toggle_mode(id, cx);
        }
    }

    pub(crate) fn clear_mode(&mut self, id: &str, cx: &mut Context<Self>) {
        let enabled = match id {
            "draft" => &mut self.follow_ups.draft_selected,
            "plan" => &mut self.settings.plan,
            _ => return,
        };
        if *enabled {
            // The input has already been edited. Clearing its old mode must
            // preserve the replacement text and the user's caret position.
            *enabled = false;
            self.save_settings(cx);
            cx.notify();
        }
    }

    pub(super) fn clear_submitted_mode(&mut self, cx: &mut Context<Self>) {
        // The accepted turn or queue item already owns its mode. The empty
        // composer starts a fresh draft, matching Composer.tsx's submit reset.
        self.settings.plan = false;
        self.input
            .update(cx, |input, cx| input.selected_command(None, cx));
        self.save_settings(cx);
        cx.notify();
    }

    pub(crate) fn toggle_mode(&mut self, id: &str, cx: &mut Context<Self>) {
        if id != "draft" {
            self.follow_ups.draft_selected = false;
        }
        match id {
            "draft" => {
                self.follow_ups.draft_selected = !self.follow_ups.draft_selected;
                self.settings.plan = false;
            }
            "plan" => self.settings.plan = !self.settings.plan,
            _ => return,
        }
        let selected = if self.follow_ups.draft_selected {
            Some("draft")
        } else if self.settings.plan {
            Some("plan")
        } else {
            None
        };
        self.input.update(cx, |input, cx| {
            let leading = input::Command::leading(&input.content);
            if leading.map(input::Command::name) != selected {
                let body = leading.map_or(input.content.as_ref(), |mode| {
                    input.content[mode.name().len() + 1..].trim_start()
                });
                let next =
                    selected.map_or_else(|| body.to_owned(), |name| format!("/{name} {body}"));
                input.set_text(next, cx);
            }
        });
        self.save_settings(cx);
        cx.notify();
    }

    pub(super) fn consume_mode_command(&mut self, text: String, cx: &mut Context<Self>) -> String {
        let Some(rest) = onicode_protocol::composer::consume_command(&text, "plan") else {
            return text;
        };
        let rest = rest.to_owned();
        self.settings.plan = true;
        self.save_settings(cx);
        rest
    }

    pub(super) fn choose_attachments(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach files".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |this, cx| this.add_attachments(paths, cx));
            }
        })
        .detach();
    }

    pub(super) fn paste_attachments(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(clipboard) = cx.read_from_clipboard() else {
            return false;
        };
        let paths: Vec<_> = clipboard
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                gpui::ClipboardEntry::ExternalPaths(paths) => Some(paths.paths().to_vec()),
                _ => None,
            })
            .flatten()
            .collect();
        if !paths.is_empty() {
            self.add_attachments(paths, cx);
            return true;
        }
        let images: Vec<_> = clipboard
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                gpui::ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            })
            .collect();
        if images.is_empty() {
            return false;
        }
        if self.attaching {
            return true;
        }
        self.attaching = true;
        // The engine keeps them with the conversation, so deleting it removes them. No op id:
        // the composer needs the saved paths back now, so a machine that is away answers that it
        // is offline instead of holding the call in the outbox the way a sent prompt waits.
        let save = crate::engine_client::call::<SaveAttachments>(
            &self.engine,
            None,
            SaveAttachments {
                session: Some(self.engine.row(self.summary.id)),
                kept: self
                    .attachments
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                images: images
                    .iter()
                    .map(|image| NewAttachment {
                        bytes: Bytes(image.bytes().to_vec()),
                        extension: image.format().extension().to_owned(),
                        name: None,
                    })
                    .collect(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let result = save
                .await
                .map(|saved| saved.into_iter().map(PathBuf::from).collect());
            let _ = this.update(cx, |this, cx| {
                this.finish_attachment_read(result, cx);
            });
        })
        .detach();
        cx.notify();
        true
    }

    pub(super) fn add_attachments(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            if !self.attachments.contains(&path) && !self.pending_attachments.contains(&path) {
                self.pending_attachments.push(path);
            }
        }
        // Rapid successive drops must wait for the active read, rather than
        // silently losing the later files (including during an image paste).
        if self.attaching || self.pending_attachments.is_empty() {
            return;
        }
        let incoming = std::mem::take(&mut self.pending_attachments);
        self.attaching = true;
        let elsewhere = incoming.iter().any(|path| {
            path.to_str()
                .is_some_and(|path| onicode_protocol::ProjectRef::parse_uri(path).is_some())
        });
        let work = if self.engine.target().is_some() || elsewhere {
            self.carry_attachments(incoming, cx)
        } else {
            let mut all = self.attachments.clone();
            all.extend(incoming.iter().cloned());
            cx.background_executor().spawn(async move {
                onicode_core::composer::validate_attachments(&all).map(|_| incoming)
            })
        };
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.finish_attachment_read(result, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// `incoming`, for a session whose engine cannot read every one of them, as that engine
    /// names them: a file of that engine already (its project's explorer drags one in) as it
    /// is; one from this Mac, or from a third engine (read through it as a preview reads it:
    /// text or an image), carried as its bytes, kept with the conversation there under its own
    /// name (`SaveAttachments`), which checks them against the composer's limits beside the
    /// attachments it holds.
    fn carry_attachments(
        &self,
        incoming: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> gpui::Task<Result<Vec<PathBuf>, String>> {
        use onicode_core::composer::MAX_EMBED_BYTES;
        let engine = self.engine.clone();
        let session = engine.row(self.summary.id);
        let kept: Vec<String> = self
            .attachments
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        cx.background_executor().spawn(async move {
            let mut there = Vec::new();
            let mut files = Vec::new();
            let mut total = 0;
            for path in incoming {
                let spelled = path.to_string_lossy().into_owned();
                match onicode_protocol::ProjectRef::parse_uri(&spelled) {
                    Some(file) if Some(&file.engine) == engine.target() => there.push(file.path),
                    None if engine.target().is_none() => there.push(spelled),
                    Some(file) => {
                        let name = std::path::Path::new(&file.path)
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .ok_or_else(|| format!("{spelled} is not a file."))?;
                        let read = engine
                            .on(file.engine)
                            .call::<Read>(
                                None,
                                Read {
                                    path: file.path,
                                    unless: None,
                                },
                            )
                            .await?;
                        let bytes = carried(read, &name)?;
                        total += bytes.len() as u64;
                        if total > MAX_EMBED_BYTES {
                            return Err("Attachments must total 20 MB or less.".to_owned());
                        }
                        files.push(NewAttachment {
                            bytes: Bytes(bytes),
                            extension: String::new(),
                            name: Some(name),
                        });
                    }
                    None => {
                        let shown = path.display();
                        let meta = path.metadata().map_err(|e| format!("{shown}: {e}"))?;
                        if !meta.is_file() {
                            return Err(format!("{shown} is not a file."));
                        }
                        total += meta.len();
                        if total > MAX_EMBED_BYTES {
                            return Err("Attachments must total 20 MB or less.".to_owned());
                        }
                        let name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .ok_or_else(|| format!("{shown} is not a file."))?;
                        let bytes = std::fs::read(&path).map_err(|e| format!("{shown}: {e}"))?;
                        files.push(NewAttachment {
                            bytes: Bytes(bytes),
                            extension: String::new(),
                            name: Some(name),
                        });
                    }
                }
            }
            // No op id, as for a pasted image: an away machine is said, not waited for.
            let save = engine.call::<SaveAttachments>(
                None,
                SaveAttachments {
                    session: Some(session),
                    kept: kept.into_iter().chain(there.iter().cloned()).collect(),
                    images: files,
                },
            );
            let saved = save.await?;
            Ok(there.into_iter().chain(saved).map(PathBuf::from).collect())
        })
    }

    fn finish_attachment_read(
        &mut self,
        result: Result<Vec<PathBuf>, String>,
        cx: &mut Context<Self>,
    ) {
        self.attaching = false;
        match result {
            Ok(paths) => {
                // Keep removals made while the background validation ran.
                for path in paths {
                    if !self.attachments.contains(&path) {
                        self.attachments.push(path);
                    }
                }
                self.attachment_error = None;
            }
            Err(error) => self.attachment_error = Some(error),
        }
        let pending = std::mem::take(&mut self.pending_attachments);
        self.add_attachments(pending, cx);
        cx.notify();
    }

    pub(super) fn attachment_chips(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Removed chips release their thumbnail sprites with the entity.
        let attachments = &self.attachments;
        self.attachment_photos
            .retain(|path, _| attachments.contains(path));
        if self.attachments.is_empty() && self.attachment_error.is_none() && !self.attaching {
            return None;
        }
        // A session on another engine keeps its attachments there: they are read through it.
        let engine = self.engine.clone();
        let shown = move |path: &PathBuf| engine.app_path(&path.to_string_lossy());
        for path in &self.attachments {
            if let Some(mime) = onicode_protocol::composer::image_mime(path)
                && !self.attachment_photos.contains_key(path)
            {
                let origin = crate::transcript::attachments::Origin::Path(shown(path).into());
                let photo = cx
                    .new(|cx| crate::transcript::attachments::Photo::new(origin, mime.into(), cx));
                self.attachment_photos.insert(path.clone(), photo);
            }
        }
        Some(
            div()
                .debug_selector(|| "composer-attachments".into())
                .px_3()
                .pt_3()
                .flex()
                .flex_wrap()
                .gap_1p5()
                .children(self.attachments.iter().enumerate().map(|(ix, path)| {
                    let label = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let photo = self.attachment_photos.get(path).cloned();
                    let image = onicode_protocol::composer::image_mime(path).is_some();
                    let remove = div()
                        .id(("remove-attachment", ix))
                        .role(Role::Button)
                        .aria_label(format!("Remove {label}"))
                        .cursor_pointer()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(image, |d| {
                            d.absolute()
                                .top(px(-4.))
                                .right(px(-4.))
                                .size(px(20.))
                                .rounded_full()
                                .bg(content(0.2))
                                .text_color(content(0.7))
                                .hover(|s| s.bg(content(0.3)).text_color(content(1.)))
                        })
                        .when(!image, |d| {
                            d.h(px(26.))
                                .px(px(6.))
                                .rounded_md()
                                .bg(content(0.1))
                                .text_color(content(1.))
                                .hover(|s| s.bg(content(0.15)))
                        })
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(icon("close", if image { 12. } else { 10. }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if ix < this.attachments.len() {
                                this.attachments.remove(ix);
                                cx.notify();
                            }
                        }));
                    if image {
                        let spelled = shown(path);
                        let source = if onicode_protocol::ProjectRef::parse_uri(&spelled).is_some()
                        {
                            crate::image_lightbox::Source::Peer(spelled)
                        } else {
                            crate::image_lightbox::Source::Path(path.clone())
                        };
                        let preview = crate::image_lightbox::Open {
                            source,
                            alt: label.clone().into(),
                        };
                        return div()
                            .id(("attachment", ix))
                            .relative()
                            .size(px(36.))
                            .flex_shrink_0()
                            .role(Role::Button)
                            .focusable()
                            .debug_selector(move || format!("composer-image-{ix}"))
                            .aria_label(format!("Open {label} full screen"))
                            .cursor(CursorStyle::PointingHand)
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                window.dispatch_action(Box::new(preview.clone()), cx);
                            })
                            .children(photo)
                            .child(remove)
                            .into_any_element();
                    }
                    div()
                        .id(("attachment", ix))
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(content(0.06))
                        .text_size(px(11.))
                        .child(crate::file_icons::file_type_icon(&label, false, false, 14.))
                        .child(div().max_w(px(190.)).truncate().child(label))
                        .child(remove)
                        .into_any_element()
                }))
                .when(self.attaching, |d| {
                    d.child(
                        div()
                            .text_size(px(11.))
                            .text_color(content(0.5))
                            .child("Attaching…"),
                    )
                })
                .when_some(self.attachment_error.clone(), |d, error| {
                    d.child(
                        div()
                            .text_size(px(11.))
                            .text_color(tint(AMBER_400, 0.9))
                            .child(error),
                    )
                })
                .into_any_element(),
        )
    }

    pub(crate) fn external_outcome(&self, cx: &gpui::App) -> (bool, String) {
        let rows = self.transcript.read(cx).source_rows();
        let start = rows.iter().rposition(|row| row.role == "user").unwrap_or(0);
        let text = onicode_protocol::response::prose(&rows[start..]);
        (
            self.status == "Ready",
            onicode_protocol::response::limit(&format!("{}\n{}", self.status, text), 12_000),
        )
    }

    /// Managed runs must wait for history restoration and worktree setup as
    /// well as the provider, including submissions deferred by the editor.
    pub(crate) fn external_submission_pending(&self) -> bool {
        self.busy
            || self.stopping
            || self.preparing_worktree
            || self.attaching
            || self.account_switching
            || self.loading
            || (!self.history_actions.is_empty() && self.history_ready())
            || self.editing_last_turn.pending()
    }

    /// An evicted history can be restored for a managed run; a user's draft
    /// or an already accepted message must keep ownership of this composer.
    pub(crate) fn available_for_managed_run(&self, cx: &gpui::App) -> bool {
        !self.busy
            && !self.stopping
            && !self.preparing_worktree
            && !self.attaching
            && !self.account_switching
            && !self.editing_last_turn.pending()
            && self.input.read(cx).content.is_empty()
            && self.attachments.is_empty()
            && self.follow_ups.state.messages.is_empty()
            && self.follow_ups.state.draft.is_none()
            && !self.settings.worktree_removed
    }
}

/// The bytes a file of a third engine is attached with, as that engine read it for a preview:
/// a text file's text or an image's bytes; anything else says why it stays there.
fn carried(read: FileContents, name: &str) -> Result<Vec<u8>, String> {
    match read {
        FileContents::Image(image) => Ok(image.bytes.0),
        FileContents::Text(text) => Ok(text.text.into_bytes()),
        FileContents::Unsupported(reason) => Err(format!("{name}: {reason}")),
        FileContents::Unchanged | FileContents::Video { .. } => Err(format!(
            "{name} can be attached only to a chat on the machine that holds it."
        )),
    }
}

#[cfg(test)]
mod tests {
    use onicode_protocol::{EngineId, SessionId, SessionKey};
    use serde_json::json;

    use super::*;

    /// A file attached from this Mac to a session on another machine reaches that machine as
    /// its bytes under its own name, kept with the conversation there beside what the composer
    /// holds, where a file of that machine already is named as it names it. While the machine
    /// is away nothing is attached and the composer says why.
    #[gpui::test]
    fn a_file_attached_to_a_peer_session_carries_its_bytes(cx: &mut gpui::TestAppContext) {
        let local = cx.update(crate::engine_client::Engine::memory);
        let studio = local.on(EngineId("nStudio".into()));
        let key = SessionKey {
            engine: EngineId("nStudio".into()),
            session: SessionId::new(),
        };
        let row = crate::engine_client::peer_row(&key);
        studio.adopt(row, 7, key.session);
        let summary = Session {
            id: row,
            title: "On the studio".into(),
            provider: "codex".into(),
            cwd: "peer://nStudio/Users/me/app".into(),
            provider_id: None,
            has_messages: false,
            updated_at: 1,
            archived: false,
            pinned: false,
            work_item: None,
        };
        let view = cx.new(|cx| Conversation::new(summary, studio, cx));
        let dir = std::env::temp_dir().join(format!("onicode-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.md");
        std::fs::write(&file, "# Notes").unwrap();
        view.update(cx, |view, cx| {
            let there = PathBuf::from("peer://nStudio/Users/me/app/README.md");
            view.add_attachments(vec![file.clone(), there], cx);
        });
        cx.run_until_parked();
        let calls = cx.update(|cx| crate::engine_client::calls_for_test(&local, cx));
        let (_, save) = calls
            .iter()
            .find(|(name, _)| *name == "SaveAttachments")
            .expect("the bytes go to the studio");
        assert_eq!(save["engine"], "nStudio");
        assert_eq!(save["body"]["session"], 7, "the row the studio keeps it at");
        assert_eq!(save["body"]["kept"], json!(["/Users/me/app/README.md"]));
        let body: SaveAttachments = serde_json::from_value(save["body"].clone()).unwrap();
        assert_eq!(body.images.len(), 1);
        assert_eq!(body.images[0].name.as_deref(), Some("notes.md"));
        assert_eq!(body.images[0].bytes.0, b"# Notes");
        view.read_with(cx, |view, _| {
            assert!(view.attachments.is_empty(), "{:?}", view.attachments);
            let error = view.attachment_error.clone().unwrap_or_default();
            assert!(error.contains("offline"), "{error}");
        });
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A file of a third machine, dragged from its project into a chat on another, is read
    /// through the machine that holds it, as a preview reads it, before its bytes go to the
    /// chat's; while that machine is away nothing is attached and the composer says why.
    #[gpui::test]
    fn a_file_of_a_third_machine_is_read_where_it_is(cx: &mut gpui::TestAppContext) {
        let local = cx.update(crate::engine_client::Engine::memory);
        let studio = local.on(EngineId("nStudio".into()));
        let key = SessionKey {
            engine: EngineId("nStudio".into()),
            session: SessionId::new(),
        };
        let row = crate::engine_client::peer_row(&key);
        studio.adopt(row, 7, key.session);
        let summary = Session {
            id: row,
            title: "On the studio".into(),
            provider: "codex".into(),
            cwd: "peer://nStudio/Users/me/app".into(),
            provider_id: None,
            has_messages: false,
            updated_at: 1,
            archived: false,
            pinned: false,
            work_item: None,
        };
        let view = cx.new(|cx| Conversation::new(summary, studio, cx));
        view.update(cx, |view, cx| {
            let laptop = PathBuf::from("peer://nLaptop/Users/me/notes.md");
            view.add_attachments(vec![laptop], cx);
        });
        cx.run_until_parked();
        let calls = cx.update(|cx| crate::engine_client::calls_for_test(&local, cx));
        let (_, read) = calls
            .iter()
            .find(|(name, params)| *name == "Read" && params["engine"] == "nLaptop")
            .expect("the file is read on the laptop");
        assert_eq!(read["body"]["path"], "/Users/me/notes.md");
        assert!(
            calls.iter().all(|(name, _)| *name != "SaveAttachments"),
            "nothing reaches the studio while the laptop is away"
        );
        view.read_with(cx, |view, _| {
            assert!(view.attachments.is_empty(), "{:?}", view.attachments);
            let error = view.attachment_error.clone().unwrap_or_default();
            assert!(error.contains("offline"), "{error}");
        });
    }
}
