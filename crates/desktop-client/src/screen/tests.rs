//! The receive path with the fake decoder: the bookkeeping on its own, then a stream task fed
//! packetized frames through the router, with seeded loss.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use onicode_desktop_media::wire::{ClockEcho, Feedback, StreamId, VideoCodec};
use onicode_desktop_media::{
    EncodedFrame, FrameStamp, Packetizer, ReassemblerStats, cursor_datagram,
};
use onicode_desktop_proto::screen::Stripe;
use parking_lot::Mutex;
use pretty_assertions::assert_eq;

use super::{
    ARRIVALS, Coded, CursorState, Delivery, Failed, Inflight, Parked, ScreenHandle, ScreenStats,
    Stitch, Uplink, observed_parity, refresh_after_refusal, refresh_for, spawn_screen,
};
use crate::decode::fake::{self, Drawn, Fake};
use crate::router::ScreenRouter;

const STREAM: StreamId = StreamId(5);

fn parked(
    pts_us: u64,
    arrived: Instant,
    ltr_token: Option<u64>,
    restarts: bool,
    discardable: bool,
) -> Parked {
    Parked {
        pts_us,
        arrived,
        ltr_token,
        restarts,
        discardable,
        coded: Coded::default(),
    }
}

/// The decoder's callback finds the arrival its frame was parked under, and the frames before
/// it are forgotten with it.
#[test]
fn a_parked_arrival_comes_back_with_its_frame_and_clears_the_older_ones() {
    let epoch = Instant::now();
    let at = |ms: u64| epoch + Duration::from_millis(ms);
    let mut arrivals = Inflight::default();
    for i in 0..4_u64 {
        arrivals.park(parked(i * 1_000, at(i * 16), None, false, false));
    }
    assert_eq!(arrivals.decoded(2_000), Some((at(32), Coded::default())));
    assert_eq!(arrivals.decoded(1_000), None, "older frames went with it");
    assert_eq!(arrivals.decoded(3_000), Some((at(48), Coded::default())));
    assert_eq!(arrivals.decoded(3_000), None, "taken once");
}

/// A long-term reference is acknowledged when its picture comes back, never when it was only
/// submitted, and never when the decoder failed on it.
#[test]
fn a_token_is_acknowledged_by_its_picture_and_a_failure_is_kept() {
    let epoch = Instant::now();
    let mut inflight = Inflight::default();
    inflight.park(parked(1, epoch, Some(11), false, false));
    inflight.park(parked(2, epoch, Some(12), false, false));
    inflight.park(parked(3, epoch, None, false, false));
    inflight.park(parked(4, epoch, Some(14), false, false));
    assert!(inflight.acks.is_empty(), "submitted is not decoded");
    inflight.failed(1, -12_909, false);
    assert_eq!(inflight.decoded(2), Some((epoch, Coded::default())));
    inflight.failed(3, -12_903, true);
    assert_eq!(inflight.acks, vec![12], "only the frame that came back");
    assert_eq!(
        inflight.failed,
        Some(Failed {
            pts_us: 3,
            session_lost: true,
            restart: false,
            count: 2
        })
    );
    assert_eq!(inflight.decoded(4), Some((epoch, Coded::default())));
    assert_eq!(inflight.acks, vec![12, 14]);
    assert!(inflight.parked.is_empty());
}

