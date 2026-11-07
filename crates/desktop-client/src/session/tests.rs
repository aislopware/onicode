//! The session's model as a fold of desktopd's frames, and the coalescing of queued moves.

use std::net::SocketAddr;

use onicode_desktop_link::broker::DesktopEndpoint;
use onicode_desktop_link::{Clip, Down, HostGrants, Pane, Seat, WIRE};
use onicode_desktop_proto::DisplayId;
use onicode_desktop_proto::clipboard::Withheld;
use onicode_desktop_proto::input::{Mods, MouseButton};
use onicode_desktop_proto::screen::{
    CaptureTarget, DisplayInfo, DisplayKey, DisplayShape, OpenAsk, Quality, ScreenEvent,
    ScreenFailure, ScreenInput, ScreenRequest, SourceState, StreamId, VideoCodec, VirtualDisplay,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;

use super::{Book, Command, Effect, KeptCopy, Open, Phase, coalesce};

fn endpoint() -> DesktopEndpoint {
    DesktopEndpoint {
        device_id: "mac".into(),
        host: "mac-studio".into(),
        addrs: vec![SocketAddr::from(([100, 64, 0, 1], 7434))],
        wire: WIRE,
        grants: HostGrants::default(),
        displays: Vec::new(),
    }
}

fn display(id: u32) -> DisplayInfo {
    DisplayInfo {
        id: DisplayId(id),
        w: 1512.0,
        h: 982.0,
        scale: 2.0,
        hz: 120.0,
    }
}

fn granted() -> HostGrants {
    HostGrants {
        screen_recording: true,
        accessibility: true,
        post_event: true,
    }
}

fn welcome(seat: Seat) -> Down {
    Down::Welcome {
        wire: WIRE,
        host: "mac-studio".into(),
        displays: vec![display(1), display(2)],
        grants: granted(),
        seat,
    }
}

fn opened(stream: u32, display: u32) -> Down {
    Down::Screen(ScreenEvent::Opened {
        stream: StreamId(stream),
        target: CaptureTarget::Display(DisplayId(display)),
        codec: VideoCodec::Hevc,
        width: 3024,
        height: 1964,
        scale: 2.0,
        stripes: Vec::new(),
    })
}

/// A welcome joins; a display asked for is open once desktopd says so, and its stream is
/// spawned; a link that drops leaves it wanted, and the next welcome opens it again.
#[test]
fn a_wanted_display_is_opened_and_opened_again_after_a_drop() {
    let mut book = Book::new(&endpoint());
    assert_eq!(book.state.phase, Phase::Connecting);
    assert_eq!(book.down(welcome(Seat::default())), vec![Effect::Reopen]);
    assert_eq!(book.state.phase, Phase::Joined);
    let ask = book.open(Open::Display(DisplayId(2)), Quality::default());
    assert!(matches!(
        ask,
        Some(ScreenRequest::Open {
            target: CaptureTarget::Display(DisplayId(2)),
            ..
        })
    ));
    assert_eq!(
        book.down(opened(7, 2)),
        vec![Effect::Spawn {
            stream: StreamId(7),
            codec: VideoCodec::Hevc
        }]
    );
    assert_eq!(book.wanted[0].stream, Some(StreamId(7)));
    assert_eq!(book.state.streams.len(), 1);
    assert_eq!(book.state.streams[0].display, Some(DisplayId(2)));

    book.unlinked();
    assert!(book.state.streams.is_empty());
    assert_eq!(book.wanted[0].stream, None, "still wanted, open on no link");
    assert_eq!(book.down(welcome(Seat::default())), vec![Effect::Reopen]);
    let _spawn = book.down(opened(1, 2));
    assert_eq!(book.wanted[0].stream, Some(StreamId(1)));
}

/// A view that asks again for a display already wanted, open or waiting on the next welcome,
/// asks desktopd nothing: one want, one capture. A made display asked again at another shape
/// keeps its one want, at the new shape.
#[test]
fn a_display_asked_twice_is_asked_of_desktopd_once() {
    let mut book = Book::new(&endpoint());
    let _ = book.down(welcome(Seat::default()));
    assert!(
        book.open(Open::Display(DisplayId(1)), Quality::default())
            .is_some()
    );
    let _ = book.down(opened(4, 1));
    book.unlinked();
    assert_eq!(
        book.open(Open::Display(DisplayId(1)), Quality::default()),
        None,
        "wanted already: the welcome opens it"
    );
    assert_eq!(book.wanted.len(), 1);
    let key = DisplayKey([3; 16]);
    let shape = |width| DisplayShape {
        width,
        height: 1600,
        scale: 2.0,
        refresh_hz: 120,
    };
    assert!(
        book.open(
            Open::Made {
                key,
                shape: shape(2560)
            },
            Quality::default()
        )
        .is_some()
    );
    assert_eq!(
        book.open(
            Open::Made {
                key,
                shape: shape(1920)
            },
            Quality::default()
        ),
        None
    );
    assert_eq!(book.wanted.len(), 2);
    assert_eq!(
        book.wanted[1].open,
        Open::Made {
            key,
            shape: shape(1920)
        }
    );
}

/// A display made for this device: its `Display` event comes before `Opened`, and the stream
/// it opens is the made one's.
#[test]
fn a_made_display_is_matched_by_its_key() {
    let key = DisplayKey([7; 16]);
    let shape = DisplayShape {
        width: 2360,
        height: 1640,
        scale: 2.0,
        refresh_hz: 120,
    };
    let mut book = Book::new(&endpoint());
    let _ = book.down(welcome(Seat::default()));
    let _ = book.open(Open::Display(DisplayId(1)), Quality::default());
    let _ = book.open(Open::Made { key, shape }, Quality::default());
    let _ = book.down(Down::Screen(ScreenEvent::Display {
        stream: StreamId(3),
        key,
        display: VirtualDisplay::Made(DisplayId(9)),
    }));
    let _ = book.down(opened(3, 9));
    assert_eq!(
        book.wanted[0].stream, None,
        "the physical display is not it"
    );
    assert_eq!(book.wanted[1].stream, Some(StreamId(3)));
    assert_eq!(
        book.state.streams[0].made,
        Some(VirtualDisplay::Made(DisplayId(9)))
    );
}

/// A Mac whose grants are off says which panes; an open it refuses is no longer wanted and
/// says why; a stream it closes goes.
#[test]
fn refusals_name_what_is_off_and_stop_wanting() {
    let mut book = Book::new(&endpoint());
    let _ = book.down(Down::NotPermitted {
        missing: vec![Pane::ScreenRecording],
    });
    assert_eq!(
        book.state.phase,
        Phase::NotPermitted {
            missing: vec![Pane::ScreenRecording]
        }
    );
    let _ = book.down(Down::Grants(granted()));
    assert_eq!(book.state.grants, granted());

    let _ = book.down(welcome(Seat::default()));
    let _ = book.open(Open::Display(DisplayId(5)), Quality::default());
    let asked = OpenAsk::Target(CaptureTarget::Display(DisplayId(5)));
    let _ = book.down(Down::Screen(ScreenEvent::OpenFailed {
        asked,
        why: ScreenFailure::Gone,
    }));
    assert!(book.wanted.is_empty());
    assert_eq!(book.state.failure, Some((asked, ScreenFailure::Gone)));

    let _ = book.open(Open::Display(DisplayId(1)), Quality::default());
    assert_eq!(
        book.state.failure, None,
        "a new open clears the last refusal"
    );
    let _ = book.down(opened(4, 1));
    assert_eq!(
        book.down(Down::Screen(ScreenEvent::Closed {
            stream: StreamId(4),
            reason: "display gone".into()
        })),
        vec![Effect::Drop(StreamId(4))]
    );
    assert!(book.wanted.is_empty() && book.state.streams.is_empty());
}

/// Events about a stream land on its view; the cursor and the source go to its handle too.
#[test]
fn stream_events_land_on_their_view() {
    let mut book = Book::new(&endpoint());
    let _ = book.down(welcome(Seat::default()));
    let _ = book.open(Open::Display(DisplayId(1)), Quality::default());
    let _ = book.down(opened(4, 1));
    let s = StreamId(4);
    let _ = book.down(Down::Screen(ScreenEvent::Geometry {
        stream: s,
        width: 1920,
        height: 1080,
        stripes: Vec::new(),
    }));
    assert_eq!(
        book.down(Down::Screen(ScreenEvent::Source {
            stream: s,
            state: SourceState::Locked
        })),
        vec![Effect::Source {
            stream: s,
            live: false
        }]
    );
    let view = &book.state.streams[0];
    assert_eq!(
        (view.width, view.height, view.source),
        (1920, 1080, SourceState::Locked)
    );
    assert_eq!(
        book.down(Down::Screen(ScreenEvent::Cursor {
            stream: s,
            shape: None
        })),
        vec![Effect::Cursor {
            stream: s,
            shape: None
        }]
    );
}

/// Input refused for not holding the controls shows until this device holds them.
#[test]
fn refused_input_shows_until_the_controls_are_ours() {
    let mut book = Book::new(&endpoint());
    let _ = book.down(welcome(Seat {
        controller: Some("MacBook Pro".into()),
        you: false,
        clients: 2,
    }));
    let _ = book.down(Down::NotController);
    assert!(book.state.input_refused);
    let _ = book.down(Down::Seat(Seat {
        controller: Some("iPhone".into()),
        you: true,
        clients: 2,
    }));
    assert!(!book.state.input_refused);
    assert!(book.state.seat.you);
}

/// A copy past the clipboard's caps shows until a copy within them follows on its Mac: the
/// later of this device's and the Mac's, and the Mac's only while its link stands.
#[test]
fn a_copy_past_the_cap_shows_until_one_within_follows() {
    let mut book = Book::new(&endpoint());
    let _ = book.down(welcome(Seat::default()));
    assert_eq!(book.state.clipboard_kept, None);
    let there = |at, size| Down::Clip(Clip::Withheld(Some(Withheld { at, size })));

    assert_eq!(book.down(there(1_000, Some(20 << 20))), Vec::new());
    assert_eq!(
        book.state.clipboard_kept,
        Some(KeptCopy {
            here: false,
            size: Some(20 << 20)
        })
    );
    book.kept_here(Some(Withheld {
        at: 2_000,
        size: None,
    }));
    assert_eq!(
        book.state.clipboard_kept,
        Some(KeptCopy {
            here: true,
            size: None
        }),
        "the later copy: a folder here"
    );
    let _ = book.down(there(3_000, Some(17 << 20)));
    assert_eq!(book.state.clipboard_kept.map(|kept| kept.here), Some(false));
    let _ = book.down(Down::Clip(Clip::Withheld(None)));
    assert_eq!(
        book.state.clipboard_kept.map(|kept| kept.here),
        Some(true),
        "the Mac's next copy went: this device's still stays"
    );
    book.kept_here(None);
    assert_eq!(book.state.clipboard_kept, None);

    let _ = book.down(there(4_000, Some(18 << 20)));
    book.unlinked();
    assert_eq!(
        book.state.clipboard_kept, None,
        "the Mac says again over the next link"
    );
}

/// A Mac on another session wire is not joined.
#[test]
fn another_wire_is_refused() {
    let mut book = Book::new(&endpoint());
    let down = Down::Welcome {
        wire: WIRE + 1,
        host: "mac".into(),
        displays: Vec::new(),
        grants: granted(),
        seat: Seat::default(),
    };
    assert_eq!(book.down(down), vec![Effect::WrongWire(WIRE + 1)]);
    assert_eq!(book.state.phase, Phase::Connecting);
}

fn input(stream: u32, kind: u8, i: u16) -> Command {
    let x = f32::from(i);
    let input = match kind % 3 {
        0 => ScreenInput::Move { x, y: x },
        1 => ScreenInput::Button {
            button: MouseButton::Left,
            down: true,
            x,
            y: x,
            clicks: 1,
            mods: Mods::empty(),
        },
        _ => ScreenInput::SmartMagnify { x, y: x },
    };
    Command::Input(StreamId(stream), input)
}

/// What a batch of queued input is reduced to.
fn shape(batch: &[Command]) -> Vec<(u32, bool, f32)> {
    batch
        .iter()
        .filter_map(|command| match command {
            Command::Input(stream, input) => Some((
                stream.0,
                matches!(input, ScreenInput::Move { .. }),
                match input {
                    ScreenInput::Move { x, .. }
                    | ScreenInput::Button { x, .. }
                    | ScreenInput::SmartMagnify { x, .. } => *x,
                    _ => -1.0,
                },
            )),
            _ => None,
        })
        .collect()
}

#[test]
fn queued_moves_go_as_the_newest() {
    let batch = vec![
        input(1, 0, 1),
        input(1, 0, 2),
        input(2, 0, 3),
        input(1, 0, 4),
        input(1, 1, 5),
        input(1, 0, 6),
    ];
    assert_eq!(
        shape(&coalesce(batch)),
        vec![
            (2, true, 3.0),
            (1, true, 4.0),
            (1, false, 5.0),
            (1, true, 6.0)
        ]
    );
}

proptest! {
    /// Coalescing keeps every event that is not a move, in order; per stream it keeps the
    /// move before each of them and the last move, and nothing it keeps is reordered.
    #[test]
    fn coalescing_keeps_every_ordered_event_and_the_last_move(
        events in proptest::collection::vec((1_u32..3, 0_u8..3), 0..64),
    ) {
        let batch: Vec<Command> = events
            .iter()
            .zip(0_u16..)
            .map(|(&(stream, kind), i)| input(stream, kind, i))
            .collect();
        let before = shape(&batch);
        let after = shape(&coalesce(batch));
        let ordered = |v: &[(u32, bool, f32)]| -> Vec<(u32, bool, f32)> {
            v.iter().copied().filter(|e| !e.1).collect()
        };
        prop_assert_eq!(ordered(&after), ordered(&before));
        for stream in 1..3 {
            let last_move = |v: &[(u32, bool, f32)]| {
                v.iter().copied().rfind(|e| e.0 == stream && e.1)
            };
            prop_assert_eq!(last_move(&after), last_move(&before));
        }
        // A subsequence of what was queued.
        let mut rest = before.iter();
        for kept in &after {
            prop_assert!(rest.any(|e| e == kept), "{after:?} from {before:?}");
        }
    }
}
