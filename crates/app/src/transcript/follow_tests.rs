use gpui::{TestAppContext, WindowBounds, WindowOptions, point};

use super::*;

fn row(role: &str, text: &str) -> Row {
    Row {
        role: role.into(),
        text: text.into(),
        first: true,
        sent_at: None,
        id: None,
    }
}

fn chat(cx: &mut TestAppContext) -> gpui::WindowHandle<TranscriptView> {
    cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(900.), px(600.)),
                ))),
                ..Default::default()
            },
            |_, cx| cx.new(|_| TranscriptView::new()),
        )
        .unwrap()
    })
}

fn render(window: gpui::WindowHandle<TranscriptView>, cx: &mut TestAppContext) {
    window.update(cx, |_, _, cx| cx.notify()).unwrap();
    cx.refresh().unwrap();
    cx.run_until_parked();
}

#[gpui::test]
fn ordinary_send_keeps_the_previous_anchor_until_its_prompt_arrives(cx: &mut TestAppContext) {
    for reading_history in [false, true] {
        let window = chat(cx);
        window
            .update(cx, |view, _, cx| {
                view.smooth_reveal = false;
                view.patch(
                    0,
                    vec![
                        row("user", "Earlier question"),
                        row("assistant", &"Earlier answer.\n\n".repeat(30)),
                    ],
                    cx,
                );
                view.streaming(true, cx);
                view.visible = false;
                view.patch(
                    2,
                    vec![row("user", "Previous prompt"), row("assistant", "Done.")],
                    cx,
                );
                view.streaming(false, cx);
                view.visible = true;
            })
            .unwrap();
        render(window, cx);
        let before = window
            .update(cx, |view, _, cx| {
                if reading_history {
                    view.list.scroll_by(px(-80.));
                }
                let before = view.list.logical_scroll_top();
                view.streaming(true, cx);
                assert_eq!(view.anchor_turn, Some(2));
                assert_eq!(view.list.is_following_tail(), !reading_history);
                before
            })
            .unwrap();
        // The background worker may not deliver the saved user row in the
        // same frame as begin_turn. That intermediate frame must stay still.
        render(window, cx);
        window
            .update(cx, |view, _, cx| {
                let after = view.list.logical_scroll_top();
                assert_eq!(after.item_ix, before.item_ix);
                assert_eq!(after.offset_in_item, before.offset_in_item);
                assert_eq!(view.follow.offset, 0.);
                assert!(view.prompt_rise.is_none());
                view.patch(4, vec![row("user", "New prompt")], cx);
                assert_eq!(view.anchor_turn, Some(4));
                assert_eq!(view.prompt_rise.map(|(at, _)| at), Some(4));
                assert!(view.prompt_rise_pending);
                assert!(view.list.is_following_tail());
                assert!(view.follow.measured.is_none());
            })
            .unwrap();
    }
}

#[gpui::test]
fn optimistic_prompt_anchors_once_and_worker_acceptance_keeps_its_entrance(
    cx: &mut TestAppContext,
) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.patch(
                0,
                vec![
                    row("user", "Earlier question"),
                    row("assistant", "Earlier answer"),
                ],
                cx,
            );
            view.visible = true;
            view.streaming(true, cx);
            view.submit_prompt(
                "New question",
                &onicode_core::agent::Settings::default(),
                cx,
            );
            assert_eq!(view.anchor_turn, Some(2));
            assert_eq!(view.introduced_prompt, Some(2));
            assert!(
                view.rows
                    .iter()
                    .any(|row| matches!(&row.body, Body::Plain(text) if text == "New question"))
            );
            assert!(
                view.rows
                    .iter()
                    .any(|row| matches!(row.body, Body::WorkLine { .. }))
            );
            let entrance = view.prompt_rise;
            assert!(entrance.is_some());
            let accepted = view.source[2..].to_vec();
            view.patch(2, accepted, cx);
            assert_eq!(view.source.len(), 4);
            assert_eq!(
                view.prompt_rise, entrance,
                "saving must not restart the message animation"
            );
            assert_eq!(view.anchor_turn, Some(2));
        })
        .unwrap();
}