/// A refresh that itself fails is answered with a keyframe; a failure after a refresh that
/// decoded asks for a refresh, and one before the last restart for nothing.
#[test]
fn a_failed_refresh_asks_for_a_keyframe() {
    let epoch = Instant::now();
    let mut inflight = Inflight::default();
    inflight.park(parked(10, epoch, None, true, false));
    inflight.park(parked(11, epoch, None, false, false));
    inflight.failed(10, -12_909, false);
    let failed = inflight.failed.take().unwrap();
    assert_eq!(
        refresh_for(failed, Some(10), true),
        Some(true),
        "the refresh failed"
    );
    inflight.failed(11, -12_909, false);
    let failed = inflight.failed.take().unwrap();
    assert_eq!(refresh_for(failed, Some(10), true), Some(false));
    inflight.park(parked(13, epoch, None, true, false));
    inflight.park(parked(14, epoch, None, false, false));
    inflight.failed(13, -12_909, false);
    inflight.failed(14, -12_909, false);
    let failed = inflight.failed.take().unwrap();
    assert_eq!(
        refresh_for(failed, Some(13), true),
        Some(true),
        "the refresh was among them"
    );
    inflight.park(parked(12, epoch, None, false, false));
    inflight.failed(12, -12_909, false);
    let failed = inflight.failed.take().unwrap();
    assert_eq!(
        refresh_for(failed, Some(20), true),
        None,
        "already replaced"
    );
    assert_eq!(
        refresh_for(failed, Some(12), false),
        Some(true),
        "no session left"
    );
}

#[test]
fn a_refused_frame_asks_by_what_it_was_and_what_is_left() {
    assert_eq!(refresh_after_refusal(true, true, false), None);
    assert_eq!(refresh_after_refusal(true, false, false), Some(true));
    assert_eq!(refresh_after_refusal(false, true, false), Some(false));
    assert_eq!(refresh_after_refusal(false, true, true), Some(true));
    assert_eq!(refresh_after_refusal(false, false, false), Some(true));
}

#[test]
fn a_failed_frame_nothing_refers_to_asks_for_nothing_unless_the_session_went() {
    let epoch = Instant::now();
    let mut inflight = Inflight::default();
    inflight.park(parked(1, epoch, Some(11), false, true));
    inflight.failed(1, -12_909, false);
    assert_eq!((inflight.failed, inflight.failed_discardable), (None, 1));
    assert!(inflight.acks.is_empty());
    inflight.park(parked(2, epoch, None, false, true));
    inflight.failed(2, -12_903, true);
    assert_eq!(
        inflight.failed,
        Some(Failed {
            pts_us: 2,
            session_lost: true,
            restart: false,
            count: 1
        })
    );
}

#[test]
fn the_ring_forgets_the_oldest_arrivals() {
    let epoch = Instant::now();
    let mut arrivals = Inflight::default();
    let bound = u64::try_from(ARRIVALS).unwrap();
    for i in 0..bound + 10 {
        arrivals.park(parked(i, epoch, None, false, false));
    }
    assert_eq!(arrivals.parked.len(), ARRIVALS);
    assert_eq!(arrivals.decoded(0), None);
    assert_eq!(arrivals.decoded(bound), Some((epoch, Coded::default())));
}

#[test]
fn the_observed_parity_is_the_shard_ratio_in_thousandths() {
    let stats = |data: u64, parity: u64| ReassemblerStats {
        data_shards: data,
        parity_shards: parity,
        ..ReassemblerStats::default()
    };
    assert_eq!(observed_parity(&stats(0, 0)), 0);
    assert_eq!(observed_parity(&stats(10, 2)), 200);
    assert_eq!(observed_parity(&stats(3, 1)), 333);
    assert_eq!(observed_parity(&stats(1, 100)), u16::MAX);
}

mod stitch {
    use pretty_assertions::assert_eq;

    use super::*;

    /// A stripe's picture, named by its capture and its stripe.
    type Pic = (u64, usize);

    const BOTH: u8 = 0b11;

    fn at(epoch: Instant, ms: f64) -> Instant {
        epoch + Duration::from_secs_f64(ms / 1000.0)
    }

    fn stamp(pts: u64, at: Instant) -> FrameStamp {
        FrameStamp {
            pts_us: pts,
            decode_seq: 0,
            arrived: at,
            decoded: at,
            captured: None,
        }
    }

