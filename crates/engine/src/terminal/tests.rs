//! The Terminal hub over scripted PTYs: pipes stand in for the shell, so no test starts one. A
//! viewer here is what the app's is, a mirror fed the watch, and must show what the host does.
use std::io::{PipeReader, PipeWriter, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use futures::StreamExt;
use futures::executor::block_on;
use futures::stream::BoxStream;
use onicode_protocol::terminal::{
    Bytes, CloseTerminal, Input, InputKind, Key, OpenTerminal, ProgramState, ResizeTerminal,
    TerminalColors, TerminalEvent, TerminalInput, TerminalSize, TerminalState, TerminalSummary,
    ViewerId, WatchTerminal,
};
use onicode_protocol::{Cursor, Item, TerminalId};
use onicode_term::mirror::Mirror;
use onicode_term::pty::{Process, Spawned, Spawner, Spec};
use proptest::prelude::*;

use super::hub::{TerminalFeed, TerminalHub, size_of};
use crate::feed::{Limits, Topic};
use crate::session::ThreadSpawn;

const SIZE: TerminalSize = TerminalSize {
    cols: 20,
    rows: 4,
    cell_width: 8,
    cell_height: 16,
};

/// One spawned shell, as the test drives it.
struct Shell {
    /// What the program writes.
    output: PipeWriter,
    /// What the hub wrote to the program.
    input: PipeReader,
    exit: mpsc::Sender<Option<i32>>,
    hung_up: Arc<AtomicBool>,
    sizes: Arc<Mutex<Vec<TerminalSize>>>,
}

impl Shell {
    fn write(&mut self, bytes: &[u8]) {
        self.output.write_all(bytes).unwrap();
        self.output.flush().unwrap();
    }

    /// The next `len` bytes the hub wrote to the program.
    fn read(&mut self, len: usize) -> Vec<u8> {
        let mut buf = vec![0; len];
        self.input.read_exact(&mut buf).unwrap();
        buf
    }
}

#[derive(Default)]
struct Pipes {
    shells: Mutex<Vec<Shell>>,
    fail: bool,
}

impl Pipes {
    fn shell(&self) -> Shell {
        self.shells.lock().unwrap().remove(0)
    }
}

struct Piped {
    hung_up: Arc<AtomicBool>,
    sizes: Arc<Mutex<Vec<TerminalSize>>>,
}

impl Process for Piped {
    fn resize(&self, size: TerminalSize) {
        self.sizes.lock().unwrap().push(size);
    }

    fn foreground(&self) -> Option<i32> {
        None
    }

    fn hangup(&self) {
        self.hung_up.store(true, Ordering::SeqCst);
    }
}

impl Spawner for Pipes {
    fn spawn(&self, _: &Spec, _: TerminalSize) -> Result<Spawned, String> {
        if self.fail {
            return Err("boom".into());
        }
        let (output, program_output) = std::io::pipe().map_err(|error| error.to_string())?;
        let (program_input, input) = std::io::pipe().map_err(|error| error.to_string())?;
        let (exit, exited) = mpsc::channel();
        let hung_up = Arc::new(AtomicBool::new(false));
        let sizes = Arc::new(Mutex::new(Vec::new()));
        self.shells.lock().unwrap().push(Shell {
            output: program_output,
            input: program_input,
            exit,
            hung_up: hung_up.clone(),
            sizes: sizes.clone(),
        });
        Ok(Spawned {
            process: Box::new(Piped { hung_up, sizes }),
            output: Box::new(output),
            input: Box::new(input),
            wait: Box::new(move || exited.recv().ok().flatten()),
        })
    }
}

fn hub(pipes: &Arc<Pipes>) -> Arc<TerminalHub> {
    TerminalHub::build(
        Some(pipes.clone()),
        Arc::new(|cwd: &str| {
            let spec = Spec {
                program: "fake-shell".into(),
                cwd: cwd.into(),
                ..Spec::default()
            };
            (spec, "xterm-256color")
        }),
        Arc::new(ThreadSpawn),
        Limits::default(),
        0,
    )
}

/// The viewer every test opens, types and sizes as, unless it names another.
fn me() -> ViewerId {
    ViewerId(uuid::Uuid::from_u128(1))
}

fn open(hub: &Arc<TerminalHub>, terminal: TerminalId) -> TerminalSummary {
    block_on(hub.open(OpenTerminal {
        terminal,
        viewer: me(),
        cwd: "/tmp".into(),
        size: SIZE,
        colors: TerminalColors::default(),
    }))
    .unwrap()
}

/// A viewer: a mirror fed what the watch brings.
struct Viewer {
    stream: BoxStream<'static, Item<TerminalState, TerminalEvent>>,
    mirror: Option<Mirror>,
    cursor: Option<Cursor>,
    events: Vec<TerminalEvent>,
    snapshots: usize,
    state: Option<TerminalState>,
}

impl Viewer {
    fn watch(hub: &TerminalHub, terminal: TerminalId) -> Self {
        Self::watch_as(hub, terminal, me())
    }

    fn watch_as(hub: &TerminalHub, terminal: TerminalId, viewer: ViewerId) -> Self {
        let stream = hub
            .watch(WatchTerminal {
                terminal,
                viewer,
                since: None,
            })
            .unwrap();
        Self {
            stream,
            mirror: None,
            cursor: None,
            events: Vec::new(),
            snapshots: 0,
            state: None,
        }
    }

    /// Comes back from the cursor it holds.
    fn rewatch(&mut self, hub: &TerminalHub, terminal: TerminalId) {
        self.stream = hub
            .watch(WatchTerminal {
                terminal,
                viewer: me(),
                since: self.cursor,
            })
            .unwrap();
    }

    /// Takes items until `done` holds.
    fn until(&mut self, done: impl Fn(&Self) -> bool) {
        while !done(self) {
            match block_on(self.stream.next()).expect("the watch goes on") {
                Item::Snapshot { at, state } => {
                    self.cursor = Some(at);
                    self.snapshots += 1;
                    self.mirror = Some(Mirror::attach(&state.screen.0, state.size));
                    self.state = Some(state);
                }
                Item::Events {
                    first,
                    last,
                    events,
                } => {
                    if let Some(cursor) = &mut self.cursor {
                        assert_eq!(first, cursor.seq + 1, "no gap and no repeat");
                        cursor.seq = last;
                    }
                    let mirror = self.mirror.as_mut().expect("a snapshot first");
                    for event in events {
                        match &event {
                            TerminalEvent::Output(bytes) => mirror.feed(&bytes.0),
                            TerminalEvent::Resized(size) => mirror.resize(*size),
                            TerminalEvent::History(bytes) => mirror.history(&bytes.0),
                            TerminalEvent::Clipboard(_)
                            | TerminalEvent::Exited { .. }
                            | TerminalEvent::Status(_) => {}
                        }
                        self.events.push(event);
                    }
                }
                Item::Resync => panic!("a viewer that keeps up never resyncs"),
            }
        }
    }

    fn text(&self) -> String {
        self.mirror.as_ref().map(Mirror::text).unwrap_or_default()
    }

    fn shows(&mut self, text: &str) {
        self.until(|viewer| viewer.text().contains(text));
    }
}

#[test]
fn output_reaches_viewers_and_the_host_answers_queries() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    assert!(!open(&hub, terminal).exited);
    let mut shell = pipes.shell();
    let mut viewer = Viewer::watch(&hub, terminal);
    shell.write(b"hello\r\n\x1b[6n");
    viewer.shows("hello");
    assert_eq!(shell.read(6), b"\x1b[2;1R");
    shell.write(b"\x1b]52;c;aGk=\x07");
    viewer.until(|viewer| {
        viewer
            .events
            .contains(&TerminalEvent::Clipboard("hi".into()))
    });
}