#[gpui::test]
fn ordinary_overflow_glides_and_keeps_settling_after_streaming_ends(cx: &mut TestAppContext) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.anchor_prompts = false;
            view.smooth_reveal = false;
            view.streaming(true, cx);
            view.visible = false;
            view.patch(
                0,
                vec![row("user", "Prompt"), row("assistant", "A short reply.")],
                cx,
            );
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.patch(
                1,
                vec![row("assistant", &"Another paragraph.\n\n".repeat(35))],
                cx,
            );
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    let before = window
        .update(cx, |view, _, _| {
            assert!(view.follow.offset > 0.);
            assert!(view.follow.offset <= 0.75 * f32::from(view.viewport.size.height));
            assert!(view.list.is_following_tail());
            view.follow.offset
        })
        .unwrap();
    window
        .update(cx, |view, _, cx| {
            view.streaming(false, cx);
            view.follow.last_frame = Some(Instant::now() - std::time::Duration::from_millis(16));
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            assert!(
                view.follow.offset >= 0. && view.follow.offset < before,
                "completion must continue the glide rather than snap to the end"
            );
        })
        .unwrap();
}

#[gpui::test]
fn ordinary_height_collapse_eases_back_and_a_new_prompt_resets_follow(cx: &mut TestAppContext) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.anchor_prompts = false;
            view.smooth_reveal = false;
            view.streaming(true, cx);
            view.visible = false;
            view.patch(
                0,
                vec![
                    row("user", "Prompt"),
                    row("assistant", &"A paragraph.\n\n".repeat(35)),
                ],
                cx,
            );
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.patch(1, vec![row("assistant", "A shorter result.")], cx);
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    let before = window
        .update(cx, |view, _, _| {
            assert!(
                view.follow.offset < 0.,
                "shrinking content should preserve its painted position"
            );
            view.follow.offset
        })
        .unwrap();
    window
        .update(cx, |view, _, _| {
            view.follow.last_frame = Some(Instant::now() - std::time::Duration::from_millis(16));
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, cx| {
            assert!(view.follow.offset > before && view.follow.offset <= 0.);
            view.patch(2, vec![row("user", "Next prompt")], cx);
            assert_eq!(view.follow.offset, 0.);
            assert!(
                view.follow.measured.is_none(),
                "a sent prompt starts a fresh baseline"
            );
        })
        .unwrap();
}

// Patch with row entrances disabled to isolate follow motion from disclosures.
#[gpui::test]
fn ordinary_short_replies_fill_the_prompt_spacer_without_moving_the_chat(cx: &mut TestAppContext) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.smooth_reveal = false;
            view.streaming(true, cx);
            view.visible = false;
            view.patch(
                0,
                vec![row("user", "Prompt"), row("assistant", "Short reply.")],
                cx,
            );
            view.visible = true;
            view.prompt_rise = None;
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.patch(
                1,
                vec![row("assistant", "Short reply.\n\nAnother paragraph.")],
                cx,
            );
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            assert!(
                view.follow.offset.abs() < 0.25,
                "text should consume the spacer before the prompt moves"
            );
            assert!(view.follow.measured.as_ref().unwrap().footer > BOTTOM_INSET);
        })
        .unwrap();
}

