//! Commit pill turns ("Stage your changes", …). While the agent works, one
//! line says what it is doing; once it settles, a card leads its Markdown reply
//! with what the turn did to the checkout, as the engine read it from Git when
//! the turn ended (the turn receipt's [`TurnGit`]).
use gpui::{AnyElement, Div};
use onicode_protocol::git::{CommitFile, TurnGit};

use super::*;
use crate::conversation::git_actions::GitAction;
use crate::right_panel::{DIFF_ADD, DIFF_DEL, format_integer, tabular};

/// The card shows this many files before a "more" line.
const FILES: usize = 4;

pub(super) struct GitCard {
    action: GitAction,
    /// The turn is still running: the card is its one status line.
    live: bool,
    git: TurnGit,
    /// The reply the card sits above.
    pub(super) answer: Option<usize>,
}
impl GitCard {
    pub(super) fn live(&self) -> bool {
        self.live
    }
}

/// Commit-loaded file lists, by hash; `None` while one loads.
pub(super) type CommitFiles = HashMap<String, Option<Arc<Vec<CommitFile>>>>;

fn live_label(action: GitAction) -> &'static str {
    match action {
        GitAction::Stage => "Staging…",
        GitAction::Commit => "Committing…",
        GitAction::CommitPush => "Committing and pushing…",
        GitAction::Push => "Pushing…",
    }
}

/// A commit's hash as the card shows it.
fn short(hash: &str) -> &str {
    hash.get(..7).unwrap_or(hash)
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{count} {}", if count == 1 { one } else { many })
}

fn counts(additions: u64, deletions: u64) -> Div {
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .font_features(tabular())
        .child(
            div()
                .text_color(rgb(DIFF_ADD))
                .child(format!("+{}", format_integer(additions as i64))),
        )
        .child(
            div()
                .text_color(rgb(DIFF_DEL))
                .child(format!("-{}", format_integer(deletions as i64))),
        )
}

/// One file of the card: what changed and how much; `None` for binary files.
struct FileLine {
    path: String,
    counts: Option<(u64, u64)>,
}

impl TranscriptView {
    /// The hash of the session's most recent commit, from any turn.
    pub(crate) fn latest_commit(&self) -> Option<String> {
        self.turns
            .iter()
            .filter_map(|(turn, info)| Some((*turn, info.git.as_ref()?.commits.last()?)))
            .max_by_key(|(turn, _)| *turn)
            .map(|(_, commit)| commit.hash.clone())
    }

    /// The card for the pill turn at `turn`, or `None` for other turns and
    /// for settled ones whose receipt shows nothing the action did.
    pub(super) fn git_card(
        &self,
        turn: usize,
        turn_end: usize,
        final_answer: Option<usize>,
    ) -> Option<Arc<GitCard>> {
        if turn == 0 {
            return None;
        }
        let prompt: String = self.source[prompt_start(&self.source, turn)..turn]
            .iter()
            .filter(|row| row.role == "user")
            .map(|row| row.text.as_str())
            .collect();
        let action = GitAction::from_message(&prompt)?;
        if self.streaming && turn_end == self.source.len() {
            return Some(Arc::new(GitCard {
                action,
                live: true,
                git: TurnGit::default(),
                answer: None,
            }));
        }
        let answer = final_answer?;
        let git = self.turns.get(&turn)?.git.clone()?;
        let shown = match action {
            GitAction::Stage => !git.staged.is_empty() || !git.commits.is_empty(),
            GitAction::Commit => !git.commits.is_empty(),
            GitAction::CommitPush => !git.commits.is_empty() || git.push.is_some(),
            GitAction::Push => git.push.is_some(),
        };
        if !shown {
            return None;
        }
        Some(Arc::new(GitCard {
            action,
            live: false,
            git,
            answer: Some(answer),
        }))
    }