#[test]
fn input_is_encoded_from_the_hosts_modes() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    open(&hub, terminal);
    let mut shell = pipes.shell();
    let mut viewer = Viewer::watch(&hub, terminal);
    let send = |input| {
        let viewer = me();
        hub.input(TerminalInput {
            terminal,
            viewer,
            input,
        })
        .unwrap();
    };
    send(Input::Paste("a\nb".into()));
    assert_eq!(shell.read(3), b"a\rb");
    shell.write(b"\x1b[?2004hready");
    viewer.shows("ready");
    send(Input::Paste("a\nb".into()));
    let bracketed = b"\x1b[200~a\nb\x1b[201~";
    assert_eq!(shell.read(bracketed.len()), bracketed);
    send(Input::Key(Key {
        key: "enter".into(),
        ..Key::default()
    }));
    send(Input::Text("ls\r".into()));
    assert_eq!(shell.read(4), b"\rls\r");
}

#[test]
fn a_late_viewer_starts_from_the_snapshot_and_a_returning_one_from_its_cursor() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    open(&hub, terminal);
    let mut shell = pipes.shell();
    let mut first = Viewer::watch(&hub, terminal);
    // Output stops inside an escape sequence: the snapshot carries the half of it.
    shell.write(b"one\r\n\x1b[3");
    first.until(|viewer| !viewer.events.is_empty());
    let mut late = Viewer::watch(&hub, terminal);
    late.until(|viewer| viewer.mirror.is_some());
    shell.write(b"1mtwo");
    first.shows("two");
    late.shows("two");
    assert_eq!(late.text(), first.text());
    first.rewatch(&hub, terminal);
    shell.write(b"\r\nthree");
    first.shows("three");
    assert_eq!(first.snapshots, 1, "the ring still held what it missed");
}