    fn decode(
        stitch: &mut Stitch<Pic>,
        index: usize,
        pts: u64,
        (stripes, build): (u8, u8),
        when: Instant,
    ) -> Option<(Pic, Pic)> {
        let coded = Coded { stripes, build };
        stitch
            .decoded(index, (pts, index), stamp(pts, when), coded, when)
            .map(|j| (j.top, j.lower))
    }

    #[test]
    fn both_stripes_of_a_capture_go_up_together() {
        let t = Instant::now();
        let mut stitch = Stitch::default();
        assert_eq!(decode(&mut stitch, 0, 10, (BOTH, 1), at(t, 0.0)), None);
        assert!(stitch.waiting());
        let joined = stitch
            .decoded(
                1,
                (10, 1),
                stamp(10, at(t, 3.0)),
                Coded {
                    stripes: BOTH,
                    build: 1,
                },
                at(t, 3.0),
            )
            .expect("both in");
        assert_eq!((joined.top, joined.lower), ((10, 0), (10, 1)));
        assert_eq!(
            (joined.stamp.pts_us, joined.stamp.arrived),
            (10, at(t, 3.0))
        );
        assert!(!stitch.waiting());
        assert!(stitch.settle(at(t, 40.0)).is_none(), "shown once");
        assert_eq!(stitch.tears, 0);
    }

    #[test]
    fn a_stripe_coded_alone_goes_up_beside_the_others_picture() {
        let t = Instant::now();
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), at(t, 0.0));
        assert!(decode(&mut stitch, 1, 10, (BOTH, 1), at(t, 1.0)).is_some());
        assert_eq!(
            decode(&mut stitch, 1, 20, (0b10, 1), at(t, 2.0)),
            Some(((10, 0), (20, 1)))
        );
        assert_eq!(stitch.tears, 0);
    }

    #[test]
    fn a_late_stripe_leaves_the_capture_up_with_its_previous_picture() {
        let t = Instant::now();
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), at(t, 0.0));
        let _both = decode(&mut stitch, 1, 10, (BOTH, 1), at(t, 0.0));
        assert_eq!(decode(&mut stitch, 0, 20, (BOTH, 1), at(t, 10.0)), None);
        assert!(stitch.settle(at(t, 26.0)).is_none(), "within the wait");
        let torn = stitch
            .settle(at(t, 27.0))
            .map(|j| (j.top, j.lower, j.stamp.pts_us));
        assert_eq!(torn, Some(((20, 0), (10, 1), 20)));
        assert_eq!(stitch.tears, 1);
        assert_eq!(
            decode(&mut stitch, 1, 20, (BOTH, 1), at(t, 30.0)),
            Some(((20, 0), (20, 1)))
        );
    }

    #[test]
    fn a_stalled_stripe_holds_nothing_up() {
        let t = Instant::now();
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), at(t, 0.0));
        let _both = decode(&mut stitch, 1, 10, (BOTH, 1), at(t, 0.0));
        stitch.set_stalled(1, true);
        assert_eq!(
            decode(&mut stitch, 0, 20, (BOTH, 1), at(t, 8.0)),
            Some(((20, 0), (10, 1)))
        );
        assert_eq!(stitch.tears, 1);
        stitch.set_stalled(1, false);
        assert_eq!(
            decode(&mut stitch, 1, 40, (0b10, 1), at(t, 20.0)),
            Some(((20, 0), (40, 1)))
        );
    }

    /// A stripe a capture behind the other on a 120 Hz beat never starves the picture: one
    /// goes up at least once a wait and a beat, and captures only go forward.
    #[test]
    fn a_stripe_running_behind_never_starves_the_picture() {
        let t = Instant::now();
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 0, (BOTH, 1), at(t, 0.0));
        let _both = decode(&mut stitch, 1, 0, (BOTH, 1), at(t, 0.0));
        let mut shown: Vec<(f64, (Pic, Pic))> = vec![(0.0, ((0, 0), (0, 1)))];
        let mut now = 0.0;
        for capture in 1..=24_u64 {
            now += 8.333;
            let top = decode(&mut stitch, 0, capture * 10, (BOTH, 1), at(t, now));
            let lower = decode(
                &mut stitch,
                1,
                (capture - 1) * 10,
                (BOTH, 1),
                at(t, now + 1.0),
            );
            let late = stitch.settle(at(t, now + 2.0)).map(|j| (j.top, j.lower));
            shown.extend(top.map(|p| (now, p)));
            shown.extend(lower.map(|p| (now + 1.0, p)));
            shown.extend(late.map(|p| (now + 2.0, p)));
        }
        let gaps: Vec<f64> = shown.windows(2).map(|w| w[1].0 - w[0].0).collect();
        assert!(
            gaps.iter().all(|&gap| gap <= 16.667 + 8.333 + 2.0),
            "{gaps:?}"
        );
        assert!(shown.len() >= 8, "{shown:?}");
        let shown: Vec<(Pic, Pic)> = shown.into_iter().skip(1).map(|(_, p)| p).collect();
        assert!(shown.windows(2).all(|w| w[0].0.0 < w[1].0.0), "{shown:?}");
    }

    #[test]
    fn stripes_of_two_builds_never_go_up_together() {
        let t = Instant::now();
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), at(t, 0.0));
        let _both = decode(&mut stitch, 1, 10, (BOTH, 1), at(t, 0.0));
        assert_eq!(decode(&mut stitch, 0, 20, (BOTH, 2), at(t, 8.0)), None);
        assert!(
            stitch.settle(at(t, 40.0)).is_none(),
            "the lower is of the old build"
        );
        assert_eq!(
            decode(&mut stitch, 1, 40, (0b10, 2), at(t, 60.0)),
            Some(((20, 0), (40, 1)))
        );
    }

    #[test]
    fn a_whole_picture_forgets_the_stripes() {
        let t = Instant::now();
        let mut stitch = Stitch::default();
        let _first = decode(&mut stitch, 0, 10, (BOTH, 1), at(t, 0.0));
        let _both = decode(&mut stitch, 1, 10, (BOTH, 1), at(t, 0.0));
        stitch.tears = 3;
        stitch.whole();
        assert!(!stitch.waiting());
        assert_eq!(stitch.tears, 3, "the count is the stream's");
        assert_eq!(decode(&mut stitch, 1, 30, (0b10, 1), at(t, 2.0)), None);
    }
}

