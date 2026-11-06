//! Every protocol type survives JSON: serialized, read back and serialized again, it is the same
//! JSON value. Compared as values, so maps may come back in another order.
use std::path::PathBuf;

use onicode_protocol::accounts::Profile;
use onicode_protocol::agent::{
    AccessMode, Approval, Handoff, ProviderSwitch, Question, QuestionItem, QuestionOption,
    Settings, TurnInfo, TurnMetrics,
};
use onicode_protocol::context::Usage;
use onicode_protocol::engine::{
    EngineDetails, EngineInfo, Hello, StopEngine, UpdateEngine, UpdateStarted,
};
use onicode_protocol::follow_up::{Message, State};
use onicode_protocol::provider::{BackgroundTask, Kind, NativeCommand, NativeSubcommand};
use onicode_protocol::session::{
    Answer, Btw, BtwAnswer, BtwThread, BtwThreads, Configure, Decide, EditLastTurn, EditedTurn,
    Fork, KeepQuestionOpen, QueueChange, QueueEdit, ReadBtw, ReadSession, ReadTranscript,
    RecallLastTurn, RecalledTurn, Rewind, SaveBtw, SaveQueue, Send, SendNow, SessionEvent,
    SessionRecord, SessionState, Steer, SteerNow, Stop, StopBtw, SwitchProvider, TranscriptPage,
    TurnState, Verdict, WatchSession,
};
use onicode_protocol::sessions::{
    Folder, Groups, Orchestration, Reminder, Session, SummaryTask, TextModel, WorkItem,
};
use onicode_protocol::transcript::Row;
use onicode_protocol::usage::Limit;
use onicode_protocol::workspace::{
    Archive, ClaimInboxEvents, CreateNote, CreateSession, Created, Delete, DeleteNote,
    FinishAutomationRun, GenerateTitle, Lead, MarkSeen, NameWorktree, Named, NewSession, Pin,
    Preview, PreviewSession, Project, ReminderChange, Rename, SaveGroups, SaveOrchestration,
    SearchSessions, SessionDefaults, SessionSummary, SetReminder, SetWorkItem, WatchWorkspace,
    WorkspaceEvent, WorkspaceState,
};
use onicode_protocol::{
    Cursor, EngineId, Item, MessageId, OpId, Outcome, Rejection, Routed, SessionId, TerminalId,
};
use proptest::collection::{hash_map, vec};
use proptest::option;
use proptest::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

fn round_trips<T: Serialize + DeserializeOwned>(value: &T) -> Result<(), TestCaseError> {
    let first = serde_json::to_value(value).map_err(|e| TestCaseError::fail(e.to_string()))?;
    let text = serde_json::to_string(value).map_err(|e| TestCaseError::fail(e.to_string()))?;
    let back: T = serde_json::from_str(&text).map_err(|e| TestCaseError::fail(e.to_string()))?;
    let second = serde_json::to_value(&back).map_err(|e| TestCaseError::fail(e.to_string()))?;
    prop_assert_eq!(second, first, "{}", text);
    Ok(())
}

fn text() -> impl Strategy<Value = String> {
    ".{0,12}"
}

fn paths() -> impl Strategy<Value = Vec<PathBuf>> {
    vec(text().prop_map(PathBuf::from), 0..3)
}

fn session_id() -> impl Strategy<Value = SessionId> {
    any::<u128>().prop_map(|n| SessionId(Uuid::from_u128(n)))
}

fn message_id() -> impl Strategy<Value = MessageId> {
    any::<u128>().prop_map(|n| MessageId(Uuid::from_u128(n)))
}

fn op_id() -> impl Strategy<Value = OpId> {
    any::<u128>().prop_map(|n| OpId(Uuid::from_u128(n)))
}

fn terminal_id() -> impl Strategy<Value = TerminalId> {
    any::<u128>().prop_map(|n| TerminalId(Uuid::from_u128(n)))
}

fn engine_id() -> impl Strategy<Value = EngineId> {
    text().prop_map(EngineId)
}

