//! The pipeline on the drawn platform ([`crate::synthetic::Drawn`]): a stream opens on a
//! keyframe, a refresh is answered from an acknowledged reference and a lost one with a
//! keyframe, a full link drops captures and beats instead, reports move the rate, a switch
//! reshapes the stream, and input lands on the streamed display.
//!
//! The drawn platform draws, codes and injects on the runtime ([`Platform::ON_RUNTIME`]), so these
//! run on a paused clock: every wait is virtual time, and a run repeats exactly. They assert what
//! the pipeline must do, never how fast.

use std::sync::atomic::{AtomicBool, AtomicUsize};

use onicode_desktop_media::wire::{Kind, MediaHeader, flags};
use onicode_desktop_proto::screen::RateVerdict;
use proptest::prelude::*;
use tokio::sync::mpsc;

use super::*;
use crate::synthetic::{self, Drawn};

/// The longest any one thing the tests wait for may take, in virtual time.
const PATIENCE: Duration = Duration::from_secs(3);
/// More than the largest datagram queue a frame fits behind.
const FULL: usize = 64 << 20;

/// A link the test holds: what is sent lands in a channel, and the test sets how much the
/// link holds.
struct Sink {
    tx: mpsc::UnboundedSender<Bytes>,
    held: AtomicUsize,
    closed: AtomicBool,
}

