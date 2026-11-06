//! RegistryDoc unit tests: the typed API, replication by version vector, the merge join, and
//! owner-only fields.

use onicode_proto::{HarnessId, SandboxLevel, SessionStatus};
use proptest::prelude::*;

use super::*;

/// Every doc sends every other the rows it lacks, as the `registry` streams would, until nothing
/// moves.
fn sync(docs: &mut [&mut RegistryDoc]) {
    for _ in 0..2 {
        for from in 0..docs.len() {
            for to in 0..docs.len() {
                if from != to {
                    let rows = docs[from].rows_since(docs[to].vv());
                    let vv = docs[from].vv().clone();
                    docs[to].merge_remote(&rows, Some(&vv));
                }
            }
        }
    }
}

fn ts(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).unwrap_or(DateTime::UNIX_EPOCH)
}

fn hlc(ms: i64) -> String {
    encode_hlc(ms, 0, "dev-a")
}

#[test]
fn hlc_orders_lexicographically() {
    assert!(hlc(2) > hlc(1));
    assert!(encode_hlc(1, 2, "a") > encode_hlc(1, 1, "a"));
    assert!(encode_hlc(1, 1, "b") > encode_hlc(1, 1, "a"));
    assert!(hlc(10_000) > hlc(999));
}

#[test]
fn hlc_clock_is_monotonic_across_regressions() {
    let mut clock = HlcClock::default();
    let a = clock.next(1_000, "d");
    let b = clock.next(500, "d"); // wall clock went backwards
    let c = clock.next(1_000, "d"); // and stalled
    assert!(b > a);
    assert!(c > b);
}

fn device(id: &str, name: &str) -> Device {
    Device {
        id: id.into(),
        name: name.into(),
        platform: "linux".into(),
        last_seen_at: Some(ts(1_000)),
        created_at: Some(ts(500)),
        version: Some("0.1.0".into()),
        cursor_sdk_version: Some("1.0.31".into()),
        capabilities: Vec::new(),
    }
}

fn chat(id: &str, device_id: &str) -> Chat {
    Chat {
        id: id.into(),
        device_id: device_id.into(),
        title: Some("First chat".into()),
        archived: false,
        cwd: Some("/tmp/repo".into()),
        branch: Some("main".into()),
        checkout_id: None,
        source_context: None,
        config: Some(ChatConfig {
            harness: HarnessId::Mock,
            model: Some("mock-1".into()),
            reasoning: None,
            model_options: Default::default(),
            sandbox: SandboxLevel::WorkspaceWrite,
            access: Default::default(),
            plan: false,
        }),
        last_message_preview: None,
        last_message_at: None,
        created_at: ts(2_000),
        harness_session_id: None,
        harness_session_cwd: None,
        harness_session_harness: None,
        parent_chat_id: Some("parent-chat".into()),
        child_kind: None,
        space_id: None,
        last_seen_at: None,
        room_gen: None,
    }
}

fn space(id: &str, device_id: &str, path: &str) -> Space {
    Space {
        id: id.into(),
        device_id: device_id.into(),
        path: path.into(),
        name: None,
        git_detected: false,
        git_checked_at: None,
        checkout_id: None,
        repository_id: None,
        github_repo: None,
        icon: None,
        created_at: ts(1_500),
    }
}

fn session(chat_id: &str, device_id: &str, status: SessionStatus) -> Session {
    Session {
        last_completed_turn: None,
        last_turn_outcome: None,
        terminal_activity: None,
        running_subagents: 0,
        background_tasks: Vec::new(),
        chat_id: chat_id.into(),
        device_id: device_id.into(),
        status,
        started_at: Some(ts(3_000)),
        updated_at: ts(3_500),
    }
}

#[test]
fn background_processes_replicate_and_clear() {
    let mut host = RegistryDoc::new("host");
    let mut viewer = RegistryDoc::new("viewer");
    let mut row = session("chat", "host", SessionStatus::Idle);
    row.background_tasks = vec![onicode_proto::BackgroundTask {
        id: "bgsh".into(),
        command: "npm run dev".into(),
        description: Some("Start the dev server".into()),
        started_at: ts(3_100),
    }];
    host.upsert_session(&row).unwrap();
    sync(&mut [&mut host, &mut viewer]);
    assert_eq!(viewer.read_sessions().unwrap(), vec![row.clone()]);

    // Every process ended: an empty list replaces the old one.
    row.background_tasks.clear();
    row.updated_at = ts(3_600);
    host.upsert_session(&row).unwrap();
    sync(&mut [&mut host, &mut viewer]);
    assert_eq!(viewer.read_sessions().unwrap(), vec![row]);
}

#[test]
fn rows_round_trip_and_upsert_refreshes() {
    let mut doc = RegistryDoc::new("dev-a");
    let mut device = device("dev-a", "laptop");
    device.capabilities = vec![onicode_proto::capabilities::MESSAGE_QUEUE_V1.into()];
    doc.upsert_device(&device).unwrap();
    doc.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
    doc.upsert_session(&session("chat-1", "dev-a", SessionStatus::Working))
        .unwrap();

    let state = doc.read_all().unwrap();
    assert_eq!(state.devices, vec![device]);
    assert_eq!(state.chats, vec![chat("chat-1", "dev-a")]);
    assert_eq!(
        state.sessions,
        vec![session("chat-1", "dev-a", SessionStatus::Working)]
    );

    let mut updated = chat("chat-1", "dev-a");
    updated.title = None;
    updated.last_message_preview = Some("hello".into());
    updated.last_message_at = Some(ts(9_000));
    doc.upsert_chat(&updated).unwrap();
    let chats = doc.read_chats().unwrap();
    assert_eq!(chats.len(), 1);
    assert_eq!(chats[0].title, None);
    assert_eq!(chats[0].last_message_preview.as_deref(), Some("hello"));
}