fn cursor() -> impl Strategy<Value = Cursor> {
    (any::<u64>(), any::<u64>()).prop_map(|(epoch, seq)| Cursor { epoch, seq })
}

fn rejection() -> impl Strategy<Value = Rejection> {
    prop_oneof![
        Just(Rejection::AlreadyResolved),
        Just(Rejection::StaleRev),
        Just(Rejection::Gone),
        text().prop_map(Rejection::Denied),
    ]
}

fn outcome() -> impl Strategy<Value = Outcome> {
    prop_oneof![
        Just(Outcome::Applied),
        rejection().prop_map(Outcome::Rejected)
    ]
}

fn row() -> impl Strategy<Value = Row> {
    (
        text(),
        text(),
        any::<bool>(),
        option::of(any::<u64>()),
        option::of(any::<u128>()),
    )
        .prop_map(|(role, text, first, sent_at, id)| Row {
            role,
            text,
            first,
            sent_at,
            id: id.map(Uuid::from_u128),
        })
}

fn approval() -> impl Strategy<Value = Approval> {
    (text(), text(), text(), any::<bool>()).prop_map(|(id, title, detail, can_remember)| Approval {
        id,
        title,
        detail,
        can_remember,
    })
}

fn question() -> impl Strategy<Value = Question> {
    let choice = (text(), text(), text()).prop_map(|(id, label, description)| QuestionOption {
        id,
        label,
        description,
    });
    let item = (
        text(),
        text(),
        any::<bool>(),
        any::<bool>(),
        vec(choice, 0..3),
    )
        .prop_map(
            |(id, prompt, multiple, allow_custom, options)| QuestionItem {
                id,
                prompt,
                multiple,
                allow_custom,
                options,
            },
        );
    (text(), text(), vec(item, 0..3), option::of(any::<u64>())).prop_map(
        |(id, title, items, auto_resolve_at)| Question {
            id,
            title,
            items,
            auto_resolve_at,
        },
    )
}

fn metrics() -> impl Strategy<Value = TurnMetrics> {
    (
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        option::of(0.0..100.0_f64),
    )
        .prop_map(
            |(input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, hit)| {
                TurnMetrics {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                    cache_hit_percent: hit,
                }
            },
        )
}

fn turn_info() -> impl Strategy<Value = TurnInfo> {
    (
        option::of(any::<usize>()),
        option::of(text()),
        text(),
        text(),
        option::of(text()),
        any::<u64>(),
        any::<bool>(),
        option::of(any::<u64>()),
        option::of(any::<u64>()),
        option::of(metrics()),
    )
        .prop_map(
            |(
                joined_turn,
                provider_turn_id,
                provider,
                model,
                model_id,
                duration_ms,
                duration_known,
                started_at,
                completed_at,
                metrics,
            )| TurnInfo {
                joined_turn,
                provider_turn_id,
                provider,
                model,
                model_id,
                duration_ms,
                duration_known,
                started_at,
                completed_at,
                metrics,
            },
        )
}

fn handoff() -> impl Strategy<Value = Handoff> {
    (text(), text()).prop_map(|(from, brief)| Handoff { from, brief })
}

fn limit() -> impl Strategy<Value = Limit> {
    (option::of(any::<i64>()), any::<bool>()).prop_map(|(resets_at, resume_at_reset)| Limit {
        resets_at,
        resume_at_reset,
    })
}

fn usage() -> impl Strategy<Value = Usage> {
    (any::<u64>(), option::of(any::<u64>())).prop_map(|(used, window)| Usage { used, window })
}

fn access() -> impl Strategy<Value = AccessMode> {
    prop_oneof![
        Just(AccessMode::ReadOnly),
        Just(AccessMode::Supervised),
        Just(AccessMode::AutoAcceptEdits),
        Just(AccessMode::Auto),
        Just(AccessMode::FullAccess),
    ]
}

fn model_settings() -> impl Strategy<Value = std::collections::HashMap<String, String>> {
    hash_map(text(), text(), 0..3)
}

