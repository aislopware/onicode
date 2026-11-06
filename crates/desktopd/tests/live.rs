//! The pipeline on this Mac's own display: ScreenCaptureKit and VideoToolbox. Opt-in
//! (`ONICODE_DESKTOP_LIVE=1`, and `--ignored`), and only once Screen Recording reads granted,
//! since asking for shareable content without it would prompt.

#![cfg(target_os = "macos")]

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use onicode_desktop_media::wire::{Kind, MediaHeader, flags};
use onicode_desktop_proto::StreamId;
use onicode_desktop_proto::screen::{CaptureTarget, Quality, ScreenEvent};
use onicode_desktop_testkit::live;
use onicode_desktopd::pipeline::{DatagramSink, Opening, Pipeline, Refused, StreamEvent};
use onicode_desktopd::platform::Native;
use tokio::sync::mpsc;

struct Sink(mpsc::UnboundedSender<Bytes>);

impl DatagramSink for Sink {
    fn send(&self, datagrams: &[Bytes]) -> Result<(), Refused> {
        for datagram in datagrams {
            self.0
                .send(datagram.clone())
                .map_err(|_gone| Refused::Closed)?;
        }
        Ok(())
    }

    fn max_size(&self) -> Option<usize> {
        Some(1_200)
    }

    fn held(&self) -> usize {
        0
    }

    fn cwnd(&self) -> u64 {
        1 << 20
    }

    fn is_closed(&self) -> bool {
        self.0.is_closed()
    }
}

/// The first physical display streams: `Opened` with its size, then a keyframe and more frames.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: ONICODE_DESKTOP_LIVE=1, Screen Recording granted"]
async fn the_main_display_streams_from_a_keyframe() {
    if !live::opted_in(live::DESKTOP_LIVE) {
        return;
    }
    if !onicode_desktop_grants::Grants::current().screen_recording {
        live::skip("no Screen Recording grant for this test binary");
        return;
    }
    let display = Pipeline::<Native>::physical_display(None)
        .await
        .expect("a display");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (pipeline, opened) = Pipeline::<Native>::open(Opening {
        id: StreamId(1),
        target: CaptureTarget::Display(display),
        quality: Quality {
            bitrate_bps: 8_000_000,
            scale: 0.5,
            ..Quality::default()
        },
        sink: Arc::new(Sink(tx)),
        events: Arc::new(|event: StreamEvent| {
            assert!(
                !matches!(event, StreamEvent::Stopped(_)),
                "the capture stopped: {event:?}"
            );
        }),
        sound: None,
    })
    .await
    .expect("the display opens");
    let ScreenEvent::Opened { width, height, .. } = opened else {
        panic!("not Opened: {opened:?}");
    };
    assert!(width >= 2 && height >= 2);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let (mut frames, mut keyframes) = (0_u32, 0_u32);
    while let Ok(Some(datagram)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        let (header, _payload) = MediaHeader::parse(&datagram).expect("a media datagram");
        if header.kind() == Some(Kind::VideoData) && header.index.get() == 0 {
            frames += 1;
            keyframes += u32::from(header.flags & flags::KEYFRAME != 0);
        }
    }
    assert!(keyframes >= 1, "the stream starts on a keyframe");
    assert!(frames >= 1, "frames flow: {frames}");
    pipeline.close().await;
}