#[test]
fn field_mutators_round_trip() {
    let mut ws = RegistryDoc::new("dev-a");
    ws.upsert_device(&device("dev-a", "laptop")).unwrap();
    ws.upsert_chat(&chat("chat-1", "dev-a")).unwrap();

    assert!(ws.rename_chat("chat-1", "Renamed").unwrap());
    assert!(ws.set_chat_archived("chat-1", true).unwrap());
    assert!(
        ws.set_chat_last_message("chat-1", "preview text", ts(5_000))
            .unwrap()
    );
    assert!(ws.rename_device("dev-a", "workstation").unwrap());
    assert!(ws.set_device_last_seen("dev-a", ts(6_000)).unwrap());
    assert!(!ws.rename_chat("nope", "x").unwrap());
    assert!(!ws.set_chat_archived("nope", true).unwrap());
    assert!(!ws.rename_device("nope", "x").unwrap());

    let chat = ws.chat("chat-1").unwrap().unwrap();
    assert_eq!(chat.title.as_deref(), Some("Renamed"));
    assert!(chat.archived);
    assert_eq!(chat.last_message_preview.as_deref(), Some("preview text"));
    assert_eq!(chat.last_message_at, Some(ts(5_000)));
    let dev = &ws.read_devices().unwrap()[0];
    assert_eq!(dev.cursor_sdk_version.as_deref(), Some("1.0.31"));
    assert_eq!(dev.name, "workstation");
    assert_eq!(dev.last_seen_at, Some(ts(6_000)));
}

#[test]
fn delete_chat_tombstones_row_and_session() {
    let mut ws = RegistryDoc::new("dev-a");
    ws.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
    ws.upsert_session(&session("chat-1", "dev-a", SessionStatus::Idle))
        .unwrap();
    assert!(ws.delete_chat("chat-1").unwrap());
    assert!(ws.read_chats().unwrap().is_empty());
    assert!(ws.read_sessions().unwrap().is_empty());
    assert!(!ws.delete_chat("chat-1").unwrap());
}

#[test]
fn a_space_icon_is_set_whole_survives_upserts_and_clears() {
    use onicode_proto::SpaceIcon;
    let mut ws = RegistryDoc::new("dev-a");
    ws.upsert_space(&space("sp-1", "dev-a", "/home/u/project"))
        .unwrap();
    assert_eq!(ws.space("sp-1").unwrap().unwrap().icon, None);

    let emoji = SpaceIcon {
        emoji: Some("🚀".into()),
        image: None,
        hue: Some("violet".into()),
    };
    assert!(ws.set_space_icon("sp-1", Some(&emoji)).unwrap());
    assert_eq!(ws.space("sp-1").unwrap().unwrap().icon, Some(emoji));

    // Another kind replaces it whole: no emoji left beside the image.
    let image = SpaceIcon {
        emoji: None,
        image: Some("https://edge.test/auth/avatars/ab".into()),
        hue: None,
    };
    assert!(ws.set_space_icon("sp-1", Some(&image)).unwrap());
    assert_eq!(ws.space("sp-1").unwrap().unwrap().icon, Some(image.clone()));

    // An upsert of the row without one (a re-seed, a create) leaves it.
    ws.upsert_space(&space("sp-1", "dev-a", "/home/u/project"))
        .unwrap();
    assert_eq!(ws.space("sp-1").unwrap().unwrap().icon, Some(image));

    assert!(ws.set_space_icon("sp-1", None).unwrap());
    assert_eq!(ws.space("sp-1").unwrap().unwrap().icon, None);
    assert!(!ws.set_space_icon("nope", None).unwrap());
}

#[test]
fn a_malformed_space_icon_is_no_icon_not_a_lost_row() {
    let mut ws = RegistryDoc::new("dev-a");
    ws.upsert_space(&space("sp-1", "dev-a", "/home/u/project"))
        .unwrap();
    // (As a newer or broken writer might leave it.)
    ws.write(
        KIND_SPACES,
        "sp-1",
        OpKind::Update,
        fields([("icon", json!("🚀"))]),
    );
    let row = ws.space("sp-1").unwrap().expect("the row stays");
    assert_eq!(row.icon, None);
    ws.write(
        KIND_SPACES,
        "sp-1",
        OpKind::Update,
        fields([("icon", json!({ "image": "file:///etc/passwd" }))]),
    );
    assert_eq!(ws.space("sp-1").unwrap().unwrap().icon, None);
}

#[test]
fn spaces_round_trip_and_mutate() {
    let mut ws = RegistryDoc::new("dev-a");
    ws.upsert_space(&space("sp-1", "dev-a", "/home/u/project"))
        .unwrap();
    let row = ws.space("sp-1").unwrap().expect("row exists");
    assert_eq!(row.display_name(), "project");
    assert!(!row.git_detected);

    assert!(ws.rename_space("sp-1", Some("My Project")).unwrap());
    assert_eq!(
        ws.space("sp-1").unwrap().unwrap().display_name(),
        "My Project"
    );
    assert!(ws.rename_space("sp-1", None).unwrap());
    assert_eq!(ws.space("sp-1").unwrap().unwrap().display_name(), "project");

    assert!(
        ws.set_space_git(
            "sp-1",
            true,
            Some("checkout-abc"),
            Some("github.com/owner/project"),
            Some("acme/app"),
            ts(4_000),
        )
        .unwrap()
    );
    let row = ws.space("sp-1").unwrap().unwrap();
    assert!(row.git_detected);
    assert_eq!(row.checkout_id.as_deref(), Some("checkout-abc"));
    assert_eq!(
        row.repository_id.as_deref(),
        Some("github.com/owner/project")
    );
    assert_eq!(row.git_checked_at, Some(ts(4_000)));

    assert!(!ws.rename_space("nope", Some("x")).unwrap());
    assert!(
        !ws.set_space_git("nope", true, None, None, None, ts(1))
            .unwrap()
    );
}

#[test]
fn chat_seen_is_monotonic() {
    let mut ws = RegistryDoc::new("dev-a");
    ws.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
    assert!(ws.set_chat_seen("chat-1", ts(5_000)).unwrap());
    assert_eq!(
        ws.chat("chat-1").unwrap().unwrap().last_seen_at,
        Some(ts(5_000))
    );
    // Older stamps are ignored without a write.
    let before = ws.generation();
    assert!(ws.set_chat_seen("chat-1", ts(4_000)).unwrap());
    assert_eq!(ws.generation(), before);
    assert_eq!(
        ws.chat("chat-1").unwrap().unwrap().last_seen_at,
        Some(ts(5_000))
    );
    assert!(!ws.set_chat_seen("nope", ts(1)).unwrap());
}