#[test]
fn measurements_follow_surviving_rows_across_footer_changes_and_window_boundaries() {
    let measure = |rows: &[(usize, f32)]| FollowMeasure {
        viewport: size(px(900.), px(600.)),
        bottom: px(600.),
        window_size: size(px(900.), px(600.)),
        footer: 0.,
        slack: 0.,
        rows: rows
            .iter()
            .map(|(key, height)| ((*key, 0), *height))
            .collect(),
    };
    let old = measure(&[(0, 60.), (1, 28.), (2, 28.)]);
    assert_eq!(
        measure(&[(0, 60.), (1, 28.), (4, 28.)]).shift_from(&old),
        0.
    );
    assert_eq!(
        measure(&[(0, 60.), (5, 60.), (1, 28.), (2, 28.)]).shift_from(&old),
        60.
    );
    assert_eq!(
        measure(&[(1, 28.), (2, 28.), (3, 40.)]).shift_from(&old),
        40.
    );
    let mut resized = measure(&[(0, 80.), (1, 28.), (2, 28.)]);
    resized.viewport.width = px(800.);
    assert_eq!(resized.shift_from(&old), 0.);
    let mut taller = measure(&[(0, 80.), (1, 28.), (2, 28.)]);
    taller.viewport.height += px(36.);
    taller.bottom += px(36.);
    assert_eq!(taller.shift_from(&old), -16.);
    taller.window_size.height += px(36.);
    assert_eq!(taller.shift_from(&old), 0.);
    let mut shorter = measure(&[(0, 60.), (1, 28.), (2, 28.)]);
    shorter.viewport.height -= px(18.);
    shorter.bottom -= px(18.);
    assert_eq!(shorter.shift_from(&old), 18.);
    assert_eq!(
        measure(&[(6, 20.)]).shift_from(&old),
        0.,
        "disconnected history starts a fresh baseline"
    );
}

#[test]
fn the_follow_spring_never_overshoots_and_keeps_momentum_across_brief_pauses() {
    let mut follow = Follow::default();
    let mut previous = 940.;
    let mut offset = follow.step(previous, previous, 1., 1.);
    for _ in 0..240 {
        assert!(offset >= 0. && offset <= previous);
        previous = offset;
        offset = follow.step(offset, 0., 1., 1.);
    }
    assert!(offset < 0.25);
    let mut follow = Follow {
        velocity: 4.,
        growth: 2.,
        offset: 0.1,
        ..Default::default()
    };
    follow.settle(Instant::now());
    assert_eq!(follow.offset, 0.);
    assert_eq!((follow.velocity, follow.growth), (4., 2.));
}

#[gpui::test]
fn closing_work_above_the_answer_moves_it_one_way(cx: &mut TestAppContext) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            let mut source = vec![
                row("user", "Prompt"),
                row("assistant", &"Some text.\n\n".repeat(20)),
            ];
            source.extend((0..4).map(|ix| tool(ix, true)));
            source.push(row("assistant", "Still checking."));
            source.extend((4..8).map(|ix| tool(ix, true)));
            source.push(row("assistant", "Here is the answer."));
            view.patch(0, source, cx);
            view.work_folds.insert(1, true);
            view.rebuild(0, cx);
            view.streaming(true, cx);
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    render(window, cx);
    let answer = window
        .update(cx, |view, _, cx| {
            assert!(view.list.is_following_tail());
            // The work line's click handler.
            view.work_folds.insert(1, false);
            view.list.pause_following_tail();
            view.rebuild(1, cx);
            view.rows
                .iter()
                .rev()
                .find(|r| matches!(r.body, Body::Markdown(_)))
                .unwrap()
                .key
        })
        .unwrap();
    // The header stays under the pointer while the answer rises into the
    // space. Following again at the end must not ease that distance a second
    // time, overshooting and settling back down.
    let mut previous: Option<f32> = None;
    for _ in 0..24 {
        render(window, cx);
        let top = window
            .update(cx, |view, _, _| {
                let ix = view.rows.iter().position(|r| r.key == answer).unwrap();
                f32::from(view.list.bounds_for_item(ix).unwrap().top()) + view.follow.offset
            })
            .unwrap();
        if let Some(previous) = previous {
            assert!(
                top <= previous + 0.5,
                "the answer moved back down: {previous} -> {top}"
            );
        }
        previous = Some(top);
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
}