#[test]
fn a_shell_that_ends_leaves_its_note_and_its_terminal() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    open(&hub, terminal);
    let shell = pipes.shell();
    let mut viewer = Viewer::watch(&hub, terminal);
    shell.exit.send(Some(3)).unwrap();
    viewer.until(|viewer| {
        viewer
            .events
            .contains(&TerminalEvent::Exited { code: Some(3) })
    });
    assert!(viewer.text().contains("[process exited (3)]"));
    assert!(hub.list().iter().all(|summary| summary.exited));
    assert_eq!(hub.list().len(), 1);
}

#[test]
fn a_shell_that_cannot_start_says_why() {
    let pipes = Arc::new(Pipes {
        fail: true,
        ..Pipes::default()
    });
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    assert!(!open(&hub, terminal).exited);
    let mut viewer = Viewer::watch(&hub, terminal);
    viewer.until(|viewer| viewer.mirror.is_some());
    assert!(
        // The notice wraps at the terminal's width.
        viewer
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("Could not start the shell (fake-shell): boom"),
        "{}",
        viewer.text()
    );
}

#[test]
fn a_resize_reaches_the_pty_the_host_and_viewers_once() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    open(&hub, terminal);
    let shell = pipes.shell();
    let mut viewer = Viewer::watch(&hub, terminal);
    let size = TerminalSize { cols: 30, ..SIZE };
    for _ in 0..2 {
        let viewer = me();
        hub.resize(ResizeTerminal {
            terminal,
            viewer,
            size,
        })
        .unwrap();
    }
    viewer.until(|viewer| viewer.events.contains(&TerminalEvent::Resized(size)));
    assert_eq!(*shell.sizes.lock().unwrap(), [size]);
    assert_eq!(size_of(&hub, terminal), Some(size));
    assert_eq!(viewer.mirror.as_ref().unwrap().size(), (30, 4));
}

#[test]
fn opening_twice_spawns_once_and_closing_hangs_up() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    open(&hub, terminal);
    open(&hub, terminal);
    let shell = pipes.shell();
    assert!(pipes.shells.lock().unwrap().is_empty());
    hub.close(CloseTerminal { terminal }).unwrap();
    assert!(shell.hung_up.load(Ordering::SeqCst));
    assert!(hub.list().is_empty());
    let input = TerminalInput {
        terminal,
        viewer: me(),
        input: Input::Text("x".into()),
    };
    assert!(hub.input(input).is_err());
}