fn provider_switch() -> impl Strategy<Value = ProviderSwitch> {
    (
        (text(), text(), text(), option::of(text()), model_settings()),
        (
            option::of(text()),
            any::<bool>(),
            option::of(text()),
            option::of(any::<bool>()),
            any::<bool>(),
            text(),
            option::of(text()),
        ),
    )
        .prop_map(
            |(
                (from, model_label, cwd, model, model_settings),
                (effort, fast, service_tier, thinking, skip_outgoing_recap, account, provider_id),
            )| {
                let mut switch = ProviderSwitch {
                    from,
                    model_label,
                    cwd,
                    model,
                    model_settings,
                    effort,
                    fast,
                    service_tier,
                    thinking,
                    skip_outgoing_recap,
                    provider_id,
                    ..ProviderSwitch::default()
                };
                switch.account.id = account;
                switch
            },
        )
}

fn settings() -> impl Strategy<Value = Settings> {
    (
        (
            text(),
            option::of(limit()),
            any::<u64>(),
            option::of(text()),
            option::of(text()),
            access(),
            any::<bool>(),
            option::of(text()),
            option::of(any::<bool>()),
            model_settings(),
        ),
        (
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
            option::of(text()),
            option::of(text()),
            any::<bool>(),
            option::of(handoff()),
            option::of(provider_switch()),
            hash_map(0_usize..64, turn_info(), 0..3),
        ),
    )
        .prop_map(
            |(
                (
                    account,
                    usage_limit,
                    revision,
                    model,
                    effort,
                    access,
                    fast,
                    service_tier,
                    thinking,
                    model_settings,
                ),
                (
                    plan,
                    operator,
                    operator_enabled,
                    orchestrator,
                    new_worktree,
                    worktree_base,
                    worktree_cwd,
                    worktree_removed,
                    handoff,
                    pending_provider_switch,
                    turns,
                ),
            )| {
                let mut settings = Settings {
                    usage_limit,
                    revision,
                    model,
                    effort,
                    access,
                    fast,
                    service_tier,
                    thinking,
                    model_settings,
                    plan,
                    operator,
                    operator_enabled,
                    orchestrator,
                    new_worktree,
                    worktree_base,
                    worktree_cwd,
                    worktree_removed,
                    handoff,
                    pending_provider_switch,
                    turns,
                    ..Settings::default()
                };
                settings.account.id = account;
                settings
            },
        )
}

fn message() -> impl Strategy<Value = Message> {
    (
        text(),
        text(),
        option::of(any::<u64>()),
        option::of(text()),
        paths(),
        option::of(text()),
        any::<usize>(),
        option::of(text()),
        any::<bool>(),
    )
        .prop_map(
            |(id, text, sent_at, prepared_prompt, attachments, mode, at, error, steer)| Message {
                id,
                text,
                sent_at,
                prepared_prompt,
                attachments,
                mode,
                at,
                error,
                steer,
            },
        )
}

fn queue() -> impl Strategy<Value = State> {
    (
        any::<u64>(),
        vec(message(), 0..3),
        any::<bool>(),
        option::of(message()),
    )
        .prop_map(|(revision, messages, paused, draft)| State {
            revision,
            messages,
            paused,
            draft,
        })
}

fn background_task() -> impl Strategy<Value = BackgroundTask> {
    (
        text(),
        text(),
        any::<bool>(),
        option::of(text()),
        any::<u64>(),
    )
        .prop_map(
            |(id, description, agent, command, started_at)| BackgroundTask {
                id,
                description,
                agent,
                command,
                started_at,
            },
        )
}

fn native_command() -> impl Strategy<Value = NativeCommand> {
    let subcommand =
        (text(), option::of(text()), option::of(text())).prop_map(|(name, description, usage)| {
            NativeSubcommand {
                name,
                description,
                usage,
            }
        });
    (
        text(),
        text(),
        text(),
        proptest::sample::select(Kind::ALL.to_vec()),
        option::of(text()),
        vec(text(), 0..3),
        option::of(text()),
        vec(subcommand, 0..3),
    )
        .prop_map(
            |(name, description, invocation, source, origin, aliases, input_hint, subcommands)| {
                NativeCommand {
                    name,
                    description,
                    invocation,
                    source,
                    origin,
                    aliases,
                    input_hint,
                    subcommands,
                }
            },
        )
}