fn tool(ix: usize, done: bool) -> Row {
    row(
        "tool",
        &serde_json::to_string(&ToolCall {
            id: ix.to_string(),
            name: "Bash".into(),
            input: format!(r#"{{"command":"rg pattern-{ix} src"}}"#),
            output: if done {
                "match\n".repeat(6)
            } else {
                String::new()
            },
            state: if done {
                ToolState::Completed
            } else {
                ToolState::Running
            },
            panel: None,
            display: None,
        })
        .unwrap(),
    )
}

#[gpui::test]
fn the_line_being_read_never_moves_back_down_as_work_folds(cx: &mut TestAppContext) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.patch(
                0,
                vec![
                    row("user", "Earlier"),
                    row("assistant", &"Earlier answer paragraph.\n\n".repeat(30)),
                    row("user", "Fix the jitter"),
                ],
                cx,
            );
            view.streaming(true, cx);
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    let prose = |s: &str| row("assistant", s);
    let mut script: Vec<(u64, usize, Vec<Row>)> = Vec::new();
    // A turn streams prose, a group of tools, more prose and another group.
    // Each step is (delay in ms, source index, rows).
    let intro = "Looking at how the transcript scrolls when rows toggle and what React does there.";
    let words: Vec<&str> = intro.split(' ').collect();
    for n in (3..=words.len()).step_by(3) {
        script.push((60, 3, vec![prose(&words[..n].join(" "))]));
    }
    let mut at = 4;
    for ix in 0..3 {
        script.push((150, at, vec![tool(ix, false)]));
        script.push((200, at, vec![tool(ix, true)]));
        at += 1;
    }
    let mid = "Found the follow logic. Now checking how the list anchors while folds animate and whether the spring overshoots.";
    let words: Vec<&str> = mid.split(' ').collect();
    for n in (3..=words.len()).step_by(3) {
        script.push((60, at, vec![prose(&words[..n].join(" "))]));
    }
    at += 1;
    for ix in 3..6 {
        script.push((150, at, vec![tool(ix, false)]));
        script.push((200, at, vec![tool(ix, true)]));
        at += 1;
    }
    let fin = "Here is the final answer with a few sentences of explanation. ".repeat(4);
    let words: Vec<&str> = fin.split(' ').collect();
    for n in (4..=words.len()).step_by(4) {
        script.push((50, at, vec![prose(&words[..n].join(" "))]));
    }
    let mut t = 0u64;
    let mut events = script.into_iter().peekable();
    let mut next_at = 0u64;
    let mut prev: Option<((usize, usize), f32)> = None;
    while t < 9000 {
        if let Some((delay, _, _)) = events.peek() {
            if t >= next_at + delay {
                let (delay, start, rows) = events.next().unwrap();
                next_at += delay;
                window
                    .update(cx, |view, _, cx| view.patch(start, rows, cx))
                    .unwrap();
            }
        } else if t > next_at + 1500 {
            break;
        }
        render(window, cx);
        window
            .update(cx, |view, _, _| {
                // Each paragraph after a group folds the work above it while
                // the chat stays pinned to the end. Growth glides the newest
                // prose up; the fold must not drop it back down.
                let (ix, row) = view
                    .rows
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, r)| matches!(r.body, Body::Markdown(_) | Body::Plain(_)))
                    .unwrap();
                let top =
                    f32::from(view.list.bounds_for_item(ix).unwrap().top()) + view.follow.offset;
                if let Some((key, previous)) = prev
                    && key == row.key
                {
                    assert!(
                        top <= previous + 1.5,
                        "{key:?} moved back down at {t}ms: {previous} -> {top}"
                    );
                }
                prev = Some((row.key, top));
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(16));
        t += 16;
    }
}