    pub(super) fn git_card_row(
        &mut self,
        card: &GitCard,
        message_start: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if card.live {
            return self.git_live_line(card.action, message_start, cx);
        }
        let git = &card.git;
        let commit = git.commits.last();
        let push = git.push.as_ref();
        // Asked to push, the turn left commits the upstream lacks.
        let push_failed = matches!(card.action, GitAction::CommitPush | GitAction::Push)
            && push.is_none()
            && git.ahead > 0;
        let files = self.git_card_files(card, message_start, cx);
        let (additions, deletions) = if commit.is_some() {
            git.commits.iter().fold((0, 0), |(a, d), commit| {
                (a + commit.additions, d + commit.deletions)
            })
        } else {
            git.staged.iter().fold((0, 0), |(a, d), file| {
                (
                    a + file.additions.unwrap_or(0),
                    d + file.deletions.unwrap_or(0),
                )
            })
        };
        let has_counts = additions + deletions > 0;
        let title: SharedString = match (commit, push) {
            (Some(commit), _) => commit.subject.clone().into(),
            (None, Some(push)) => format!("Pushed {}", push.upstream).into(),
            (None, None) => format!("Staged {}", plural(git.staged.len(), "file", "files")).into(),
        };

        let dot = || div().text_color(content(0.25)).child("·");
        let mut subtitle = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(6.))
            .text_size(px(11.))
            .line_height(px(16.))
            .text_color(content(0.5))
            .overflow_hidden();
        let mut parts = 0;
        if let Some(commit) = commit {
            let copied = self
                .copied_commit
                .as_ref()
                .is_some_and(|(hash, _)| *hash == commit.hash);
            let hash = commit.hash.clone();
            subtitle = subtitle.child(
                div()
                    .id(("git-card-hash", message_start))
                    .role(Role::Button)
                    .aria_label(if copied {
                        "Copied the commit hash".to_string()
                    } else {
                        format!("Copy commit {}", short(&commit.hash))
                    })
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(15.))
                    .px(px(4.))
                    .rounded(px(4.))
                    .bg(content(0.06))
                    .font_family(crate::fonts::mono())
                    .text_size(px(10.))
                    .text_color(content(0.7))
                    .cursor_pointer()
                    .hover(|s| s.bg(content(0.1)).text_color(content(1.)))
                    .child(if copied {
                        icon("check", 11.)
                            .text_color(rgb(DIFF_ADD))
                            .into_any_element()
                    } else {
                        div()
                            .child(short(&commit.hash).to_owned())
                            .into_any_element()
                    })
                    .when(copied, |d| d.child("Copied"))
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()));
                        let born = Instant::now();
                        this.copied_commit = Some((hash.clone(), born));
                        cx.notify();
                        cx.spawn(async move |this, cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_secs(2))
                                .await;
                            let _ = this.update(cx, |this, cx| {
                                if this.copied_commit.as_ref().is_some_and(|c| c.1 == born) {
                                    this.copied_commit = None;
                                    cx.notify();
                                }
                            });
                        })
                        .detach();
                    })),
            );
            parts += 1;
            if let Some(branch) = &git.branch {
                subtitle = subtitle.child(dot()).child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .child(icon("git-branch", 12.).text_color(content(0.4)))
                        .child(div().truncate().child(branch.clone())),
                );
            }
            if git.commits.len() > 1 {
                subtitle = subtitle
                    .child(dot())
                    .child(div().flex_shrink_0().child(plural(
                        git.commits.len(),
                        "commit",
                        "commits",
                    )));
            }
        }
        if push.is_some() || push_failed {
            if parts > 0 {
                subtitle = subtitle.child(dot());
            }
            parts += 1;
            subtitle =
                subtitle.child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .when(push_failed, |d| d.text_color(rgb(0xf87171)))
                        .child(
                            icon(if push_failed { "close" } else { "cloud-upload" }, 12.)
                                .text_color(if push_failed {
                                    rgb(0xf87171).into()
                                } else {
                                    content(0.4)
                                }),
                        )
                        .child(div().truncate().child(match push {
                            None => "Not pushed".to_string(),
                            Some(push) if commit.is_none() => {
                                push.repo.clone().unwrap_or_else(|| "Remote updated".into())
                            }
                            Some(push) => push.repo.as_ref().map_or_else(
                                || "Pushed".into(),
                                |repo| format!("Pushed to {repo}"),
                            ),
                        })),
                );
        }
        if commit.is_none() && push.is_none() {
            parts += 1;
            subtitle = subtitle.child(div().child("Ready to commit"));
        }
        if has_counts {
            if parts > 0 {
                subtitle = subtitle.child(dot());
            }
            subtitle = subtitle.child(counts(additions, deletions));
        }

        let link = push.and_then(|push| {
            let web = push.web.as_ref()?;
            let commit = commit?;
            web.starts_with("https://github.com/").then(|| {
                (
                    "View commit",
                    "external-link",
                    format!("{web}/commit/{}", commit.hash),
                )
            })
        });

        let (badge, badge_color) = if push_failed {
            ("close", rgb(0xf87171))
        } else {
            ("check", rgb(DIFF_ADD))
        };
        let header = div()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(10.))
            .px_3()
            .py(px(10.))
            .child(
                div()
                    .relative()
                    .size(px(32.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.))
                    .bg(content(0.08))
                    .child(icon(card.action.icon(), 16.).text_color(content(0.6)))
                    .child(
                        div()
                            .absolute()
                            .right(px(-3.))
                            .bottom(px(-3.))
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(rgb(CHAT))
                            .child(
                                div()
                                    .size(px(12.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .bg(badge_color)
                                    .child(icon(badge, 8.).text_color(rgb(CHAT))),
                            ),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(content(0.85))
                            .child(title),
                    )
                    .child(subtitle),
            )
            .when_some(link, |d, (label, glyph, url)| {
                d.child(
                    div()
                        .id(("git-card-link", message_start))
                        .role(Role::Link)
                        .aria_label(label)
                        .flex_shrink_0()
                        .h(px(28.))
                        .px(px(10.))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(6.))
                        .bg(content(0.08))
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(content(0.75))
                        .cursor_pointer()
                        .hover(|s| s.bg(content(0.12)).text_color(content(1.)))
                        .child(icon(glyph, 13.).text_color(content(0.6)))
                        .child(label)
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
            });

        let hidden = files.len().saturating_sub(FILES);
        let base = self.file_link_base.clone();
        let rows = files.into_iter().take(FILES).enumerate().map(|(ix, file)| {
            let name = std::path::Path::new(&file.path)
                .file_name()
                .map_or_else(|| file.path.clone(), |n| n.to_string_lossy().into_owned());
            // In a session on another engine, the file opens on that engine.
            let path = base.as_deref().map_or_else(
                || file.path.clone(),
                |base| {
                    let joined = std::path::Path::new(base).join(&file.path);
                    crate::engine_client::app_path_beside(base, &joined.to_string_lossy())
                },
            );
            div()
                .id(("git-card-file", message_start * 64 + ix))
                .role(Role::Link)
                .aria_label(SharedString::from(format!("Open {}", file.path)))
                .h(px(30.))
                .w_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .cursor_pointer()
                .text_color(content(0.65))
                .hover(|s| s.bg(content(0.05)).text_color(content(1.)))
                .child(crate::file_icons::file_type_icon(&name, false, false, 15.))
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .font_family(crate::fonts::mono())
                        .text_size(px(12.))
                        .child(file.path.clone()),
                )
                .when_some(file.counts, |d, (additions, deletions)| {
                    d.child(counts(additions, deletions))
                })
                .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, window, cx| {
                    window
                        .dispatch_action(Box::new(crate::file_editor::OpenPath(path.clone())), cx);
                })
        });
        let rows: Vec<_> = rows.collect();
        div()
            .w_full()
            .py_1()
            .child(
                div()
                    .id(("git-card", message_start))
                    .debug_selector(move || format!("git-card-{message_start}"))
                    .role(Role::Group)
                    .aria_label(SharedString::from(format!(
                        "{}: {}",
                        card.action.label(),
                        title_text(card)
                    )))
                    .w_full()
                    .overflow_hidden()
                    .rounded(px(12.))
                    .border_1()
                    .border_color(content(0.06))
                    .bg(content(0.03))
                    .child(header)
                    .when(!rows.is_empty(), |d| {
                        d.child(
                            div()
                                .border_t_1()
                                .border_color(content(0.05))
                                .py_1()
                                .children(rows)
                                .when(hidden > 0, |d| {
                                    d.child(
                                        div()
                                            .h(px(26.))
                                            .px_3()
                                            .flex()
                                            .items_center()
                                            .text_size(px(11.))
                                            .text_color(content(0.4))
                                            .child(format!(
                                                "+{} more",
                                                plural(hidden, "file", "files")
                                            )),
                                    )
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    /// The running turn's single line: a spinner and the step it is on.
    fn git_live_line(
        &self,
        action: GitAction,
        message_start: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let label = live_label(action).to_owned();
        let still = cx.reduce_motion();
        div()
            .id(("git-live", message_start))
            .debug_selector(move || format!("git-live-{message_start}"))
            .role(Role::Status)
            .aria_label(label.clone())
            .flex()
            .w_full()
            .min_w_0()
            .items_center()
            .gap_2()
            .py_1()
            .text_size(px(14.))
            .line_height(px(20.))
            .text_color(content(0.55))
            .child(
                div()
                    .size(px(14.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if still {
                        icon(action.icon(), 14.).into_any_element()
                    } else {
                        crate::session_mark::spinner(
                            ("git-live-loader", message_start),
                            crate::pulse::Rate::Half,
                        )
                        .into_any_element()
                    }),
            )
            .child(div().min_w_0().truncate().child(if still {
                div().child(label).into_any_element()
            } else {
                crate::activity::shimmer(label, self.shimmer_clock(cx))
            }))
            .into_any_element()
    }

    /// The files to list: the commits' own numbers once Git has read them,
    /// or the staged files.
    fn git_card_files(
        &mut self,
        card: &GitCard,
        message_start: usize,
        cx: &mut Context<Self>,
    ) -> Vec<FileLine> {
        if card.git.commits.is_empty() {
            return card
                .git
                .staged
                .iter()
                .map(|file| FileLine {
                    path: file.path.clone(),
                    counts: file.additions.zip(file.deletions),
                })
                .collect();
        }
        let mut files: Vec<FileLine> = Vec::new();
        for commit in &card.git.commits {
            match self.commit_files.get(&commit.hash) {
                Some(Some(loaded)) => {
                    for file in loaded.iter() {
                        let counts = file.additions.zip(file.deletions);
                        match files.iter_mut().find(|f| f.path == file.path) {
                            Some(line) => {
                                line.counts =
                                    line.counts.zip(counts).map(|(a, b)| (a.0 + b.0, a.1 + b.1));
                            }
                            None => files.push(FileLine {
                                path: file.path.clone(),
                                counts,
                            }),
                        }
                    }
                }
                Some(None) => {}
                None => self.load_commit_files(commit.hash.clone(), message_start, cx),
            }
        }
        files
    }

    fn load_commit_files(&mut self, hash: String, message_start: usize, cx: &mut Context<Self>) {
        let Some(cwd) = self.file_link_base.clone() else {
            return;
        };
        self.commit_files.insert(hash.clone(), None);
        let (engine, checkout) = crate::engine_client::for_project(&cwd, cx);
        let work = engine.call::<onicode_protocol::git::CommitFiles>(
            None,
            onicode_protocol::git::CommitFiles {
                checkout,
                sha: hash.clone(),
            },
        );
        cx.spawn(async move |this, cx| {
            let files = work.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.commit_files.insert(hash, Some(Arc::new(files)));
                this.rebuild(message_start, cx);
            });
        })
        .detach();
    }
}

fn title_text(card: &GitCard) -> String {
    card.git
        .commits
        .last()
        .map(|commit| commit.subject.clone())
        .or_else(|| card.git.push.as_ref().map(|push| push.upstream.clone()))
        .unwrap_or_else(|| plural(card.git.staged.len(), "file", "files"))
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;
    use onicode_protocol::agent::TurnInfo;
    use onicode_protocol::git::{TurnCommit, TurnPush};

    use super::*;

    fn row(role: &str, text: String) -> Row {
        Row {
            role: role.into(),
            text,
            first: true,
            sent_at: None,
            id: None,
        }
    }

    fn bash(command: &str, state: ToolState) -> String {
        let mut call = ToolCall::new(command, "Bash");
        call.input = serde_json::json!({ "command": command }).to_string();
        call.state = state;
        serde_json::to_string(&call).unwrap()
    }

    fn committed(hash: &str, push: Option<TurnPush>) -> TurnInfo {
        TurnInfo {
            git: Some(TurnGit {
                branch: Some("main".into()),
                commits: vec![TurnCommit {
                    hash: hash.into(),
                    subject: "Center the jump button".into(),
                    files: 1,
                    additions: 2,
                    deletions: 1,
                }],
                push,
                ..TurnGit::default()
            }),
            ..TurnInfo::default()
        }
    }

    fn cards(view: &TranscriptView) -> Vec<Arc<GitCard>> {
        view.rows
            .iter()
            .filter_map(|row| match &row.body {
                Body::GitAction(card) => Some(card.clone()),
                _ => None,
            })
            .collect()
    }

    #[gpui::test]
    fn pill_turns_show_one_live_line_then_a_card_from_the_receipt(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| TranscriptView::new())
            })
            .unwrap()
        });
        let commit = "git add -- a.rs && git commit -m \"Center the jump button\" -- a.rs";
        window
            .update(cx, |view, _, cx| {
                view.streaming(true, cx);
                view.patch(
                    0,
                    vec![
                        row("user", "Commit and push your changes".into()),
                        row("tool", bash("git status --short", ToolState::Completed)),
                        row("tool", bash(commit, ToolState::Running)),
                    ],
                    cx,
                );
            })
            .unwrap();
        let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
        visual.run_until_parked();
        window
            .update(visual, |view, _, _| {
                let cards = cards(view);
                assert_eq!(cards.len(), 1, "one line for the whole running turn");
                assert!(cards[0].live());
                assert!(
                    !view
                        .rows
                        .iter()
                        .any(|row| matches!(row.body, Body::Tool { .. } | Body::Phase { .. })),
                    "its steps stay behind the line"
                );
            })
            .unwrap();
        assert!(visual.debug_bounds("git-live-1").is_some());

        let hash = "3bb0c42d0e7a5b1c9f8e6d4c2b0a9f8e7d6c5b4a";
        window
            .update(visual, |view, _, cx| {
                view.patch(
                    2,
                    vec![
                        row("tool", bash(commit, ToolState::Completed)),
                        row("tool", bash("git push", ToolState::Completed)),
                        row("assistant", "Committed and pushed it.\n\n- a.rs".into()),
                    ],
                    cx,
                );
                view.turns.insert(
                    1,
                    committed(
                        hash,
                        Some(TurnPush {
                            upstream: "origin/main".into(),
                            repo: Some("o/r".into()),
                            web: Some("https://github.com/o/r".into()),
                        }),
                    ),
                );
                view.streaming(false, cx);
            })
            .unwrap();
        visual.run_until_parked();
        window
            .update(visual, |view, _, _| {
                let cards = cards(view);
                assert_eq!(cards.len(), 1);
                assert!(!cards[0].live());
                assert_eq!(cards[0].git.commits[0].hash, hash);
                assert_eq!(short(&cards[0].git.commits[0].hash), "3bb0c42");
                assert!(cards[0].git.push.is_some());
                let card = view
                    .rows
                    .iter()
                    .position(|row| matches!(row.body, Body::GitAction(_)))
                    .unwrap();
                assert!(
                    view.rows[card + 1..]
                        .iter()
                        .any(|row| matches!(row.body, Body::Markdown(_))),
                    "the reply follows the card"
                );
            })
            .unwrap();
        assert!(visual.debug_bounds("git-card-4").is_some());
    }

    #[gpui::test]
    fn ordinary_turns_and_empty_results_keep_their_reply(cx: &mut TestAppContext) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |_, cx| {
                cx.new(|_| TranscriptView::new())
            })
            .unwrap()
        });
        window
            .update(cx, |view, _, cx| {
                view.turns.insert(1, committed("abc1234", None));
                view.patch(
                    0,
                    vec![
                        row("user", "Please commit".into()),
                        row("tool", bash("git commit -m x", ToolState::Completed)),
                        row("assistant", "Done".into()),
                        row("user", "Commit your changes".into()),
                        row("assistant", "I changed nothing in this session.".into()),
                    ],
                    cx,
                );
                assert!(cards(view).is_empty());
                assert_eq!(
                    view.latest_commit().as_deref(),
                    Some("abc1234"),
                    "any turn's commit can still be pushed"
                );
            })
            .unwrap();
    }
}