impl DatagramSink for Sink {
    fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Refused::Closed);
        }
        for datagram in datagrams {
            let _gone = self.tx.send(datagram.clone());
        }
        Ok(())
    }

    fn max_size(&self) -> Option<usize> {
        Some(1_200)
    }

    fn held(&self) -> usize {
        self.held.load(Ordering::Relaxed)
    }

    fn cwnd(&self) -> u64 {
        1 << 20
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

/// A stream the test drives.
struct Opened {
    pipeline: Pipeline<Drawn>,
    event: ScreenEvent,
    sink: Arc<Sink>,
    datagrams: mpsc::UnboundedReceiver<Bytes>,
    events: mpsc::UnboundedReceiver<StreamEvent>,
}

fn quality() -> Quality {
    Quality {
        bitrate_bps: 12_000_000,
        ..Quality::default()
    }
}

async fn open(display: u32) -> Opened {
    let (tx, datagrams) = mpsc::unbounded_channel();
    let (told, events) = mpsc::unbounded_channel();
    let sink = Arc::new(Sink {
        tx,
        held: AtomicUsize::new(0),
        closed: AtomicBool::new(false),
    });
    let (pipeline, event) = Pipeline::<Drawn>::open(Opening {
        id: StreamId(7),
        target: CaptureTarget::Display(DisplayId(display)),
        quality: quality(),
        sink: Arc::clone(&sink) as Arc<dyn DatagramSink>,
        events: Arc::new(move |event: StreamEvent| {
            let _gone = told.send(event);
        }),
        sound: None,
    })
    .await
    .expect("the drawn display opens");
    Opened {
        pipeline,
        event,
        sink,
        datagrams,
        events,
    }
}

/// A datagram's header, as the client reads it.
#[derive(Clone, Copy, Debug)]
struct Seen {
    kind: Option<Kind>,
    frame: u32,
    index: u16,
    flags: u8,
}

impl Seen {
    fn of(datagram: &[u8]) -> Self {
        let (header, _payload) = MediaHeader::parse(datagram).expect("a media datagram");
        Self {
            kind: header.kind(),
            frame: header.frame.get(),
            index: header.index.get(),
            flags: header.flags,
        }
    }

    /// The first piece of a frame the encoder coded (not a copy sent again).
    const fn starts_a_frame(&self) -> bool {
        matches!(self.kind, Some(Kind::VideoData))
            && self.index == 0
            && self.flags & flags::RETRANSMIT == 0
    }

    const fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

/// Every datagram sent within `within`.
async fn gather(rx: &mut mpsc::UnboundedReceiver<Bytes>, within: Duration) -> Vec<Seen> {
    let deadline = tokio::time::Instant::now() + within;
    let mut seen = Vec::new();
    while let Ok(Some(datagram)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        seen.push(Seen::of(&datagram));
    }
    seen
}

/// The datagrams up to and with the first `wanted` one, within [`PATIENCE`].
async fn until(
    rx: &mut mpsc::UnboundedReceiver<Bytes>,
    wanted: impl Fn(&Seen) -> bool,
) -> (Vec<Seen>, Seen) {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut before = Vec::new();
    loop {
        let datagram = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("the datagram came in time")
            .expect("the stream still sends");
        let seen = Seen::of(&datagram);
        if wanted(&seen) {
            return (before, seen);
        }
        before.push(seen);
    }
}

fn drain(rx: &mut mpsc::UnboundedReceiver<Bytes>) {
    while rx.try_recv().is_ok() {}
}

/// The stream's first frame is a keyframe offering the first reference, and frames keep
/// coming with the display's size and scale told in `Opened`.
#[tokio::test(start_paused = true)]
async fn a_stream_opens_on_a_keyframe_and_keeps_coding() {
    let mut s = open(1).await;
    let ScreenEvent::Opened {
        stream,
        target,
        width,
        height,
        scale,
        stripes,
        ..
    } = &s.event
    else {
        panic!("not Opened: {:?}", s.event);
    };
    assert_eq!(*stream, StreamId(7));
    assert_eq!(*target, CaptureTarget::Display(DisplayId(1)));
    assert_eq!(
        (*width, *height),
        (1280, 800),
        "the drawn Retina panel's pixels"
    );
    assert!((scale - 2.0).abs() < f32::EPSILON, "two pixels a point");
    assert!(stripes.is_empty(), "too small to stripe");

    let (_before, first) = until(&mut s.datagrams, Seen::starts_a_frame).await;
    assert!(first.has(flags::KEYFRAME), "the first frame stands alone");
    assert!(first.has(flags::LTR), "and offers the first reference");
    let more = gather(&mut s.datagrams, Duration::from_millis(500)).await;
    let frames = more.iter().filter(|d| d.starts_a_frame()).count();
    assert!(frames >= 5, "frames keep coming: {frames}");
    let stats = s.pipeline.stats();
    assert!(stats.encoded >= 5, "{stats:?}");
    assert!(stats.ltr_offered >= 1, "{stats:?}");
    s.pipeline.close().await;
}

/// A refresh is answered from the reference the client acknowledged, which costs no keyframe;
/// once the client says it lost its references, only a keyframe answers.
#[tokio::test(start_paused = true)]
async fn an_acknowledged_reference_answers_a_refresh_and_a_lost_one_a_keyframe() {
    let mut s = open(1).await;
    let (_before, first) = until(&mut s.datagrams, Seen::starts_a_frame).await;
    assert!(first.has(flags::KEYFRAME));
    // The fake encoder's first frame offers token 1.
    let ack = ReceiverReport {
        acked_ltr: [1, 0, 0, 0],
        acked_ltr_len: 1,
        ..ReceiverReport::default()
    };
    let _decision = s.pipeline.report(0, &ack, None);
    assert_eq!(s.pipeline.stats().ltr_acked, 1, "token 1 was offered");
    tokio::time::sleep(Duration::from_micros(KEYFRAME_IN_FLIGHT_US)).await;
    drain(&mut s.datagrams);

    s.pipeline.request_refresh(0, first.frame, false);
    let (before, refresh) = until(&mut s.datagrams, |d| {
        d.starts_a_frame() && d.has(flags::LTR_REFRESH)
    })
    .await;
    assert!(
        !refresh.has(flags::KEYFRAME),
        "predicted from the reference"
    );
    assert!(
        before.iter().all(|d| !d.has(flags::KEYFRAME)),
        "no keyframe on the way: {before:?}"
    );

    s.pipeline.request_refresh(0, refresh.frame, true);
    let (_before, keyframe) = until(&mut s.datagrams, |d| {
        d.starts_a_frame() && d.has(flags::KEYFRAME)
    })
    .await;
    assert!(!keyframe.has(flags::LTR_REFRESH));
    assert_eq!(s.pipeline.stats().refreshes, 2);
    s.pipeline.close().await;
}

/// While the link holds more than a frame fits behind, captures are dropped, a heartbeat fills
/// the silence and a nack waits; once it drains, frames flow again and a nack is answered.
#[tokio::test(start_paused = true)]
async fn a_full_link_drops_captures_and_beats_until_it_drains() {
    let mut s = open(1).await;
    let (_before, _first) = until(&mut s.datagrams, Seen::starts_a_frame).await;
    s.sink.held.store(FULL, Ordering::Relaxed);
    // What the encoder had in hand goes out first.
    tokio::time::sleep(Duration::from_millis(100)).await;
    drain(&mut s.datagrams);

    let full = gather(&mut s.datagrams, Duration::from_millis(400)).await;
    assert!(
        full.iter()
            .all(|d| !matches!(d.kind, Some(Kind::VideoData | Kind::VideoParity))),
        "no video into a full link: {full:?}"
    );
    assert!(
        full.iter().any(|d| d.kind == Some(Kind::Heartbeat)),
        "a heartbeat fills the silence: {full:?}"
    );
    let stats = s.pipeline.stats();
    assert!(stats.dropped > 0, "{stats:?}");
    assert!(stats.heartbeats > 0, "{stats:?}");

    s.sink.held.store(0, Ordering::Relaxed);
    let (_before, resumed) = until(&mut s.datagrams, Seen::starts_a_frame).await;

    s.sink.held.store(FULL, Ordering::Relaxed);
    drain(&mut s.datagrams);
    s.pipeline.nack(0, resumed.frame, &[0]);
    let waiting = gather(&mut s.datagrams, Duration::from_millis(100)).await;
    assert!(
        waiting.iter().all(|d| !d.has(flags::RETRANSMIT)),
        "a nack waits while the link is full"
    );

    s.sink.held.store(0, Ordering::Relaxed);
    s.pipeline.nack(0, resumed.frame, &[0]);
    let (_before, again) = until(&mut s.datagrams, |d| d.has(flags::RETRANSMIT)).await;
    assert_eq!((again.frame, again.index), (resumed.frame, 0));
    s.pipeline.close().await;
}

/// Every tenth report decides the rate: a window of deep queues cuts it, and the cut is told
/// to the client as `Rate`.
#[tokio::test(start_paused = true)]
async fn deep_queues_cut_the_rate() {
    let mut s = open(1).await;
    let (_before, _first) = until(&mut s.datagrams, Seen::starts_a_frame).await;
    let deep = ReceiverReport {
        frames_ok: 6,
        queue_depth: 5,
        ..ReceiverReport::default()
    };
    for _ in 0..9 {
        assert!(
            s.pipeline.report(0, &deep, None).is_none(),
            "nine reports decide nothing"
        );
    }
    let decision = s
        .pipeline
        .report(0, &deep, None)
        .expect("the tenth decides");
    assert_eq!(decision.verdict, RateVerdict::Cut);
    assert!(decision.target_bps < quality().bitrate_bps, "{decision:?}");
    let told = loop {
        let event = tokio::time::timeout(PATIENCE, s.events.recv())
            .await
            .expect("told in time")
            .expect("the stream tells");
        if let StreamEvent::Screen(ScreenEvent::Rate {
            target_bps,
            verdict,
            ..
        }) = event
        {
            break (target_bps, verdict);
        }
    };
    assert_eq!(told, (decision.target_bps, RateVerdict::Cut));
    s.pipeline.close().await;
}

/// Switching to another display reshapes the stream to that display's pixels, on a new
/// keyframe.
#[tokio::test(start_paused = true)]
async fn a_switch_reshapes_the_stream() {
    let mut s = open(1).await;
    let (_before, _first) = until(&mut s.datagrams, Seen::starts_a_frame).await;
    let geometry = s
        .pipeline
        .switch_display(DisplayId(2))
        .await
        .expect("the other drawn display");
    let ScreenEvent::Geometry {
        stream,
        width,
        height,
        stripes,
    } = geometry
    else {
        panic!("not Geometry: {geometry:?}");
    };
    assert_eq!(stream, StreamId(7));
    assert_eq!((width, height), (320, 200));
    assert!(stripes.is_empty());
    assert_eq!(s.pipeline.target(), CaptureTarget::Display(DisplayId(2)));
    drain(&mut s.datagrams);
    let (_before, keyframe) = until(&mut s.datagrams, |d| {
        d.starts_a_frame() && d.has(flags::KEYFRAME)
    })
    .await;
    assert!(keyframe.has(flags::KEYFRAME));
    s.pipeline.close().await;
}

/// The controller's input lands on the streamed display, and the pointer it moved goes to the
/// client as cursor datagrams.
#[tokio::test(start_paused = true)]
async fn input_lands_on_the_streamed_display() {
    let mut s = open(1).await;
    let (_before, _first) = until(&mut s.datagrams, Seen::starts_a_frame).await;
    // The still pointer's first sample is out; a cursor datagram from now on is the move's.
    tokio::time::sleep(Duration::from_millis(50)).await;
    drain(&mut s.datagrams);
    let moved = ScreenInput::Move { x: 401.0, y: 203.0 };
    s.pipeline
        .inject(&moved)
        .expect("the drawn display has bounds");
    assert!(
        synthetic::recorded().contains(&(CaptureTarget::Display(DisplayId(1)), moved)),
        "the injector was handed the move on display 1"
    );
    assert_eq!(s.pipeline.stats().inputs, 1);
    let (_before, _cursor) = until(&mut s.datagrams, |d| d.kind == Some(Kind::Cursor)).await;
    s.pipeline.close().await;
}

/// The capture and the encoder for a display: the stream's sides follow the scale, even and at
/// least 2; HEVC pads them to 16 for the encoder; the frame rate is the display's at most, and
/// the capture follows the display's own rate when it has one.
#[test]
fn configs_follow_the_display_and_the_quality() {
    let hevc = Quality {
        fps: 120,
        ..Quality::default()
    };
    let (capture, encoder) = configs((1366, 768), &hevc, Some(60.0), Chroma::Subsampled);
    assert_eq!((capture.width, capture.height), (1366, 768));
    assert_eq!((encoder.width, encoder.height), (1376, 768), "padded to 16");
    assert_eq!(encoder.fps, 60, "no faster than the display");
    assert_eq!(capture.fps, 0, "the display's own rate");

    let h264 = Quality {
        codec: VideoCodec::H264,
        scale: 0.5,
        fps: 30,
        bitrate_bps: 0,
        ..Quality::default()
    };
    let (capture, encoder) = configs((1366, 768), &h264, None, Chroma::Subsampled);
    assert_eq!((capture.width, capture.height), (684, 384));
    assert_eq!(
        (encoder.width, encoder.height),
        (684, 384),
        "H.264 keeps the even size"
    );
    assert_eq!(capture.fps, 30, "no display rate: the asked one");
    assert_eq!(encoder.bitrate_bps, 100_000, "a floor under the rate");
}

proptest! {
    /// Every side is even, at least 2, never larger than the display's, and the encoder's
    /// covers the capture's, padded to the codec's multiple.
    #[test]
    fn configs_are_coded_whole(
        width in 1_u32..8_000,
        height in 1_u32..8_000,
        scale in 0.0_f32..2.0,
        fps in 0_u16..300,
        hz in proptest::option::of(1.0_f64..240.0),
        hevc in any::<bool>(),
    ) {
        let quality = Quality {
            scale,
            fps,
            codec: if hevc { VideoCodec::Hevc } else { VideoCodec::H264 },
            ..Quality::default()
        };
        let (capture, encoder) = configs((width, height), &quality, hz, Chroma::Subsampled);
        for (side, native) in [(capture.width, width), (capture.height, height)] {
            prop_assert!(side >= 2 && side % 2 == 0, "{side}");
            prop_assert!(side <= native.max(2).next_multiple_of(2), "{side} of {native}");
        }
        let multiple = if hevc { HEVC_ALIGN } else { 2 };
        prop_assert_eq!(encoder.width % multiple, 0);
        prop_assert_eq!(encoder.height % multiple, 0);
        prop_assert!(encoder.width >= capture.width && encoder.height >= capture.height);
        prop_assert!((1..=240).contains(&encoder.fps));
    }
}