#[gpui::test]
fn a_line_being_read_that_folds_into_the_work_line_keeps_the_reader_in_its_turn(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let turn = |i: usize| {
        vec![
            row("user", &format!("Question {i}")),
            tool(i, true),
            row(
                "assistant",
                &format!(
                    "Answer {i}.\n\n{}",
                    "A long paragraph of prose. ".repeat(40)
                ),
            ),
        ]
    };
    // The live turn's commentary is the line being read, with plenty below it,
    // so the chat is not following the tail.
    let live = || {
        let mut rows: Vec<Row> = (0..11).flat_map(turn).collect();
        rows.extend([
            row("user", "Question 11"),
            tool(100, true),
            row("assistant", "Checking."),
            tool(101, true),
            row("assistant", &"Checking one more thing.\n\n".repeat(40)),
        ]);
        rows
    };
    // The answer arrives: the turn's commentary and tool calls fold into its
    // work line, and the row being read has no successor with its key.
    let done = || {
        let mut rows = live();
        rows.push(row("assistant", &"Here is the answer.\n\n".repeat(40)));
        rows
    };
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.patch(0, live(), cx);
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            let ix = view.rows.iter().position(|r| r.key.0 >= 37).unwrap();
            view.list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(13.),
            });
        })
        .unwrap();
    render(window, cx);
    let (read, before) = window
        .update(cx, |view, _, _| {
            assert!(!view.list.is_following_tail());
            let ix = view.list.logical_scroll_top().item_ix;
            (
                view.rows[ix].key,
                f32::from(view.list.bounds_for_item(ix).unwrap().top()),
            )
        })
        .unwrap();
    window
        .update(cx, |view, _, cx| view.patch(0, done(), cx))
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            let ix = view.list.logical_scroll_top().item_ix;
            let top = view.rows[ix].clone();
            // The line stays when it survives; when it folded, its turn's
            // work line takes the reader, rather than the answer or the
            // previous turn's reply.
            let expected = if view.rows.iter().any(|r| r.key == read) {
                top.key == read
            } else {
                matches!(top.body, Body::WorkLine { .. }) && top.key.0 == 34
            };
            assert!(
                expected,
                "the reader moved from {read:?} to {:?} {}",
                top.key,
                view.source[top.key.0]
                    .text
                    .chars()
                    .take(20)
                    .collect::<String>()
            );
            let after = f32::from(view.list.bounds_for_item(ix).unwrap().top());
            assert!(
                (after - before).abs() <= 14.,
                "the content moved from {before} to {after}"
            );
        })
        .unwrap();
}

#[gpui::test]
fn a_trimmed_final_message_keeps_the_reader_on_its_own_prose_not_its_actions(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let chat_with = |paragraphs: usize| {
        vec![
            row("user", "Question 0"),
            row(
                "assistant",
                &"A long paragraph of prose.\n\n".repeat(paragraphs),
            ),
            row("user", "Question 1"),
            row("assistant", &"Later answer.\n\n".repeat(40)),
        ]
    };
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.patch(0, chat_with(40), cx);
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            let ix = view
                .rows
                .iter()
                .rposition(|r| r.key.0 == 1 && matches!(r.body, Body::Markdown(_)))
                .unwrap();
            view.list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: px(0.),
            });
        })
        .unwrap();
    render(window, cx);
    let read = window
        .update(cx, |view, _, _| {
            assert!(!view.list.is_following_tail());
            view.rows[view.list.logical_scroll_top().item_ix].key
        })
        .unwrap();
    // The patch drops the later prose of the message; its actions row keeps
    // the same source index.
    window
        .update(cx, |view, _, cx| view.patch(0, chat_with(10), cx))
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            assert!(!view.rows.iter().any(|r| r.key == read));
            assert!(
                view.rows
                    .iter()
                    .any(|r| r.key.0 == 1 && matches!(r.body, Body::Actions { .. }))
            );
            let top = &view.rows[view.list.logical_scroll_top().item_ix];
            assert!(
                top.key.0 == 1 && matches!(top.body, Body::Markdown(_)),
                "the reader moved from {read:?} to {:?}",
                top.key
            );
        })
        .unwrap();
}