#[test]
fn a_quiet_hub_hosts_terminals_without_a_shell() {
    let hub = TerminalHub::quiet(Arc::new(ThreadSpawn));
    let terminal = TerminalId::new();
    open(&hub, terminal);
    let input = TerminalInput {
        terminal,
        viewer: me(),
        input: Input::Text("x".into()),
    };
    assert!(hub.input(input).is_ok());
    assert_eq!(hub.list().len(), 1);
}

#[test]
fn a_watcher_paints_the_prefix_then_takes_the_history() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    open(&hub, terminal);
    let mut shell = pipes.shell();
    let mut first = Viewer::watch(&hub, terminal);
    let lines: String = (0..40).map(|n| format!("line {n}\r\n")).collect();
    shell.write(lines.as_bytes());
    first.shows("line 39");
    let mut late = Viewer::watch(&hub, terminal);
    late.until(|viewer| viewer.mirror.is_some());
    let mirror = late.mirror.as_ref().unwrap();
    assert!(
        mirror.awaiting_history(),
        "the first item paints without the history"
    );
    assert!(mirror.text().contains("line 39"));
    late.until(|viewer| {
        viewer
            .events
            .iter()
            .any(|event| matches!(event, TerminalEvent::History(_)))
    });
    assert!(!late.mirror.as_ref().unwrap().awaiting_history());
    shell.write(b"after");
    first.shows("after");
    late.shows("after");
    for viewer in [&mut first, &mut late] {
        viewer.mirror.as_mut().unwrap().select_all();
    }
    let all = |viewer: &Viewer| viewer.mirror.as_ref().unwrap().selection_text();
    assert!(
        all(&late).unwrap().contains("line 0\n"),
        "the history arrived"
    );
    assert_eq!(all(&late), all(&first));
}

#[test]
fn the_pty_takes_the_size_of_the_viewer_that_typed_last() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    open(&hub, terminal);
    let shell = pipes.shell();
    let phone = ViewerId(uuid::Uuid::from_u128(2));
    let mut mac = Viewer::watch(&hub, terminal);
    let iphone = Viewer::watch_as(&hub, terminal, phone);
    let wide = TerminalSize { cols: 30, ..SIZE };
    let narrow = TerminalSize { cols: 12, ..SIZE };
    let resize = |viewer, size| {
        hub.resize(ResizeTerminal {
            terminal,
            viewer,
            size,
        })
        .unwrap();
    };
    let type_as = |viewer| {
        let input = Input::Text("x".into());
        hub.input(TerminalInput {
            terminal,
            viewer,
            input,
        })
        .unwrap();
    };
    resize(me(), wide);
    type_as(me());
    resize(phone, narrow);
    assert_eq!(
        size_of(&hub, terminal),
        Some(wide),
        "the phone has not typed"
    );
    type_as(phone);
    assert_eq!(size_of(&hub, terminal), Some(narrow), "it typed: its size");
    drop(iphone);
    assert_eq!(
        size_of(&hub, terminal),
        Some(wide),
        "the typist left: the Mac's again"
    );
    mac.until(|viewer| viewer.events.contains(&TerminalEvent::Resized(wide)));
    assert_eq!(*shell.sizes.lock().unwrap(), [wide, narrow, wide]);
}