#[test]
fn two_docs_converge_by_exchanging_rows() {
    let mut a = RegistryDoc::new("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    a.upsert_device(&device("dev-a", "laptop")).unwrap();
    a.upsert_chat(&chat("chat-a", "dev-a")).unwrap();
    a.upsert_session(&session("chat-a", "dev-a", SessionStatus::Working))
        .unwrap();
    b.upsert_device(&device("dev-b", "vps")).unwrap();
    b.upsert_chat(&chat("chat-b", "dev-b")).unwrap();
    sync(&mut [&mut a, &mut b]);

    let sa = a.read_all().unwrap();
    let sb = b.read_all().unwrap();
    assert_eq!(sa, sb);
    assert_eq!(
        sa.devices.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        vec!["dev-a", "dev-b"]
    );
    assert_eq!(
        sa.chats.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        vec!["chat-a", "chat-b"]
    );
    assert_eq!(sa.sessions.len(), 1);
    assert_eq!(a.vv(), b.vv());

    // Concurrent same-field rename settles identically on both.
    a.rename_chat("chat-a", "from a").unwrap();
    b.rename_chat("chat-a", "from b").unwrap();
    sync(&mut [&mut a, &mut b]);
    let title_a = a.chat("chat-a").unwrap().unwrap().title;
    let title_b = b.chat("chat-a").unwrap().unwrap().title;
    assert_eq!(title_a, title_b);
    assert!(matches!(
        title_a.as_deref(),
        Some("from a") | Some("from b")
    ));
}

/// The command plane outruns the registry channel: the host's
/// claim-on-first-command lands with NEWER clocks than the viewer's earlier
/// `createChat`. The claim writes only the fields it knows, so the viewer's
/// `config`/`title` must still land on merge (a full-row claim's `Null`
/// writes deleted them — the missing-harness-icon clobber).
#[test]
fn late_create_chat_config_survives_a_prior_claim() {
    let mut viewer = RegistryDoc::new("dev-viewer");
    let mut host = RegistryDoc::new("dev-a");

    // Viewer writes the real row first (older clocks)…
    viewer.upsert_chat(&chat("chat-race", "dev-a")).unwrap();
    // …then the host claims the same chat before that row reaches it. (Real
    // claims trail by whole seconds; the sleep keeps the HLCs out of the
    // same-millisecond device-id tiebreak.)
    std::thread::sleep(std::time::Duration::from_millis(2));
    host.claim_chat("chat-race", Some("/tmp/repo"), Some("space-1"), ts(9_000));
    sync(&mut [&mut viewer, &mut host]);

    for doc in [&viewer, &host] {
        let merged = doc.chat("chat-race").unwrap().expect("merged row");
        // Claimed fields (newer clocks) stand…
        assert_eq!(merged.device_id, "dev-a");
        assert_eq!(merged.space_id.as_deref(), Some("space-1"));
        // …and the fields the claim never wrote come from the createChat.
        assert_eq!(merged.config, chat("chat-race", "dev-a").config);
        assert_eq!(merged.title.as_deref(), Some("First chat"));
    }
}

#[test]
fn delete_space_cascades_and_converges() {
    let mut a = RegistryDoc::new("dev-a");
    a.upsert_space(&space("sp-1", "dev-a", "/tmp/one")).unwrap();
    a.upsert_space(&space("sp-2", "dev-a", "/tmp/two")).unwrap();
    let mut in_space = chat("chat-1", "dev-a");
    in_space.space_id = Some("sp-1".into());
    let mut other = chat("chat-2", "dev-a");
    other.space_id = Some("sp-2".into());
    a.upsert_chat(&in_space).unwrap();
    a.upsert_chat(&other).unwrap();
    a.upsert_session(&session("chat-1", "dev-a", SessionStatus::Working))
        .unwrap();
    let mut b = RegistryDoc::new("dev-b");
    sync(&mut [&mut a, &mut b]);
    assert_eq!(a.read_all().unwrap(), b.read_all().unwrap());

    let deleted = a.delete_space("sp-1").unwrap();
    assert!(deleted.existed);
    assert_eq!(deleted.chat_ids, vec!["chat-1".to_string()]);
    // The cascade applies locally before any peer sees it.
    assert_eq!(a.read_spaces().unwrap().len(), 1);
    sync(&mut [&mut a, &mut b]);

    for ws in [&a, &b] {
        let state = ws.read_all().unwrap();
        assert_eq!(
            state
                .spaces
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sp-2"]
        );
        assert_eq!(
            state
                .chats
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            vec!["chat-2"]
        );
        assert!(state.sessions.is_empty());
    }
    let again = b.delete_space("sp-1").unwrap();
    assert!(!again.existed);
    assert!(again.chat_ids.is_empty());
}

fn pin_sessions(doc: &mut RegistryDoc, ids: &[&str]) {
    for id in ids {
        if doc.chat(id).unwrap().is_none() {
            doc.upsert_chat(&chat(id, "desktop")).unwrap();
        }
        doc.change_sidebar_pin(&onicode_proto::SidebarPinChange::Pin {
            session_id: (*id).into(),
            after: None,
            before: None,
        })
        .unwrap();
    }
}

#[test]
fn sidebar_preferences_preserve_unknown_empty_and_ordered_states() {
    let mut doc = RegistryDoc::new("dev-a");
    assert_eq!(doc.sidebar_preferences(), None);
    assert!(!doc.reconcile_sidebar_pins(false).unwrap());
    assert!(doc.reconcile_sidebar_pins(true).unwrap());
    assert!(
        doc.sidebar_preferences()
            .unwrap()
            .pinned_session_ids
            .is_empty()
    );
    pin_sessions(&mut doc, &["chat-b", "chat-a"]);
    assert_eq!(
        doc.sidebar_preferences().unwrap().pinned_session_ids,
        ["chat-b", "chat-a"]
    );
}

#[test]
fn sidebar_cleanup_waits_for_authority_and_keeps_archived_pins() {
    let mut doc = RegistryDoc::new("desktop");
    pin_sessions(&mut doc, &["live", "archived", "deleted"]);
    let mut archived = chat("archived", "desktop");
    archived.archived = true;
    doc.upsert_chat(&archived).unwrap();
    doc.delete_chat("deleted").unwrap();
    assert!(!doc.reconcile_sidebar_pins(false).unwrap());
    assert_eq!(
        doc.sidebar_preferences().unwrap().pinned_session_ids.len(),
        3
    );
    assert!(doc.reconcile_sidebar_pins(true).unwrap());
    assert_eq!(
        doc.sidebar_preferences().unwrap().pinned_session_ids,
        ["live", "archived"]
    );
    assert!(!doc.reconcile_sidebar_pins(true).unwrap());
}

#[test]
fn sidebar_ignores_old_whole_list_preferences() {
    let mut doc = RegistryDoc::new("desktop");
    doc.upsert_chat(&chat("old", "desktop")).unwrap();
    doc.write(
        KIND_PREFERENCES,
        "sidebar-v1",
        OpKind::Upsert,
        fields([("pinnedSessionIds", json!(["old"]))]),
    );
    assert!(doc.sidebar_preferences().is_none());
    doc.reconcile_sidebar_pins(true).unwrap();
    assert!(
        doc.sidebar_preferences()
            .unwrap()
            .pinned_session_ids
            .is_empty()
    );
    pin_sessions(&mut doc, &["new"]);
    assert_eq!(
        doc.sidebar_preferences().unwrap().pinned_session_ids,
        ["new"]
    );
}

#[test]
fn sidebar_rejects_invalid_or_missing_sessions_before_writing() {
    use onicode_proto::SidebarPinChange;
    let mut doc = RegistryDoc::new("dev-a");
    for id in ["", "invalid id", "missing"] {
        assert!(
            doc.change_sidebar_pin(&SidebarPinChange::Pin {
                session_id: id.into(),
                after: None,
                before: None
            })
            .is_err()
        );
        assert!(doc.sidebar_preferences().is_none());
    }
}

#[test]
fn sidebar_concurrent_additions_keep_overflow_and_allow_removal_and_moves() {
    use onicode_proto::SidebarPinChange;
    let mut a = RegistryDoc::new("a");
    let mut b = RegistryDoc::new("b");
    let ids: Vec<_> = (0..199).map(|i| format!("pin-{i}")).collect();
    for id in ids
        .iter()
        .map(String::as_str)
        .chain(["extra-a", "extra-b", "extra-c"])
    {
        a.upsert_chat(&chat(id, "a")).unwrap();
    }
    pin_sessions(&mut a, &ids.iter().map(String::as_str).collect::<Vec<_>>());
    sync(&mut [&mut a, &mut b]);
    for (doc, id) in [(&mut a, "extra-a"), (&mut b, "extra-b")] {
        doc.change_sidebar_pin(&SidebarPinChange::Pin {
            session_id: id.into(),
            after: None,
            before: None,
        })
        .unwrap();
    }
    sync(&mut [&mut a, &mut b]);
    assert_eq!(
        a.sidebar_preferences().unwrap().pinned_session_ids.len(),
        201
    );
    assert!(!a.reconcile_sidebar_pins(true).unwrap());
    assert!(
        a.change_sidebar_pin(&SidebarPinChange::Pin {
            session_id: "extra-c".into(),
            after: None,
            before: None
        })
        .is_err()
    );
    a.change_sidebar_pin(&SidebarPinChange::Move {
        session_id: "extra-a".into(),
        after: None,
        before: Some("pin-0".into()),
    })
    .unwrap();
    a.change_sidebar_pin(&SidebarPinChange::Unpin {
        session_id: "extra-b".into(),
    })
    .unwrap();
    sync(&mut [&mut a, &mut b]);
    assert_eq!(
        a.sidebar_preferences().unwrap().pinned_session_ids.len(),
        200
    );
    assert_eq!(a.sidebar_preferences(), b.sidebar_preferences());
}

#[test]
fn completion_marker_replicates_and_survives_next_turn() {
    let mut source = RegistryDoc::new("dev-a");
    let mut viewer = RegistryDoc::new("dev-b");
    let mut row = session("chat-1", "dev-a", SessionStatus::Idle);
    row.last_completed_turn = Some("turn-one".into());
    source.upsert_session(&row).unwrap();
    sync(&mut [&mut source, &mut viewer]);
    assert_eq!(viewer.read_sessions().unwrap(), vec![row.clone()]);
    row.status = SessionStatus::Working;
    source.upsert_session(&row).unwrap();
    sync(&mut [&mut source, &mut viewer]);
    assert_eq!(viewer.read_sessions().unwrap(), vec![row]);
}

fn section_change(doc: &mut RegistryDoc, change: onicode_proto::SidebarSectionChange) {
    doc.change_sidebar_pin(&onicode_proto::SidebarPinChange::Section { change })
        .unwrap();
}

#[test]
fn sidebar_sections_keep_placed_order_across_devices() {
    use onicode_proto::SidebarSectionChange::*;
    let mut a = RegistryDoc::new("a");
    let mut b = RegistryDoc::new("b");
    for id in ["one", "two", "three", "legacy"] {
        a.upsert_chat(&chat(id, "desktop")).unwrap();
    }
    section_change(
        &mut a,
        Create {
            id: "focus".into(),
            name: "Focus".into(),
        },
    );
    let place = |doc: &mut RegistryDoc, id: &str, after: Option<&str>, before: Option<&str>| {
        section_change(
            doc,
            Assign {
                session_id: id.into(),
                section_id: Some("focus".into()),
                after: after.map(Into::into),
                before: before.map(Into::into),
            },
        )
    };
    let order = |doc: &RegistryDoc| {
        doc.sidebar_preferences().unwrap().sections[0]
            .session_ids
            .clone()
    };
    // Anchorless assigns append, so membership no longer sorts by id.
    place(&mut a, "two", None, None);
    place(&mut a, "one", None, None);
    assert_eq!(order(&a), ["two", "one"]);
    // An engine that predates ordering writes a keyless member: it follows
    // the placed ones until the next placement keys it in place.
    a.write_sidebar_location("legacy", "focus");
    assert_eq!(order(&a), ["two", "one", "legacy"]);
    place(&mut a, "three", Some("two"), Some("one"));
    assert_eq!(order(&a), ["two", "three", "one", "legacy"]);
    sync(&mut [&mut a, &mut b]);
    assert_eq!(order(&b), ["two", "three", "one", "legacy"]);
    // A reorder on the other device converges back.
    place(&mut b, "legacy", None, Some("two"));
    sync(&mut [&mut b, &mut a]);
    assert_eq!(order(&a), ["legacy", "two", "three", "one"]);
    assert_eq!(order(&a), order(&b));
    // A keyless move into another section never inherits the old key.
    section_change(
        &mut a,
        Create {
            id: "later".into(),
            name: "Later".into(),
        },
    );
    a.write_sidebar_location("one", "later");
    assert_eq!(order(&a), ["legacy", "two", "three"]);
}

#[test]
fn sidebar_sections_reorder_and_converge_across_devices() {
    use onicode_proto::SidebarSectionChange::*;
    let mut a = RegistryDoc::new("a");
    let mut b = RegistryDoc::new("b");
    let create = |doc: &mut RegistryDoc, id: &str| {
        section_change(
            doc,
            Create {
                id: id.into(),
                name: id.into(),
            },
        )
    };
    let moved = |doc: &mut RegistryDoc, id: &str, after: Option<&str>, before: Option<&str>| {
        section_change(
            doc,
            Move {
                id: id.into(),
                after: after.map(Into::into),
                before: before.map(Into::into),
            },
        )
    };
    let order = |doc: &RegistryDoc| {
        doc.sidebar_preferences()
            .unwrap()
            .sections
            .into_iter()
            .map(|s| s.id)
            .collect::<Vec<_>>()
    };
    for id in ["one", "two", "three"] {
        create(&mut a, id);
    }
    // Unmoved sections keep creation order.
    assert_eq!(order(&a), ["one", "two", "three"]);
    moved(&mut a, "three", None, Some("one"));
    assert_eq!(order(&a), ["three", "one", "two"]);
    sync(&mut [&mut a, &mut b]);
    assert_eq!(order(&b), ["three", "one", "two"]);
    // A section created later, keyless as an older engine writes it, appends.
    create(&mut b, "four");
    assert_eq!(order(&b), ["three", "one", "two", "four"]);
    // Concurrent moves of different sections both survive.
    moved(&mut a, "one", Some("two"), None);
    moved(&mut b, "four", None, Some("three"));
    sync(&mut [&mut a, &mut b]);
    assert_eq!(order(&a), order(&b));
    assert_eq!(order(&a), ["four", "three", "two", "one"]);
    // Concurrent moves of the same section: the later one wins everywhere.
    moved(&mut a, "two", None, Some("four"));
    std::thread::sleep(std::time::Duration::from_millis(5));
    moved(&mut b, "two", Some("one"), None);
    sync(&mut [&mut a, &mut b]);
    assert_eq!(order(&a), order(&b));
    assert_eq!(order(&a), ["four", "three", "one", "two"]);
    // Moving a deleted section is a no-op, and its anchors may be gone.
    section_change(&mut a, Delete { id: "three".into() });
    moved(&mut a, "three", None, Some("four"));
    moved(&mut a, "one", Some("three"), None);
    assert_eq!(order(&a), ["four", "two", "one"]);
    sync(&mut [&mut a, &mut b]);
    // Keying a new section leaves the keyed ones alone, so a concurrent
    // move on another device survives a later move that keys it.
    moved(&mut a, "one", None, Some("four"));
    std::thread::sleep(std::time::Duration::from_millis(5));
    create(&mut b, "five");
    moved(&mut b, "two", Some("five"), None);
    sync(&mut [&mut a, &mut b]);
    sync(&mut [&mut a, &mut b]);
    assert_eq!(order(&a), order(&b));
    assert_eq!(order(&a), ["one", "four", "five", "two"]);
}

#[test]
fn sidebar_sections_sync_metadata_membership_and_delete_without_deleting_sessions() {
    use onicode_proto::SidebarSectionChange::*;
    let mut a = RegistryDoc::new("a");
    let mut b = RegistryDoc::new("b");
    pin_sessions(&mut a, &["session"]);
    section_change(
        &mut a,
        Create {
            id: "focus".into(),
            name: "Focus".into(),
        },
    );
    section_change(
        &mut a,
        Assign {
            session_id: "session".into(),
            section_id: Some("focus".into()),
            after: None,
            before: None,
        },
    );
    sync(&mut [&mut a, &mut b]);
    let prefs = b.sidebar_preferences().unwrap();
    assert!(prefs.pinned_session_ids.is_empty());
    assert_eq!(prefs.sections[0].session_ids, ["session"]);
    section_change(
        &mut b,
        Rename {
            id: "focus".into(),
            name: "Today".into(),
        },
    );
    section_change(
        &mut a,
        Collapse {
            id: "focus".into(),
            collapsed: true,
        },
    );
    sync(&mut [&mut b, &mut a]);
    let prefs = a.sidebar_preferences().unwrap();
    assert_eq!(prefs, b.sidebar_preferences().unwrap());
    assert_eq!(prefs.sections[0].name, "Today");
    assert!(prefs.sections[0].collapsed);
    let mut archived = a.chat("session").unwrap().unwrap();
    archived.archived = true;
    a.upsert_chat(&archived).unwrap();
    sync(&mut [&mut a, &mut b]);
    assert_eq!(
        b.sidebar_preferences().unwrap().sections[0].session_ids,
        ["session"]
    );
    section_change(&mut b, Delete { id: "focus".into() });
    sync(&mut [&mut a, &mut b]);
    assert!(a.sidebar_preferences().unwrap().sections.is_empty());
    assert!(a.chat("session").unwrap().is_some());
}

#[test]
fn sidebar_sections_concurrent_pin_and_section_moves_converge() {
    use onicode_proto::SidebarPinChange;
    use onicode_proto::SidebarSectionChange::*;
    for reverse in [false, true] {
        let mut a = RegistryDoc::new("a");
        let mut b = RegistryDoc::new("b");
        pin_sessions(&mut a, &["session"]);
        for id in ["focus", "later"] {
            section_change(
                &mut a,
                Create {
                    id: id.into(),
                    name: id.into(),
                },
            );
        }
        sync(&mut [&mut a, &mut b]);
        section_change(
            &mut a,
            Assign {
                session_id: "session".into(),
                section_id: Some("focus".into()),
                after: None,
                before: None,
            },
        );
        section_change(
            &mut b,
            Assign {
                session_id: "session".into(),
                section_id: Some("later".into()),
                after: None,
                before: None,
            },
        );
        if reverse {
            sync(&mut [&mut b, &mut a]);
        } else {
            sync(&mut [&mut a, &mut b]);
        }
        let prefs = a.sidebar_preferences().unwrap();
        assert_eq!(prefs, b.sidebar_preferences().unwrap());
        assert_eq!(
            prefs
                .sections
                .iter()
                .map(|s| s.session_ids.len())
                .sum::<usize>(),
            1
        );
        a.change_sidebar_pin(&SidebarPinChange::Pin {
            session_id: "session".into(),
            after: None,
            before: None,
        })
        .unwrap();
        section_change(
            &mut b,
            Assign {
                session_id: "session".into(),
                section_id: Some("focus".into()),
                after: None,
                before: None,
            },
        );
        if reverse {
            sync(&mut [&mut b, &mut a]);
        } else {
            sync(&mut [&mut a, &mut b]);
        }
        let prefs = a.sidebar_preferences().unwrap();
        assert_eq!(prefs, b.sidebar_preferences().unwrap());
        assert_eq!(
            prefs.pinned_session_ids.len()
                + prefs
                    .sections
                    .iter()
                    .map(|s| s.session_ids.len())
                    .sum::<usize>(),
            1
        );
    }
}

#[test]
fn sidebar_sections_migration_is_replay_safe_and_preserves_newer_remote_intent() {
    use onicode_proto::SidebarSectionChange::*;
    let mut a = RegistryDoc::new("a");
    let mut b = RegistryDoc::new("b");
    a.upsert_chat(&chat("session", "a")).unwrap();
    let import = Import {
        sections: vec![onicode_proto::SidebarSection {
            id: "focus".into(),
            name: "Focus".into(),
            collapsed: true,
            session_ids: vec!["session".into()],
        }],
    };
    section_change(&mut a, import.clone());
    // Offline outbox survives restart before it reaches the other device.
    let mut a = RegistryDoc::from_bytes(&a.to_bytes().unwrap(), "a").unwrap();
    sync(&mut [&mut a, &mut b]);
    assert!(b.sidebar_preferences().unwrap().sections[0].collapsed);
    section_change(
        &mut b,
        Assign {
            session_id: "session".into(),
            section_id: None,
            after: None,
            before: None,
        },
    );
    section_change(
        &mut b,
        Rename {
            id: "focus".into(),
            name: "Renamed".into(),
        },
    );
    sync(&mut [&mut a, &mut b]);
    section_change(&mut a, import.clone());
    assert_eq!(a.sidebar_preferences().unwrap().sections[0].name, "Renamed");
    assert!(
        a.sidebar_preferences().unwrap().sections[0]
            .session_ids
            .is_empty()
    );
    section_change(&mut b, Delete { id: "focus".into() });
    sync(&mut [&mut a, &mut b]);
    section_change(&mut a, import);
    assert!(a.sidebar_preferences().unwrap().sections.is_empty());
    assert!(a.chat("session").unwrap().is_some());
}

#[test]
fn sidebar_sections_honor_pin_toggles_from_older_clients() {
    use onicode_proto::SidebarSectionChange::*;
    let mut doc = RegistryDoc::new("a");
    pin_sessions(&mut doc, &["session"]);
    section_change(
        &mut doc,
        Create {
            id: "focus".into(),
            name: "Focus".into(),
        },
    );
    section_change(
        &mut doc,
        Assign {
            session_id: "session".into(),
            section_id: Some("focus".into()),
            after: None,
            before: None,
        },
    );
    assert!(
        doc.sidebar_preferences()
            .unwrap()
            .pinned_session_ids
            .is_empty()
    );
    // An older client still only writes the existing per-pin fields.
    doc.write(
        KIND_SIDEBAR_PINS,
        "session",
        OpKind::Upsert,
        fields([("pinned", json!(true))]),
    );
    let prefs = doc.sidebar_preferences().unwrap();
    assert_eq!(prefs.pinned_session_ids, ["session"]);
    assert!(prefs.sections[0].session_ids.is_empty());
    doc.write(
        KIND_SIDEBAR_PINS,
        "session",
        OpKind::Upsert,
        fields([("pinned", json!(false))]),
    );
    let prefs = doc.sidebar_preferences().unwrap();
    assert!(prefs.pinned_session_ids.is_empty());
    assert!(prefs.sections[0].session_ids.is_empty());
    section_change(
        &mut doc,
        Assign {
            session_id: "session".into(),
            section_id: Some("focus".into()),
            after: None,
            before: None,
        },
    );
    assert_eq!(
        doc.sidebar_preferences().unwrap().sections[0].session_ids,
        ["session"]
    );
}

#[test]
fn side_chat_origin_syncs_and_survives_updates_and_restart() {
    let mut a = RegistryDoc::new("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    let mut side = chat("side", "dev-a");
    side.parent_chat_id = Some("main".into());
    side.room_gen = Some(2);
    a.upsert_chat(&side).unwrap();
    sync(&mut [&mut a, &mut b]);
    assert_eq!(b.chat("side").unwrap(), Some(side.clone()));
    b.rename_chat("side", "Investigate caching").unwrap();
    sync(&mut [&mut a, &mut b]);
    assert_eq!(
        a.chat("side").unwrap().unwrap().parent_chat_id.as_deref(),
        Some("main")
    );
    let persisted = a.to_bytes().unwrap();
    let restored = RegistryDoc::from_bytes(&persisted, "dev-a").unwrap();
    assert_eq!(
        restored
            .chat("side")
            .unwrap()
            .unwrap()
            .parent_chat_id
            .as_deref(),
        Some("main")
    );
}

// ── replication ─────────────────────────────────────────────────────────────

fn all_rows(doc: &RegistryDoc) -> Vec<RegistryRow> {
    let mut rows: Vec<RegistryRow> = doc
        .rows
        .values()
        .flat_map(|m| m.values().cloned())
        .collect();
    rows.sort_by(|a, b| (&a.kind, &a.id).cmp(&(&b.kind, &b.id)));
    rows
}

#[test]
fn rows_since_sends_only_what_the_peer_lacks() {
    let mut a = RegistryDoc::new("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    a.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
    a.upsert_chat(&chat("chat-2", "dev-a")).unwrap();
    assert_eq!(a.rows_since(b.vv()).len(), 2);
    sync(&mut [&mut a, &mut b]);
    assert!(a.rows_since(b.vv()).is_empty());
    assert!(b.rows_since(a.vv()).is_empty());

    b.rename_chat("chat-2", "renamed by b").unwrap();
    let delta = b.rows_since(a.vv());
    assert_eq!(delta.len(), 1);
    assert_eq!(delta[0].id, "chat-2");
    sync(&mut [&mut a, &mut b]);
    assert_eq!(all_rows(&a), all_rows(&b));
    assert_eq!(
        a.chat("chat-2").unwrap().unwrap().title.as_deref(),
        Some("renamed by b")
    );
}

#[test]
fn rows_travel_through_a_replica_that_did_not_write_them() {
    let mut a = RegistryDoc::new("dev-a");
    let mut mailbox = RegistryDoc::new("dev-m");
    let mut c = RegistryDoc::new("dev-c");
    a.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
    sync(&mut [&mut a, &mut mailbox]);
    sync(&mut [&mut mailbox, &mut c]);
    assert_eq!(all_rows(&a), all_rows(&c));
    assert_eq!(c.vv().get("dev-a"), a.vv().get("dev-a"));
}

#[test]
fn non_owners_cannot_change_owner_only_fields_locally_or_at_merge() {
    let mut host = RegistryDoc::new("host");
    let mut viewer = RegistryDoc::new("viewer");
    host.upsert_chat(&chat("chat-1", "host")).unwrap();
    host.set_chat_last_message("chat-1", "from the host", ts(9_000))
        .unwrap();
    sync(&mut [&mut host, &mut viewer]);

    // Locally: the viewer's write of a host-owned field is dropped; a shared one lands.
    let before = viewer.generation();
    viewer
        .set_chat_last_message("chat-1", "forged", ts(10_000))
        .unwrap();
    assert_eq!(viewer.generation(), before);
    viewer.rename_chat("chat-1", "shared rename").unwrap();

    // At merge: a forged row claiming a newer write from the viewer is refused.
    let mut forged = viewer.live_row(KIND_CHATS, "chat-1").unwrap();
    let clock = encode_hlc(i64::MAX / 2, 0, "viewer");
    forged
        .fields
        .insert("lastMessagePreview".into(), json!("forged"));
    forged
        .clocks
        .insert("lastMessagePreview".into(), clock.clone());
    forged.fields.insert("deviceId".into(), json!("viewer"));
    forged.clocks.insert("deviceId".into(), clock);
    host.merge_remote(&[forged], None);
    sync(&mut [&mut host, &mut viewer]);

    for doc in [&host, &viewer] {
        let row = doc.chat("chat-1").unwrap().unwrap();
        assert_eq!(row.last_message_preview.as_deref(), Some("from the host"));
        assert_eq!(row.device_id, "host", "the first owner stands");
        assert_eq!(row.title.as_deref(), Some("shared rename"));
    }

    // A device row belongs to its device: only the name is anyone's.
    host.upsert_device(&device("host", "studio")).unwrap();
    sync(&mut [&mut host, &mut viewer]);
    viewer.rename_device("host", "renamed").unwrap();
    viewer.set_device_last_seen("host", ts(99_000)).unwrap();
    sync(&mut [&mut host, &mut viewer]);
    let seen = host.read_devices().unwrap();
    assert_eq!(seen[0].name, "renamed");
    assert_eq!(seen[0].last_seen_at, Some(ts(1_000)));

    // Where a chat runs is its host's, and its creator's: a viewer creates chats for a host.
    // A third device can move it neither locally nor by a forged row.
    let mut third = RegistryDoc::new("third");
    let mut born = chat("chat-2", "host");
    born.cwd = Some("/Users/host/repo".into());
    viewer.upsert_chat(&born).unwrap();
    sync(&mut [&mut host, &mut viewer, &mut third]);
    assert_eq!(
        host.chat("chat-2").unwrap().unwrap().cwd.as_deref(),
        Some("/Users/host/repo"),
        "the creator places it"
    );
    let before = third.generation();
    third.set_chat_cwd("chat-2", "/only/on/third").unwrap();
    third.set_chat_branch("chat-2", "third-branch").unwrap();
    assert_eq!(third.generation(), before);
    let mut forged = third.live_row(KIND_CHATS, "chat-2").unwrap();
    forged.fields.insert("cwd".into(), json!("/only/on/third"));
    forged
        .clocks
        .insert("cwd".into(), encode_hlc(i64::MAX / 2, 1, "third"));
    host.merge_remote(&[forged], None);
    host.set_chat_branch("chat-2", "feature").unwrap();
    viewer.set_chat_cwd("chat-2", "/Users/host/moved").unwrap();
    sync(&mut [&mut host, &mut viewer, &mut third]);
    for doc in [&host, &viewer, &third] {
        let row = doc.chat("chat-2").unwrap().unwrap();
        assert_eq!(row.cwd.as_deref(), Some("/Users/host/moved"));
        assert_eq!(row.branch.as_deref(), Some("feature"));
    }
}

#[test]
fn a_delete_and_a_later_write_converge_in_either_order() {
    for reverse in [false, true] {
        let mut a = RegistryDoc::new("dev-a");
        let mut b = RegistryDoc::new("dev-b");
        a.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
        sync(&mut [&mut a, &mut b]);
        a.delete_chat("chat-1").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        b.rename_chat("chat-1", "after the delete").unwrap();
        if reverse {
            sync(&mut [&mut b, &mut a]);
        } else {
            sync(&mut [&mut a, &mut b]);
        }
        assert_eq!(all_rows(&a), all_rows(&b));
        // The rename outlives the delete, nothing written before it does.
        let row = a.live_row(KIND_CHATS, "chat-1").unwrap();
        assert_eq!(row.fields.keys().collect::<Vec<_>>(), ["title"]);
        assert!(a.chat("chat-1").unwrap().is_none(), "no owner, no chat");
    }
}

#[test]
fn a_restored_replica_never_reuses_an_oseq_peers_have_seen() {
    let mut a = RegistryDoc::new("dev-a");
    let mut b = RegistryDoc::new("dev-b");
    a.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
    let backup = a.to_bytes().unwrap();
    a.rename_chat("chat-1", "two").unwrap();
    a.rename_chat("chat-1", "three").unwrap();
    sync(&mut [&mut a, &mut b]);

    let mut restored = RegistryDoc::from_bytes(&backup, "dev-a").unwrap();
    sync(&mut [&mut restored, &mut b]);
    restored.rename_chat("chat-1", "four").unwrap();
    assert_eq!(
        restored.rows_since(b.vv()).len(),
        1,
        "the new write is new to b"
    );
    sync(&mut [&mut restored, &mut b]);
    assert_eq!(
        b.chat("chat-1").unwrap().unwrap().title.as_deref(),
        Some("four")
    );
}

#[test]
fn snapshots_round_trip_rows_and_version_vector() {
    let mut a = RegistryDoc::new("dev-a");
    a.upsert_device(&device("dev-a", "laptop")).unwrap();
    a.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
    a.delete_chat("chat-1").unwrap();
    let restored = RegistryDoc::from_bytes(&a.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(all_rows(&restored), all_rows(&a));
    assert_eq!(restored.vv(), a.vv());
    assert!(restored.chat_deleted("chat-1"));
}

#[test]
fn an_edge_room_snapshot_loads_once_as_this_replicas_own_writes() {
    let mut source = RegistryDoc::new("dev-a");
    source.upsert_chat(&chat("chat-1", "dev-a")).unwrap();
    let row = source.live_row(KIND_CHATS, "chat-1").unwrap();
    let rename = RowOp {
        kind: KIND_CHATS.into(),
        id: "chat-1".into(),
        op: OpKind::Update,
        set: Some(fields([("title", json!("unacked rename"))])),
        hlc: encode_hlc(i64::MAX / 2, 0, "dev-a"),
        clocks: None,
    };
    let edge = json!({
        "v": 1,
        "resyncEpoch": 1,
        "deviceId": "dev-a",
        "serverSeq": 41,
        "gcFloor": 0,
        "clock": {"last_ms": 0, "counter": 0},
        "rows": [row],
        "pending": [{"batch": "b-1", "ops": [rename]}],
    });
    let doc = RegistryDoc::from_bytes(&serde_json::to_vec(&edge).unwrap(), "dev-a").unwrap();
    assert_eq!(
        doc.chat("chat-1").unwrap().unwrap().title.as_deref(),
        Some("unacked rename")
    );
    assert_eq!(doc.rows_since(&Vv::new()).len(), 1, "a fresh peer gets it");
    let reloaded = RegistryDoc::from_bytes(&doc.to_bytes().unwrap(), "dev-a").unwrap();
    assert_eq!(all_rows(&reloaded), all_rows(&doc));
}

#[test]
fn write_as_plays_another_device_in_one_replica() {
    let mut demo = RegistryDoc::new("phone");
    demo.write_as("mac", |doc| doc.upsert_device(&device("mac", "Studio")))
        .unwrap();
    demo.write_as("mac", |doc| {
        doc.upsert_session(&session("chat-1", "mac", SessionStatus::Working))
    })
    .unwrap();
    assert_eq!(demo.read_devices().unwrap(), vec![device("mac", "Studio")]);
    assert_eq!(demo.read_sessions().unwrap().len(), 1);
    assert_eq!(demo.device_id(), "phone");
}

#[derive(Debug, Clone)]
enum Act {
    Rename(usize, u8),
    LastMessage(usize, u8),
    Delete(usize),
    Recreate(usize),
    Forge(usize, u8),
    Move(usize, u8),
    Sync(usize, usize),
}

const DEVICES: [&str; 3] = ["dev-0", "dev-1", "dev-2"];

fn act() -> impl Strategy<Value = Act> {
    let doc = 0..DEVICES.len();
    prop_oneof![
        (doc.clone(), any::<u8>()).prop_map(|(d, v)| Act::Rename(d, v)),
        (doc.clone(), any::<u8>()).prop_map(|(d, v)| Act::LastMessage(d, v)),
        doc.clone().prop_map(Act::Delete),
        doc.clone().prop_map(Act::Recreate),
        (doc.clone(), any::<u8>()).prop_map(|(d, v)| Act::Forge(d, v)),
        (doc.clone(), any::<u8>()).prop_map(|(d, v)| Act::Move(d, v)),
        (doc.clone(), doc).prop_map(|(a, b)| Act::Sync(a, b)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any writes, deletes, forged rows, moves and partial exchanges, then a full exchange: every
    /// replica holds the same rows (I2), the owner-only preview only ever holds a value the owner
    /// wrote, and the cwd one the owner or the row's creator wrote (I5).
    #[test]
    fn replicas_converge_and_owner_fields_stay_the_owners(
        acts in proptest::collection::vec(act(), 0..40),
    ) {
        let mut docs: Vec<RegistryDoc> = DEVICES.iter().map(|d| RegistryDoc::new(*d)).collect();
        // dev-0 hosts the chat.
        docs[0].upsert_chat(&chat("chat", "dev-0")).unwrap();
        for act in acts {
            match act {
                Act::Rename(d, v) => {
                    docs[d].rename_chat("chat", &format!("title {v}")).unwrap();
                }
                Act::LastMessage(d, v) => {
                    let preview = format!("by {} {v}", DEVICES[d]);
                    docs[d].set_chat_last_message("chat", &preview, ts(1)).unwrap();
                }
                Act::Delete(d) => {
                    docs[d].delete_chat("chat").unwrap();
                }
                Act::Recreate(d) => {
                    docs[d].upsert_chat(&chat("chat", "dev-0")).unwrap();
                }
                Act::Forge(d, v) => {
                    let Some(mut row) = docs[d].row_or_tombstone(KIND_CHATS, "chat") else {
                        continue;
                    };
                    let clock = encode_hlc(i64::MAX / 2, u32::from(v), DEVICES[d]);
                    row.fields.insert("lastMessagePreview".into(), json!("forged"));
                    row.clocks.insert("lastMessagePreview".into(), clock);
                    let target = (d + 1) % DEVICES.len();
                    if d != 0 {
                        docs[target].merge_remote(&[row], None);
                    }
                }
                Act::Move(d, v) => {
                    docs[d].set_chat_cwd("chat", &format!("/by/{}/{v}", DEVICES[d])).unwrap();
                }
                Act::Sync(a, b) if a != b => {
                    let rows = docs[a].rows_since(docs[b].vv());
                    let vv = docs[a].vv().clone();
                    docs[b].merge_remote(&rows, Some(&vv));
                }
                Act::Sync(..) => {}
            }
        }
        let [a, b, c] = &mut docs[..] else { unreachable!() };
        sync(&mut [a, b, c]);
        let rows = all_rows(&docs[0]);
        for doc in &docs {
            prop_assert_eq!(&all_rows(doc), &rows);
            let preview = doc
                .row_or_tombstone(KIND_CHATS, "chat")
                .and_then(|r| r.fields.get("lastMessagePreview").cloned())
                .and_then(|v| v.as_str().map(str::to_owned));
            if let Some(preview) = preview {
                prop_assert!(preview.starts_with("by dev-0"), "{preview}");
            }
            let row = doc.row_or_tombstone(KIND_CHATS, "chat");
            let cwd = row
                .as_ref()
                .and_then(|r| r.fields.get("cwd"))
                .and_then(|v| v.as_str().map(str::to_owned));
            if let (Some(row), Some(cwd)) = (&row, cwd)
                && let Some(writer) = cwd.strip_prefix("/by/").and_then(|w| w.split('/').next())
            {
                prop_assert!(
                    row.owner() == Some(writer) || row.creator() == Some(writer),
                    "{cwd} on a row owned by {:?}, created by {:?}",
                    row.owner(),
                    row.creator()
                );
            }
        }
    }
}