fn turn_state() -> impl Strategy<Value = TurnState> {
    (option::of(text()), any::<bool>(), any::<bool>()).prop_map(
        |(provider_turn_id, ready, finished)| TurnState {
            provider_turn_id,
            ready,
            finished,
        },
    )
}

fn session_event() -> impl Strategy<Value = SessionEvent> {
    prop_oneof![
        (any::<usize>(), vec(row(), 0..3))
            .prop_map(|(start, rows)| SessionEvent::Rows { start, rows }),
        (any::<usize>(), text()).prop_map(|(row, text)| SessionEvent::Append { row, text }),
        (text(), option::of(turn_state()))
            .prop_map(|(status, turn)| SessionEvent::Status { status, turn }),
        approval().prop_map(SessionEvent::Approval),
        text().prop_map(SessionEvent::ApprovalResolved),
        question().prop_map(SessionEvent::Question),
        text().prop_map(SessionEvent::QuestionResolved),
        (any::<u64>(), queue()).prop_map(|(rev, state)| SessionEvent::Queue { rev, state }),
        settings().prop_map(|settings| SessionEvent::Settings(Box::new(settings))),
        text().prop_map(SessionEvent::Title),
        text().prop_map(SessionEvent::ProviderId),
        text().prop_map(SessionEvent::Model),
        text().prop_map(SessionEvent::Worktree),
        usage().prop_map(SessionEvent::Context),
        metrics().prop_map(SessionEvent::Metrics),
        option::of(limit()).prop_map(SessionEvent::UsageLimit),
        vec(background_task(), 0..3).prop_map(SessionEvent::Background),
        vec(native_command(), 0..3).prop_map(SessionEvent::Commands),
        option::of(handoff()).prop_map(SessionEvent::Handoff),
        (any::<usize>(), turn_info())
            .prop_map(|(turn, info)| SessionEvent::TurnFinished { turn, info }),
        (op_id(), outcome()).prop_map(|(op, outcome)| SessionEvent::Receipt { op, outcome }),
        (text(), text()).prop_map(|(thread, text)| SessionEvent::BtwText { thread, text }),
    ]
}

fn session_state() -> impl Strategy<Value = SessionState> {
    (
        (
            any::<usize>(),
            vec(row(), 0..3),
            text(),
            option::of(turn_state()),
            vec(approval(), 0..2),
            vec(question(), 0..2),
            any::<u64>(),
            queue(),
            settings(),
        ),
        (
            text(),
            option::of(text()),
            option::of(text()),
            option::of(text()),
            option::of(usage()),
            option::of(limit()),
            vec(background_task(), 0..2),
            vec(native_command(), 0..2),
            option::of(handoff()),
        ),
    )
        .prop_map(
            |(
                (first_row, rows, status, turn, approvals, questions, queue_rev, queue, settings),
                (
                    title,
                    provider_id,
                    model,
                    worktree,
                    context,
                    usage_limit,
                    background,
                    commands,
                    handoff,
                ),
            )| SessionState {
                first_row,
                rows,
                status,
                turn,
                approvals,
                questions,
                queue_rev,
                queue,
                settings,
                title,
                provider_id,
                model,
                worktree,
                context,
                usage_limit,
                background,
                commands,
                handoff,
            },
        )
}

fn item() -> impl Strategy<Value = Item<SessionState, SessionEvent>> {
    prop_oneof![
        (cursor(), session_state()).prop_map(|(at, state)| Item::Snapshot { at, state }),
        (any::<u64>(), any::<u64>(), vec(session_event(), 0..4)).prop_map(
            |(first, last, events)| Item::Events {
                first,
                last,
                events
            }
        ),
        Just(Item::Resync),
    ]
}