#[gpui::test]
fn long_user_messages_clamp_to_four_lines_until_shown_in_full(cx: &mut TestAppContext) {
    let lines = |n: usize| {
        (1..=n)
            .map(|i| format!("Line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            // Use the storage writer's real line/byte chunking for the long
            // prompt; a single hand-built Row cannot exercise chunk expansion.
            let mut source = onicode_core::transcript::Transcript::default();
            for (role, text) in [
                ("user", "Short".to_owned()),
                ("assistant", "Ok.".to_owned()),
                ("user", lines(8)),
                ("assistant", "Ok.".to_owned()),
                ("user", lines(30)),
                ("assistant", "Ok.".to_owned()),
            ] {
                source.begin(role);
                source.append(&text);
            }
            view.patch(0, source.rows, cx);
            view.visible = true;
            view.list.scroll_to(ListOffset {
                item_ix: 0,
                offset_in_item: px(0.),
            });
        })
        .unwrap();
    render(window, cx);
    let height = |view: &TranscriptView, start: usize| -> (usize, f32) {
        let rows: Vec<_> = (0..view.rows.len())
            .filter(|&ix| {
                view.rows[ix].key.0 == start && matches!(view.rows[ix].body, Body::Plain(_))
            })
            .collect();
        let height = rows
            .iter()
            .map(|&ix| {
                f32::from(
                    view.list
                        .bounds_for_item(ix)
                        .map_or(px(0.), |b| b.size.height),
                )
            })
            .sum();
        (rows.len(), height)
    };
    let (short, clamped, chunked) = window
        .update(cx, |view, _, _| {
            (height(view, 0), height(view, 2), height(view, 4))
        })
        .unwrap();
    // Four 20px lines and the button under them; one chunk of a long message.
    assert_eq!(chunked.0, 1);
    assert_eq!(clamped.1, chunked.1);
    assert!(
        clamped.1 >= short.1 + 3. * 20. + 16.,
        "{short:?} {clamped:?}"
    );
    assert!(clamped.1 < short.1 + 5. * 20., "{short:?} {clamped:?}");
    window
        .update(cx, |view, _, cx| {
            view.toggle_prompt(2, cx);
            view.toggle_prompt(4, cx);
            assert!(
                view.motions.is_empty(),
                "Show more opens without a transition"
            );
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            let open = height(view, 2);
            assert!(open.1 >= clamped.1 + 4. * 20., "{clamped:?} {open:?}");
            assert!(height(view, 4).0 > 1);
        })
        .unwrap();
    window
        .update(cx, |view, _, cx| {
            view.toggle_prompt(4, cx);
            assert!(
                view.motions.is_empty(),
                "Show less closes without a transition"
            );
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| assert_eq!(height(view, 4), chunked))
        .unwrap();
}

#[gpui::test]
fn an_anchored_prompt_stays_put_while_new_prose_folds_the_work_above(cx: &mut TestAppContext) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.patch(
                0,
                vec![
                    row("user", "Earlier"),
                    row("assistant", &"Earlier answer paragraph.\n\n".repeat(30)),
                ],
                cx,
            );
            view.streaming(true, cx);
            view.visible = true;
            view.patch(2, vec![row("user", "Fix the jitter")], cx);
        })
        .unwrap();
    render(window, cx);
    let prose = |s: &str| row("assistant", s);
    let mut script: Vec<(u64, usize, Vec<Row>)> = Vec::new();
    let mut at = 3;
    for round in 0..3 {
        let text =
            format!("Round {round}: looking at how the transcript scrolls when rows toggle.");
        let words: Vec<String> = text.split(' ').map(String::from).collect();
        for n in (3..=words.len()).step_by(3) {
            script.push((60, at, vec![prose(&words[..n].join(" "))]));
        }
        at += 1;
        for ix in 0..3 {
            script.push((150, at, vec![tool(round * 10 + ix, false)]));
            script.push((200, at, vec![tool(round * 10 + ix, true)]));
            at += 1;
        }
    }
    let mut t = 0u64;
    let mut events = script.into_iter().peekable();
    let mut next_at = 0u64;
    let mut prev: Option<f32> = None;
    while t < 9000 {
        if let Some((delay, _, _)) = events.peek() {
            if t >= next_at + delay {
                let (delay, start, rows) = events.next().unwrap();
                next_at += delay;
                window
                    .update(cx, |view, _, cx| view.patch(start, rows, cx))
                    .unwrap();
            }
        } else if t > next_at + 1500 {
            break;
        }
        render(window, cx);
        window
            .update(cx, |view, _, _| {
                let ix = view.rows.iter().position(|r| r.key.0 == 2).unwrap();
                let top = view
                    .list
                    .bounds_for_item(ix)
                    .map(|b| f32::from(b.top()) + view.follow.offset);
                // The spacer below absorbs the fold, so the prompt never moves.
                if let (Some(top), Some(previous)) = (top, prev) {
                    assert!(
                        (top - previous).abs() <= 0.5,
                        "the prompt moved at {t}ms: {previous} -> {top}"
                    );
                }
                prev = top;
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(16));
        t += 16;
    }
}