/// What a test's present hook saw of one picture.
#[derive(Clone, Debug)]
struct Seen {
    first: u8,
    pts_us: u64,
    size: (usize, usize),
    striped: bool,
}

/// A stream task on its own runtime with the fake decoder, and everything it sent caught.
struct Harness {
    rt: tokio::runtime::Runtime,
    router: ScreenRouter,
    handle: Option<ScreenHandle<Drawn>>,
    sent: Arc<Mutex<Vec<Feedback>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    /// The link is up; `false` makes the next feedback say it is gone.
    alive: Arc<AtomicBool>,
    rtt_reads: Arc<AtomicU32>,
    routed: AtomicU64,
}

impl Harness {
    fn start(drop_permille: u32) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let router = ScreenRouter::with_loss(drop_permille);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let alive = Arc::new(AtomicBool::new(true));
        let rtt_reads = Arc::new(AtomicU32::new(0));
        let (caught, up, reads) = (
            Arc::clone(&sent),
            Arc::clone(&alive),
            Arc::clone(&rtt_reads),
        );
        let uplink = Uplink {
            feedback: Box::new(move |bytes: Bytes| {
                if !up.load(Ordering::Relaxed) {
                    return Delivery::Gone;
                }
                caught
                    .lock()
                    .push(Feedback::decode(&bytes).expect("feedback reads back"));
                Delivery::Sent
            }),
            rtt: Box::new(move || {
                reads.fetch_add(1, Ordering::Relaxed);
                Some(Duration::from_millis(10))
            }),
        };
        let handle = spawn_screen::<Fake>(rt.handle(), &router, STREAM, VideoCodec::Hevc, uplink);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let shown = Arc::clone(&seen);
        handle.set_present(Some(Arc::new(move |picture| {
            shown.lock().push(Seen {
                first: picture.frame.image.bytes.first().copied().unwrap_or(0),
                pts_us: picture.stamp.pts_us,
                size: picture.size(),
                striped: picture.stripes.is_some(),
            });
        })));
        Self {
            rt,
            router,
            handle: Some(handle),
            sent,
            seen,
            alive,
            rtt_reads,
            routed: AtomicU64::new(0),
        }
    }

    fn handle(&self) -> &ScreenHandle<Drawn> {
        self.handle.as_ref().unwrap()
    }

    fn route(&self, datagram: Bytes) {
        self.routed.fetch_add(1, Ordering::Relaxed);
        self.router.route(datagram, Instant::now());
    }

    fn route_all(&self, datagrams: impl IntoIterator<Item = Bytes>) {
        for d in datagrams {
            self.route(d);
        }
    }

    /// Poll `done` every few milliseconds for up to `secs`.
    fn wait_for(&self, what: &str, secs: u64, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                self.handle().stats()
            );
            self.rt
                .block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
        }
    }

    /// Wait until a report has counted every datagram routed so far.
    fn settle(&self) -> ScreenStats {
        self.wait_for("the task to catch up", 5, |h| {
            h.handle().stats().datagrams == h.routed.load(Ordering::Relaxed)
        });
        self.handle().stats()
    }

    fn acked(&self) -> Vec<u64> {
        self.sent
            .lock()
            .iter()
            .filter_map(|f| match f {
                Feedback::Report { report, .. } => {
                    Some(report.acked_ltr[..usize::from(report.acked_ltr_len)].to_vec())
                }
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn refreshes(&self) -> Vec<bool> {
        self.sent
            .lock()
            .iter()
            .filter_map(|f| match f {
                Feedback::Refresh { keyframe, .. } => Some(*keyframe),
                _ => None,
            })
            .collect()
    }
}

/// A frame for the fake decoder: its first byte says what it does.
fn unit(first: u8, len: usize) -> Vec<u8> {
    let mut data = vec![first];
    data.resize(len, 0xaa);
    data
}

struct Cut {
    first: u8,
    keyframe: bool,
    ltr_token: Option<u64>,
    capture_ts_us: u32,
    stripes: u8,
}

fn cut(packetizer: &mut Packetizer, frame: &Cut) -> Vec<Bytes> {
    let data = unit(frame.first, 3000);
    let encoded = EncodedFrame {
        data: &data,
        keyframe: frame.keyframe,
        ltr_token: frame.ltr_token,
        ltr_refresh: false,
        discardable: false,
        capture_ts_us: frame.capture_ts_us,
        stripes: frame.stripes,
    };
    packetizer
        .packetize(&encoded, 0, |_| {})
        .unwrap()
        .datagrams
        .clone()
}

fn key(capture_ts_us: u32, ltr_token: Option<u64>) -> Cut {
    Cut {
        first: fake::KEY,
        keyframe: true,
        ltr_token,
        capture_ts_us,
        stripes: 0,
    }
}

fn delta(first: u8, capture_ts_us: u32, ltr_token: Option<u64>) -> Cut {
    Cut {
        first,
        keyframe: false,
        ltr_token,
        capture_ts_us,
        stripes: 0,
    }
}

/// The whole path once: the newest cursor wins, a keyframe missing a fragment is NACKed and
/// completed by its retransmission, decoded, handed to the present hook and its reference
/// acknowledged in a report; the link going ends the task, and the last handle let go
/// unroutes the stream.
#[test]
fn a_stream_reassembles_nacks_decodes_presents_and_ends_with_the_link() {
    let mut h = Harness::start(0);
    h.route(cursor_datagram(STREAM, 2, 0, 10, 20, true));
    h.wait_for("the cursor", 3, |h| {
        let c = h.handle().cursor().borrow().clone();
        (c.x, c.y, c.visible) == (10, 20, true)
    });
    h.route(cursor_datagram(STREAM, 1, 0, 99, 99, false));
    h.route(cursor_datagram(STREAM, 3, 0, 11, 21, false));
    h.wait_for("the newer cursor", 3, |h| {
        *h.handle().cursor().borrow()
            == CursorState {
                x: 11,
                y: 21,
                visible: false,
                ..CursorState::default()
            }
    });

    let mut packetizer = Packetizer::new(STREAM);
    packetizer.set_parity_permille(0);
    let datagrams = cut(&mut packetizer, &key(1_000, Some(7)));
    assert!(datagrams.len() >= 3, "{} datagrams", datagrams.len());
    h.route_all(
        datagrams
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != 1)
            .map(|(_, d)| d.clone()),
    );
    h.wait_for("a nack", 3, |h| {
        h.sent.lock().iter().any(|f| {
            matches!(f, Feedback::Nack { stream, frame: 0, fragments } if *stream == STREAM && fragments == &[1])
        })
    });
    h.route_all(packetizer.retransmit(0, &[1]));
    h.wait_for("the picture", 3, |h| !h.seen.lock().is_empty());
    let seen = h.seen.lock()[0].clone();
    assert_eq!(
        (seen.first, seen.size, seen.striped),
        (fake::KEY, (fake::WIDTH, fake::HEIGHT), false)
    );
    h.wait_for("the reference acknowledged", 3, |h| h.acked().contains(&7));
    let stats = h.settle();
    assert_eq!(
        (stats.frames, stats.frames_retransmit, stats.nacks),
        (1, 1, 1)
    );
    assert_eq!(stats.datagrams_lost, 0, "recovered, so not lost");
    assert!(stats.first_decoded_at.is_some());
    assert_eq!(
        h.handle()
            .frames()
            .borrow()
            .as_ref()
            .map(|p| p.stamp.pts_us),
        Some(seen.pts_us)
    );
    assert_eq!(stats.pacing.presented, 0, "no glass reported yet");
    let stamp = h.handle().frames().borrow().as_ref().unwrap().stamp;
    h.handle().presented(stamp, Some(Instant::now()));
    assert_eq!(h.handle().stats().pacing.presented, 1);

    h.handle().set_cursor_shape(None);
    assert_eq!(h.handle().cursor().borrow().shape_serial, 1);

    h.alive.store(false, Ordering::Relaxed);
    h.route_all(
        cut(&mut packetizer, &delta(b'P', 2_000, None))
            .into_iter()
            .skip(1),
    );
    h.wait_for("the task to end", 3, |h| h.handle().finished());
    let other = h.handle().clone();
    h.handle = None;
    assert_eq!(h.router.attached(), 1, "a clone holds the stream");
    drop(other);
    assert_eq!(h.router.attached(), 0, "the last handle unroutes");
}

/// A frame the decoder refuses asks for a refresh at once, and its reference is never
/// acknowledged: submitted is not decoded.
#[test]
fn a_refused_frame_asks_for_a_refresh_and_acknowledges_nothing() {
    let h = Harness::start(0);
    let mut packetizer = Packetizer::new(STREAM);
    packetizer.set_parity_permille(0);
    h.route_all(cut(&mut packetizer, &key(1_000, Some(7))));
    h.wait_for("the keyframe", 3, |h| h.seen.lock().len() == 1);
    h.route_all(cut(&mut packetizer, &delta(fake::REFUSE, 2_000, Some(8))));
    h.wait_for("the refusal and its refresh", 3, |h| {
        h.handle().stats().decode_errors == 1 && h.refreshes().contains(&false)
    });
    h.wait_for("the keyframe's reference acknowledged", 3, |h| {
        h.acked().contains(&7)
    });
    // Reports enough for the refused frame's token to have been acknowledged, were it held.
    h.rt.block_on(async { tokio::time::sleep(super::REPORT_EVERY * 5).await });
    assert!(!h.acked().contains(&8), "{:?}", h.acked());
    assert_eq!(h.handle().stats().decode_failure, Some(-12_909));
}

/// A frame that loses the decoder's session asks for a keyframe: no reference is left.
#[test]
fn a_lost_session_asks_for_a_keyframe() {
    let h = Harness::start(0);
    let mut packetizer = Packetizer::new(STREAM);
    packetizer.set_parity_permille(0);
    h.route_all(cut(&mut packetizer, &key(1_000, None)));
    h.wait_for("the keyframe", 3, |h| h.seen.lock().len() == 1);
    let before = h.refreshes().len();
    h.route_all(cut(&mut packetizer, &delta(fake::LOSE, 2_000, None)));
    h.wait_for("a keyframe asked for", 3, |h| {
        h.refreshes()[before..].contains(&true)
    });
}

/// A submission that never returns holds up neither the stream nor its reports: the decoder
/// is given up on after `DECODE_STUCK` and another asks for a keyframe. What the one given up
/// on returns once it is let go is not shown.
#[test]
fn a_decoder_stuck_in_a_submission_is_replaced_and_the_stream_runs_on() {
    let h = Harness::start(0);
    let mut packetizer = Packetizer::new(STREAM);
    packetizer.set_parity_permille(0);
    h.route_all(cut(&mut packetizer, &key(1_000, None)));
    h.wait_for("the keyframe", 3, |h| h.seen.lock().len() == 1);
    fake::gate::close();
    h.route_all(cut(&mut packetizer, &delta(fake::HOLD, 2_000, None)));
    h.wait_for("the submission to hang", 3, |_| fake::gate::waiting() > 0);
    let before = h.refreshes().len();
    h.wait_for("the decoder replaced", 10, |h| {
        h.handle().stats().decoders_replaced == 1
    });
    h.wait_for("a keyframe for the new one", 3, |h| {
        h.refreshes()[before..].contains(&true)
    });
    fake::gate::open();
    h.wait_for("the old thread to let go", 3, |_| {
        fake::gate::waiting() == 0
    });
    h.route_all(cut(&mut packetizer, &key(3_000, None)));
    h.wait_for("the new decoder's picture", 3, |h| h.seen.lock().len() == 2);
    assert!(
        h.seen.lock().iter().all(|s| s.first == fake::KEY),
        "the held frame never showed"
    );
}

/// Probes go out at once and then four times a second; an echo places the Mac's capture clock
/// on this one, 5 s ahead reading back as 5 s ahead within the round trip.
#[test]
fn clock_probes_go_out_and_an_echo_places_the_mac_clock() {
    let h = Harness::start(0);
    let probes = |h: &Harness| -> Vec<u64> {
        h.sent
            .lock()
            .iter()
            .filter_map(|f| match f {
                Feedback::Clock { sent_us, .. } => Some(*sent_us),
                _ => None,
            })
            .collect()
    };
    h.wait_for("the first probes", 3, |h| probes(h).len() >= 3);
    assert!(h.handle().stats().clock.is_none(), "no echo, no estimate");
    let sent_us = probes(&h)[2];
    let on_mac = 5_000_000 + sent_us;
    h.rt.block_on(async { tokio::time::sleep(Duration::from_millis(2)).await });
    let routed_at = Instant::now();
    h.route(ClockEcho::new(sent_us, on_mac, on_mac + 30).datagram(STREAM, 0));
    h.wait_for("the estimate", 3, |h| h.handle().stats().clock.is_some());
    let estimate = h.handle().stats().clock.unwrap();
    let left = estimate.anchor.captured(on_mac).unwrap();
    assert!(left <= routed_at, "{estimate:?}");
    assert!(left >= routed_at - estimate.rtt, "{estimate:?}");
    // The round trip is read once a report (and once as the stream starts), never per wake.
    let reports = h
        .sent
        .lock()
        .iter()
        .filter(|f| matches!(f, Feedback::Report { .. }))
        .count();
    let reads = h.rtt_reads.load(Ordering::Relaxed);
    assert!(
        reads <= u32::try_from(reports).unwrap() + 2,
        "{reads} reads, {reports} reports"
    );
}

/// A capture coded as two stripes, each on its media stream, goes up as one picture of both.
#[test]
fn a_striped_capture_goes_up_as_one_picture() {
    let h = Harness::start(0);
    let mut top = Packetizer::new(STREAM);
    let mut lower = Packetizer::new(Stripe::media_of(STREAM, 1));
    for packetizer in [&mut top, &mut lower] {
        packetizer.set_parity_permille(0);
        packetizer.set_build(1);
    }
    let striped = Cut {
        stripes: 0b11,
        ..key(1_000, None)
    };
    h.route_all(cut(&mut top, &striped));
    h.route_all(cut(&mut lower, &striped));
    h.wait_for("the stitched picture", 3, |h| {
        h.seen.lock().iter().any(|s| s.striped)
    });
    let seen = h.seen.lock().iter().find(|s| s.striped).cloned().unwrap();
    let overlap = usize::try_from(onicode_desktop_codec::stripes::OVERLAP).unwrap();
    assert_eq!(seen.size, (fake::WIDTH, 2 * (fake::HEIGHT - overlap)));
    assert!(h.settle().striped);
}

/// Seeded loss on the link, the host answering NACKs and refreshes as desktopd does: every
/// frame's picture still comes out in order, the loss repaired by parity and retransmission.
#[test]
fn seeded_loss_is_repaired_and_the_pictures_come_out_in_order() {
    let h = Harness::start(80);
    let mut packetizer = Packetizer::new(STREAM);
    packetizer.set_parity_permille(250);
    let mut answered = 0;
    let mut keyframe_due = true;
    let frames = 60_u32;
    let mut serve = |h: &Harness, packetizer: &mut Packetizer, keyframe_due: &mut bool| {
        let sent = h.sent.lock()[answered..].to_vec();
        answered += sent.len();
        for feedback in sent {
            match feedback {
                Feedback::Nack {
                    frame, fragments, ..
                } => {
                    h.route_all(packetizer.retransmit(frame, &fragments));
                }
                Feedback::Refresh { .. } => *keyframe_due = true,
                _ => {}
            }
        }
    };
    for n in 1..=frames {
        let ts = n * 16_667;
        let frame = if keyframe_due {
            key(ts, None)
        } else {
            delta(b'P', ts, None)
        };
        keyframe_due = false;
        h.route_all(cut(&mut packetizer, &frame));
        h.rt.block_on(async { tokio::time::sleep(Duration::from_millis(4)).await });
        serve(&h, &mut packetizer, &mut keyframe_due);
    }
    let last = u64::from(frames * 16_667);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !h.seen.lock().iter().any(|s| s.pts_us == last) {
        assert!(
            Instant::now() < deadline,
            "the last picture never came: {:?}",
            h.handle().stats()
        );
        serve(&h, &mut packetizer, &mut keyframe_due);
        if keyframe_due {
            keyframe_due = false;
            h.route_all(cut(&mut packetizer, &key(frames * 16_667, None)));
        }
        h.rt.block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
    }
    let seen = h.seen.lock().clone();
    assert!(
        seen.windows(2).all(|w| w[0].pts_us < w[1].pts_us),
        "in order"
    );
    let stats = h.handle().stats();
    assert!(
        stats.frames_fec + stats.frames_retransmit > 0,
        "loss was repaired: {stats:?}"
    );
}