fn verdict() -> impl Strategy<Value = Verdict> {
    prop_oneof![
        Just(Verdict::Allow),
        Just(Verdict::AllowForSession),
        Just(Verdict::Deny),
    ]
}

fn queue_change() -> impl Strategy<Value = QueueChange> {
    prop_oneof![
        (text(), paths(), any::<bool>()).prop_map(|(text, attachments, steer)| {
            QueueChange::Add {
                text,
                attachments,
                steer,
            }
        }),
        (text(), paths()).prop_map(|(text, attachments)| QueueChange::Text { text, attachments }),
        any::<usize>().prop_map(|to| QueueChange::Move { to }),
        Just(QueueChange::Remove),
    ]
}

fn send() -> impl Strategy<Value = Send> {
    (session_id(), message_id(), text(), paths()).prop_map(
        |(session, message, text, attachments)| Send {
            session,
            message,
            text,
            attachments,
        },
    )
}

proptest! {
    #[test]
    fn ids(
        session in session_id(),
        message in message_id(),
        op in op_id(),
        terminal in terminal_id(),
        engine in engine_id(),
    ) {
        round_trips(&session)?;
        round_trips(&message)?;
        round_trips(&op)?;
        round_trips(&terminal)?;
        round_trips(&engine)?;
    }

    #[test]
    fn cursors(value in cursor()) {
        round_trips(&value)?;
    }

    #[test]
    fn outcomes(value in outcome(), rejection in rejection()) {
        round_trips(&value)?;
        round_trips(&rejection)?;
    }

    #[test]
    fn routed(engine in option::of(engine_id()), op in option::of(op_id()), body in send()) {
        round_trips(&Routed { engine, op, body })?;
    }

    #[test]
    fn hello(protocol in any::<u32>(), version in text(), capabilities in vec(text(), 0..3)) {
        round_trips(&Hello { protocol, version, capabilities })?;
    }

    #[test]
    fn engine_info(engine in engine_id(), name in text(), version in text(), protocol in any::<u32>()) {
        round_trips(&EngineInfo {})?;
        round_trips(&EngineDetails { engine, name, version, protocol })?;
    }

    #[test]
    fn update_engine(version in option::of(text()), started in any::<bool>(), detail in text()) {
        round_trips(&UpdateEngine { version })?;
        round_trips(&UpdateStarted { started, detail })?;
        round_trips(&StopEngine {})?;
    }

    #[test]
    fn session_events(value in session_event()) {
        round_trips(&value)?;
    }

    #[test]
    fn session_states(value in session_state()) {
        round_trips(&value)?;
    }

    #[test]
    fn items(value in item()) {
        round_trips(&value)?;
    }

    #[test]
    fn watch_session(session in session_id(), since in option::of(cursor()), window_turns in any::<usize>()) {
        round_trips(&WatchSession { session, since, window_turns })?;
    }

    #[test]
    fn prompts(value in send(), turn in any::<usize>()) {
        let Send { session, message, text, attachments } = value.clone();
        round_trips(&value)?;
        round_trips(&Steer { session, message, text: text.clone(), attachments: attachments.clone() })?;
        round_trips(&EditLastTurn { session, attachments, model: Some(text) })?;
        round_trips(&Stop { session })?;
        round_trips(&Rewind { session, turn })?;
    }

    #[test]
    fn decisions(
        session in session_id(),
        id in text(),
        answer in verdict(),
        answers in vec(vec(text(), 0..3), 0..3),
    ) {
        round_trips(&Decide { session, approval: id.clone(), answer })?;
        round_trips(&Answer { session, question: id.clone(), answers })?;
        round_trips(&KeepQuestionOpen { session, question: id })?;
    }

    #[test]
    fn queue_edits(session in session_id(), id in message_id(), base_rev in any::<u64>(), edit in queue_change()) {
        round_trips(&QueueEdit { session, id, base_rev, edit })?;
        round_trips(&SendNow { session, id })?;
        round_trips(&SteerNow { session, id })?;
    }

    #[test]
    fn configure(session in session_id(), provider in text(), settings in settings()) {
        round_trips(&Configure { session, provider, settings })?;
    }

    #[test]
    fn forks_and_switches(
        session in session_id(),
        into in session_id(),
        turn in any::<usize>(),
        provider in text(),
        account in text(),
        model in option::of(text()),
    ) {
        round_trips(&Fork { session, turn, into })?;
        round_trips(&SwitchProvider { session, provider, account, model })?;
    }

    #[test]
    fn transcripts(
        session in session_id(),
        before_turn in option::of(any::<usize>()),
        turns in any::<usize>(),
        first_row in any::<usize>(),
        rows in vec(row(), 0..4),
        more in any::<bool>(),
    ) {
        round_trips(&ReadTranscript { session, before_turn, turns })?;
        round_trips(&TranscriptPage { first_row, rows, more })?;
    }
}