#[test]
fn a_programs_status_reaches_watchers_the_list_and_subscribers() {
    let pipes = Arc::new(Pipes::default());
    let hub = hub(&pipes);
    let terminal = TerminalId::new();
    open(&hub, terminal);
    let mut shell = pipes.shell();
    let mut statuses = hub.statuses();
    let mut viewer = Viewer::watch(&hub, terminal);
    shell.write(b"\x1b]7501;state=blocked:kind=permission:app=codex:msg=aGk=\x1b\\ok");
    viewer.shows("ok");
    let reported = viewer.events.iter().find_map(|event| match event {
        TerminalEvent::Status(status) => status.clone(),
        _ => None,
    });
    let status = reported.expect("a status event");
    assert_eq!(
        (
            status.state,
            status.kind,
            status.app.as_deref(),
            status.message.as_deref()
        ),
        (
            ProgramState::Blocked,
            Some(InputKind::Permission),
            Some("codex"),
            Some("hi")
        )
    );
    assert_eq!(hub.list()[0].status.as_ref(), Some(&status));
    assert_eq!(
        statuses.try_recv().unwrap(),
        (terminal, Some(status.clone()))
    );
    let mut late = Viewer::watch(&hub, terminal);
    late.until(|viewer| viewer.state.is_some());
    assert_eq!(late.state.as_ref().unwrap().status.as_ref(), Some(&status));
    // A new prompt ends a blocked record.
    shell.write(b"\x1b]133;A\x07$ ");
    viewer.until(|viewer| viewer.events.contains(&TerminalEvent::Status(None)));
    assert_eq!(hub.list()[0].status, None);
}

/// Adjacent outputs joined and adjacent statuses down to the last, so two batchings of the same
/// stream compare equal.
fn joined(events: Vec<TerminalEvent>) -> Vec<TerminalEvent> {
    let mut out: Vec<TerminalEvent> = Vec::new();
    for event in events {
        match (out.last_mut(), &event) {
            (Some(TerminalEvent::Output(last)), TerminalEvent::Output(more)) => {
                last.0.extend_from_slice(&more.0);
                continue;
            }
            (Some(TerminalEvent::Status(last)), TerminalEvent::Status(newer)) => {
                last.clone_from(newer);
                continue;
            }
            _ => {}
        }
        out.push(event);
    }
    out
}

/// Every event queued for watcher `id`.
fn drain_into(topic: &mut Topic<TerminalFeed>, id: u64, into: &mut Vec<TerminalEvent>) {
    while let Some(Some(item)) = topic.drain(id) {
        if let Item::Events { events, .. } = item {
            into.extend(events);
        }
    }
}

fn event() -> impl Strategy<Value = TerminalEvent> {
    prop_oneof![
        4 => proptest::collection::vec(any::<u8>(), 0..8)
            .prop_map(|bytes| TerminalEvent::Output(Bytes(bytes))),
        1 => (1u16..4).prop_map(|cols| TerminalEvent::Resized(TerminalSize { cols, ..SIZE })),
        1 => "[a-z]{0,4}".prop_map(TerminalEvent::Clipboard),
        1 => proptest::option::of(Just(ProgramState::Working)).prop_map(|state| {
            TerminalEvent::Status(state.map(|state| onicode_protocol::terminal::ProgramStatus {
                state,
                kind: None,
                progress: None,
                app: None,
                title: None,
                message: None,
            }))
        }),
    ]
}

proptest! {
    /// A watcher that lets its queue fill gets the same stream, with outputs merged, as one that
    /// drains every event as it comes.
    #[test]
    fn a_slow_watcher_gets_the_stream_with_outputs_merged(
        batches in proptest::collection::vec(proptest::collection::vec(event(), 1..4), 0..24),
    ) {
        let state = TerminalState::default();
        let mut topic = Topic::<TerminalFeed>::new(1, state, Limits::default());
        let (slow, _) = topic.join(None);
        let (fast, _) = topic.join(None);
        let mut sent = Vec::new();
        let mut quick = Vec::new();
        for batch in batches {
            sent.extend(batch.iter().cloned());
            topic.publish(batch);
            drain_into(&mut topic, fast, &mut quick);
        }
        let mut late = Vec::new();
        drain_into(&mut topic, slow, &mut late);
        prop_assert_eq!(joined(late), joined(sent.clone()));
        prop_assert_eq!(joined(quick), joined(sent));
    }
}