#[gpui::test]
fn replacing_work_near_the_viewport_limit_keeps_the_prompt_anchored(cx: &mut TestAppContext) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.smooth_reveal = false;
            view.visible = false;
            view.patch(
                0,
                vec![
                    row("user", "Earlier"),
                    row("assistant", &"Earlier answer.\n\n".repeat(30)),
                ],
                cx,
            );
            view.streaming(true, cx);
            view.patch(2, vec![row("user", "Keep this prompt still")], cx);
            view.patch(
                3,
                vec![
                    row(
                        "assistant",
                        &"Checking the transcript layout.\n\n".repeat(6),
                    ),
                    tool(0, true),
                    tool(1, true),
                    tool(2, true),
                ],
                cx,
            );
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    let top = window
        .update(cx, |view, _, cx| {
            assert!(view.follow.measured.as_ref().unwrap().footer > BOTTOM_INSET);
            let ix = view.rows.iter().position(|r| r.key.0 == 2).unwrap();
            let top = f32::from(view.list.bounds_for_item(ix).unwrap().top());
            view.patch(
                7,
                vec![row(
                    "assistant",
                    &"Now checking the replacement.\n\n".repeat(6),
                )],
                cx,
            );
            top
        })
        .unwrap();
    for frame in 0..40 {
        render(window, cx);
        window
            .update(cx, |view, _, _| {
                let ix = view.rows.iter().position(|r| r.key.0 == 2).unwrap();
                let actual = view
                    .list
                    .bounds_for_item(ix)
                    .map(|b| f32::from(b.top()) + view.follow.offset);
                assert!(
                    actual.is_some_and(|actual| (actual - top).abs() < 0.5),
                    "prompt moved at frame {frame}: {top} -> {actual:?}, follow {}, footer {}",
                    view.follow.offset,
                    view.follow.measured.as_ref().unwrap().footer,
                );
            })
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    window
        .update(cx, |view, _, cx| {
            view.patch(
                7,
                vec![row("assistant", &"A growing reply.\n\n".repeat(80))],
                cx,
            );
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, cx| {
            assert!(view.list.is_following_tail());
            assert_eq!(view.follow.measured.as_ref().unwrap().footer, BOTTOM_INSET);
            assert!(
                view.follow.offset > 0.,
                "genuine overflow must still follow smoothly"
            );
            view.patch(8, vec![tool(3, true), tool(4, true), tool(5, true)], cx);
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, cx| {
            view.patch(
                11,
                vec![row(
                    "assistant",
                    "A short replacement after the long reply.",
                )],
                cx,
            );
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            let ix = view.rows.iter().position(|r| r.key.0 == 2).unwrap();
            assert!((f32::from(view.list.bounds_for_item(ix).unwrap().top()) - top).abs() < 0.5);
            assert_eq!(
                view.follow.offset, 0.,
                "a reply that fits must not inherit a scroll animation"
            );
        })
        .unwrap();
}

#[gpui::test]
fn empty_and_unrevealed_replacements_do_not_reopen_previous_work(cx: &mut TestAppContext) {
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.streaming(true, cx);
            view.patch(
                0,
                vec![
                    row("user", "Prompt"),
                    row("assistant", "First progress message."),
                    tool(0, true),
                    tool(1, true),
                    row("assistant", "Current progress message."),
                ],
                cx,
            );
            view.visible = true;
            assert_eq!(view.fold_targets[&1], Some(4));
            view.patch(5, vec![row("assistant", "")], cx);
            assert_eq!(
                view.fold_targets[&1],
                Some(4),
                "an empty placeholder must keep the current block"
            );
            assert!(
                !view
                    .rows
                    .iter()
                    .any(|r| matches!(r.body, Body::Tool { .. }))
            );
            view.patch(5, vec![row("assistant", "The next progress message.")], cx);
            assert_eq!(
                view.fold_targets[&1],
                Some(4),
                "paced text must be visible before it replaces the current block"
            );
            view.reveals.get_mut(&5).unwrap().settle(Instant::now());
            view.rebuild(5, cx);
            assert_eq!(view.fold_targets[&1], Some(5));
            assert!(
                !view.rows.iter().any(|r| r.key == (4, 16)),
                "the old prose must leave layout when its replacement appears"
            );
        })
        .unwrap();
}