fn btw_thread() -> impl Strategy<Value = BtwThread> {
    (
        (
            text(),
            any::<usize>(),
            text(),
            text(),
            option::of(text()),
            text(),
        ),
        (
            text(),
            option::of(text()),
            model_settings(),
            vec(row(), 0..3),
            text(),
            text(),
            option::of(text()),
        ),
    )
        .prop_map(
            |(
                (id, source_turn, snapshot, provider, model, cwd),
                (account, effort, model_settings, rows, draft, status, error),
            )| BtwThread {
                id,
                source_turn,
                snapshot,
                provider,
                model,
                cwd,
                account: Profile {
                    id: account,
                    root: None,
                },
                effort,
                model_settings,
                rows,
                draft,
                status,
                error,
            },
        )
}

proptest! {
    #[test]
    fn session_records(
        session in session_id(),
        settings in option::of(settings()),
        context in option::of(usage()),
        queue in queue(),
        edit_recovery in option::of(text()),
    ) {
        round_trips(&ReadSession { session })?;
        round_trips(&SaveQueue { session, queue: queue.clone() })?;
        round_trips(&SessionRecord {
            settings: settings.map(Box::new),
            context,
            queue,
            edit_recovery,
        })?;
    }

    #[test]
    fn last_turn_edits(
        session in session_id(),
        turn in option::of(text()),
        start in any::<usize>(),
        names in (text(), text(), text(), text(), text()),
        attachments in paths(),
        rows in vec(row(), 0..3),
        outcome in outcome(),
        provider_id in option::of(text()),
    ) {
        let (prompt, provider, bound, account_id, cwd) = names;
        round_trips(&RecallLastTurn { session, turn: turn.clone() })?;
        round_trips(&RecalledTurn {
            start,
            prompt,
            attachments: attachments.clone(),
            provider,
            provider_id: bound,
            account_id,
            cwd,
            rows,
        })?;
        round_trips(&EditLastTurn { session, attachments, model: turn })?;
        round_trips(&EditedTurn { outcome, provider_id })?;
    }

    #[test]
    fn btw_threads(
        session in session_id(),
        thread in btw_thread(),
        threads in vec(btw_thread(), 0..3),
        revision in any::<u64>(),
        outcome in outcome(),
        text in option::of(text()),
    ) {
        round_trips(&ReadBtw { session })?;
        round_trips(&SaveBtw { session, threads: BtwThreads { revision, threads } })?;
        round_trips(&StopBtw { session, thread: thread.id.clone() })?;
        round_trips(&Btw { session, thread })?;
        round_trips(&BtwAnswer { outcome, text })?;
    }
}

fn work_item() -> impl Strategy<Value = WorkItem> {
    (text(), text(), any::<u64>(), text()).prop_map(|(kind, repo, number, url)| WorkItem {
        kind,
        repo,
        number,
        url,
    })
}

fn stored_session() -> impl Strategy<Value = Session> {
    (
        (any::<i64>(), text(), text(), text(), option::of(text())),
        (
            any::<bool>(),
            any::<u64>(),
            any::<bool>(),
            any::<bool>(),
            option::of(work_item()),
        ),
    )
        .prop_map(
            |(
                (id, title, provider, cwd, provider_id),
                (has_messages, updated_at, archived, pinned, work_item),
            )| Session {
                id,
                title,
                provider,
                cwd,
                provider_id,
                has_messages,
                updated_at,
                archived,
                pinned,
                work_item,
            },
        )
}

fn summary() -> impl Strategy<Value = SessionSummary> {
    (
        (session_id(), stored_session(), text()),
        option::of(text()),
        any::<bool>(),
        option::of(any::<i64>()),
        option::of(text()),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |((id, session, key), worktree, draft, started_by, automation, busy, unread)| {
                SessionSummary {
                    id,
                    session,
                    key,
                    worktree,
                    draft,
                    started_by,
                    automation,
                    busy,
                    unread,
                }
            },
        )
}

fn groups() -> impl Strategy<Value = Groups> {
    (
        vec(
            (
                text(),
                text(),
                vec(any::<i64>(), 0..3),
                any::<bool>(),
                option::of(any::<usize>()),
                option::of(text()),
            ),
            0..3,
        ),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(folders, pinned_collapsed, reminders_collapsed)| Groups {
            folders: folders
                .into_iter()
                .map(
                    |(id, name, session_ids, collapsed, color_index, custom_color)| Folder {
                        id,
                        name,
                        session_ids,
                        collapsed,
                        color_index,
                        custom_color,
                    },
                )
                .collect(),
            pinned_collapsed,
            reminders_collapsed,
        })
}

fn project() -> impl Strategy<Value = Project> {
    (text(), groups()).prop_map(|(path, groups)| Project { path, groups })
}

fn reminder() -> impl Strategy<Value = Reminder> {
    (any::<i64>(), any::<u64>(), option::of(any::<u64>())).prop_map(
        |(session_id, due_at, fired_at)| Reminder {
            session_id,
            due_at,
            fired_at,
        },
    )
}

fn lead() -> impl Strategy<Value = Lead> {
    (
        any::<i64>(),
        vec(
            (
                option::of(any::<i64>()),
                text(),
                text(),
                option::of(text()),
                text(),
            ),
            0..3,
        ),
        any::<bool>(),
    )
        .prop_map(|(lead, tasks, live)| Lead {
            lead,
            summary: Orchestration {
                tasks: tasks
                    .into_iter()
                    .map(|(session, title, provider, model, status)| SummaryTask {
                        session,
                        title,
                        provider,
                        model,
                        status,
                    })
                    .collect(),
                live,
            },
        })
}

fn workspace_event() -> impl Strategy<Value = WorkspaceEvent> {
    prop_oneof![
        summary().prop_map(|summary| WorkspaceEvent::Session(Box::new(summary))),
        any::<i64>().prop_map(WorkspaceEvent::Removed),
        project().prop_map(WorkspaceEvent::Project),
        vec(reminder(), 0..3).prop_map(WorkspaceEvent::Reminders),
        lead().prop_map(WorkspaceEvent::Orchestration),
        any::<usize>().prop_map(WorkspaceEvent::Notes),
        text().prop_map(WorkspaceEvent::AutomationTaken),
        (op_id(), outcome()).prop_map(|(op, outcome)| WorkspaceEvent::Receipt { op, outcome }),
    ]
}

fn workspace_state() -> impl Strategy<Value = WorkspaceState> {
    (
        vec(summary(), 0..3),
        vec(project(), 0..3),
        vec(reminder(), 0..3),
        vec(lead(), 0..2),
        any::<usize>(),
    )
        .prop_map(
            |(sessions, projects, reminders, orchestrations, notes)| WorkspaceState {
                sessions,
                projects,
                reminders,
                orchestrations,
                notes,
                automations: Vec::new(),
            },
        )
}

fn reminder_change() -> impl Strategy<Value = ReminderChange> {
    prop_oneof![
        any::<u64>().prop_map(ReminderChange::At),
        (any::<u64>(), any::<u64>())
            .prop_map(|(expected, due_at)| ReminderChange::Snooze { expected, due_at }),
        option::of(any::<u64>()).prop_map(|expected| ReminderChange::Clear { expected }),
    ]
}