fn evicted_turns() -> Vec<Row> {
    (0..12)
        .flat_map(|i| {
            [
                row("user", &format!("Question {i}")),
                tool(i, true),
                row(
                    "assistant",
                    &format!(
                        "Answer {i}.\n\n{}",
                        "A long paragraph of prose. ".repeat(40)
                    ),
                ),
            ]
        })
        .collect()
}

/// Evict an inactive transcript, then install the same history through
/// `install_prepared_history`, as a reload that does not go through the local
/// reopen path does, and report where the reader is afterwards.
fn reinstall_after_eviction(
    cx: &mut TestAppContext,
    reading: bool,
    grown: bool,
) -> ((String, usize, f32), bool, bool) {
    cx.update(|cx| cx.set_reduce_motion(true));
    let window = chat(cx);
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            view.install_prepared_history(PreparedHistory::new(evicted_turns()), cx);
            view.visible = true;
        })
        .unwrap();
    render(window, cx);
    let top = |view: &TranscriptView| {
        let at = view.list.logical_scroll_top();
        let row = &view.rows[at.item_ix];
        (
            view.source[row.key.0].text.clone(),
            row.key.1,
            f32::from(at.offset_in_item),
        )
    };
    window
        .update(cx, |view, _, _| {
            if reading {
                let ix = view.rows.iter().position(|r| r.key.0 == 15).unwrap();
                view.list.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: px(13.),
                });
            }
        })
        .unwrap();
    render(window, cx);
    let expected = window
        .update(cx, |view, _, _| {
            assert_eq!(view.list.is_following_tail(), !reading);
            top(view)
        })
        .unwrap();
    window
        .update(cx, |view, _, cx| {
            view.visible = false;
            assert!(view.suspend_history(cx));
            view.visible = true;
            let mut rows = evicted_turns();
            if grown {
                rows.push(row("assistant", "One more reply."));
            }
            view.install_prepared_history(PreparedHistory::new(rows), cx);
        })
        .unwrap();
    render(window, cx);
    window
        .update(cx, |view, _, _| {
            let after = top(view);
            let same = if reading {
                after == expected
            } else {
                view.list.is_following_tail()
            };
            (after, same, view.history_position.is_none())
        })
        .unwrap()
}

#[gpui::test]
fn a_reinstalled_history_returns_the_reader_to_where_they_left_it(cx: &mut TestAppContext) {
    let (after, same, consumed) = reinstall_after_eviction(cx, true, false);
    assert!(same, "the reader moved to {after:?}");
    assert!(consumed, "the suspended position is still pending");
}

#[gpui::test]
fn a_reinstalled_history_with_a_new_tail_row_keeps_the_reader_in_place(cx: &mut TestAppContext) {
    let (after, same, consumed) = reinstall_after_eviction(cx, true, true);
    assert!(same, "the reader moved to {after:?}");
    assert!(consumed, "the suspended position is still pending");
}

#[gpui::test]
fn a_reinstalled_history_keeps_following_the_tail(cx: &mut TestAppContext) {
    for grown in [false, true] {
        let (after, same, consumed) = reinstall_after_eviction(cx, false, grown);
        assert!(same, "the list stopped following the tail at {after:?}");
        assert!(consumed, "the suspended position is still pending");
    }
}