fn new_session() -> impl Strategy<Value = NewSession> {
    prop_oneof![
        Just(NewSession::Bare),
        settings().prop_map(|settings| NewSession::Settings(Box::new(settings))),
        (
            option::of(text()),
            text(),
            access(),
            option::of(text()),
            option::of(text())
        )
            .prop_map(|(model, account, access, effort, worktree_cwd)| {
                NewSession::Defaults(SessionDefaults {
                    model,
                    account: Profile {
                        id: account,
                        root: None,
                    },
                    access,
                    effort,
                    worktree_cwd,
                })
            }),
    ]
}

fn text_model() -> impl Strategy<Value = TextModel> {
    (
        proptest::sample::select(Kind::ALL.to_vec()),
        option::of(text()),
        text(),
    )
        .prop_map(|(kind, model, account)| TextModel {
            kind,
            model,
            account: Profile {
                id: account,
                root: None,
            },
        })
}

proptest! {
    #[test]
    fn workspace_events(value in workspace_event()) {
        round_trips(&value)?;
    }

    #[test]
    fn workspace_states(value in workspace_state(), since in option::of(cursor())) {
        round_trips(&value)?;
        round_trips(&WatchWorkspace { since })?;
    }

    #[test]
    fn workspace_items(at in cursor(), state in workspace_state(), events in vec(workspace_event(), 0..3)) {
        round_trips(&Item::<WorkspaceState, WorkspaceEvent>::Snapshot { at, state })?;
        round_trips(&Item::<WorkspaceState, WorkspaceEvent>::Events { first: 1, last: 3, events })?;
    }

    #[test]
    fn creations(
        session in session_id(),
        provider in text(),
        cwd in text(),
        title in option::of(text()),
        start in new_session(),
        started_by in option::of(any::<i64>()),
        outcome in outcome(),
        created in option::of(summary()),
        settings in option::of(settings()),
    ) {
        round_trips(&CreateSession { session, provider, cwd, title, start, started_by })?;
        round_trips(&Created {
            outcome,
            session: created.map(Box::new),
            settings: settings.map(Box::new),
        })?;
    }

    #[test]
    fn session_marks(
        session in any::<i64>(),
        sessions in vec(any::<i64>(), 0..3),
        flag in any::<bool>(),
        title in text(),
        expected in option::of(text()),
        item in option::of(work_item()),
        change in reminder_change(),
    ) {
        round_trips(&Rename { session, title, expected })?;
        round_trips(&Archive { session, archived: flag })?;
        round_trips(&Pin { sessions: sessions.clone(), pinned: flag })?;
        round_trips(&Delete { session })?;
        round_trips(&SetWorkItem { session, item, if_unlinked: flag })?;
        round_trips(&SetReminder { sessions, change })?;
        round_trips(&MarkSeen { session })?;
    }

    #[test]
    fn workspace_chores(
        project in text(),
        groups in groups(),
        lead in lead(),
        session in any::<i64>(),
        message in text(),
        model in text_model(),
        branch in option::of(text()),
        run in text(),
    ) {
        round_trips(&SaveGroups { project: project.clone(), groups })?;
        round_trips(&SaveOrchestration { lead: lead.lead, summary: lead.summary })?;
        round_trips(&SearchSessions { query: message.clone() })?;
        round_trips(&PreviewSession { session })?;
        round_trips(&Preview { model: branch.clone(), worktree: Some(project.clone()) })?;
        round_trips(&GenerateTitle {
            session,
            message: message.clone(),
            placeholder: project.clone(),
            seed: message.clone(),
            cwd: project.clone(),
            model: model.clone(),
        })?;
        round_trips(&NameWorktree { session, root: project.clone(), cwd: project.clone(), message, model })?;
        round_trips(&Named { branch })?;
        round_trips(&CreateNote { cwd: Some(project) })?;
        round_trips(&DeleteNote { id: run.clone() })?;
        round_trips(&FinishAutomationRun { run, status: "succeeded".into(), error: None })?;
        round_trips(&ClaimInboxEvents::default())?;
    }
}
