//! Capture to glass: a real desktopd session host on the drawn platform and a real
//! desktop-client session against it, over the simulated tailnet, on a paused clock.
//!
//! The drawn platform runs on the runtime ([`Platform::ON_RUNTIME`]) and the test's decoder
//! decodes in place ([`VideoDecode::INLINE`]), so every capture, every datagram, every NACK and
//! refresh, and every picture put up happens at an instant of one virtual clock, and a seed
//! repeats the run to the packet. The fake encoder writes each picture's capture, number and
//! capture stamp into its bytes ([`synthetic::Coded`]); the decoder here reads them back, so
//! what reaches the glass says which capture it shows and when that capture was drawn.
//!
//! Each run watches one stream for [`WATCH`] under the weather its test sets, typing as it goes,
//! then judges what the person would have seen: every capture coded in the window put up or
//! skipped on purpose, in order; no frame given up on where the network only drops or reorders
//! a few packets; a fresh picture soon after a blackout; the controller's typing reaching the
//! Mac once each, in order, and a viewer's refused; and capture-to-glass latency, its p50, p95
//! and p99, inside bounds derived from the link and the receiver's repair timing.
//!
//! [`Platform::ON_RUNTIME`]: onicode_desktopd::platform::Platform::ON_RUNTIME

use std::collections::BTreeSet;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use onicode_desktop_client::decode::{Outcome, OutcomeSink, Refused};
use onicode_desktop_client::{
    DecodedFrame, DesktopClient, DesktopEndpoint, DesktopSession, Image, Join, Open, Phase,
    Present, Presentable, ScreenHandle, ScreenStats, SessionState, VideoDecode,
};
use onicode_desktop_link::{HostGrants, PORT, WIRE};
use onicode_desktop_media::Config;
use onicode_desktop_media::wire::VideoCodec;
use onicode_desktop_proto::screen::{CaptureTarget, Quality, ScreenInput};
use onicode_desktop_proto::{DisplayId, StreamId};
use onicode_desktopd::session::{self, Host};
use onicode_desktopd::sized;
use onicode_desktopd::synthetic::{self, Coded, Drawn};
use onicode_net::Role;
use onicode_net::transport::endpoint_on;
use onicode_net_sim::tailnet::{ipv4, ipv6};
use onicode_net_sim::{Faults, Link, Net, NodeId, NodeSpec, SimTailnet, report_seed};
use onicode_peer::{PeerConfig, PeerEndpoint, Sockets};
use parking_lot::Mutex;
use tokio::sync::watch;

const USER: i64 = 2;
/// The longest any one step may take, in virtual time.
const PATIENCE: Duration = Duration::from_secs(5);

/// One way across the simulated tailnet: a direct path between two Macs on one network.
const DELAY: Duration = Duration::from_millis(5);
/// There and back.
const RTT: Duration = DELAY.saturating_mul(2);
/// The drawn display's frame period, at its 60 Hz.
const PERIOD: Duration = Duration::from_micros(16_667);
/// The simulated network delivers on whole milliseconds, and tokio's timers fire on them.
const TICK: Duration = Duration::from_millis(1);
/// How long a run watches under its weather.
const WATCH: Duration = Duration::from_secs(2);
/// How long the stream runs on a clear link before the weather turns: the first keyframe is
/// across and the rate has settled.
const WARM: Duration = Duration::from_millis(300);
/// How long the run goes on past the window, so the frames captured at its end are across or
/// given up on, and the receiver has reported since.
const SETTLE: Duration = Duration::from_millis(400);
/// The blackout's length.
const BLACKOUT: Duration = Duration::from_millis(300);
/// How long a reordered packet is held past its time.
const REORDER_BY: Duration = Duration::from_millis(8);
/// Text inputs the controller types over a run, spread evenly across [`WATCH`].
const INPUTS: u32 = 20;
/// noq's probe timeout on this link before any backoff: the smoothed round trip, four times
/// its variance and the peer's ack delay (`onicode_net::transport`'s 2 ms), well inside three
/// round trips.
const PTO: Duration = RTT.saturating_mul(3);

/// Repairing one lost packet, from the capture it belongs to: the next frame's packet shows the
/// hole (one period), the NACK waits at most its longest delay, the repair crosses there and
/// back, and the stream task looks at the hole on its tick.
fn repair() -> Duration {
    let receiver = Config::default();
    PERIOD + receiver.nack_delay.max + RTT + receiver.tick_period + TICK
}

/// From the end of a blackout to a fresh picture on the glass.
///
/// * The probe noq had pending when the link came back fires within one backed-off probe timeout.
///   Each wait doubles the last, so the one spanning the blackout's end is at most the blackout
///   plus the first: [`BLACKOUT`] + [`PTO`].
/// * The receiver gives up on what the blackout ate once its retries are spent: the NACK delay,
///   then each retry a round trip and a NACK delay apart, plus its grace, read on its tick.
/// * Its refresh crosses, the next capture is coded as a keyframe or a long-term reference refresh,
///   the congestion window regrows from its floor (a round trip a doubling, up to four for a
///   keyframe's packets) and the picture crosses.
/// * One refresh repeat, should the first ask land before the host could answer it.
fn recovery() -> Duration {
    let receiver = Config::default();
    let retries = u32::from(receiver.nack_retries);
    let give_up = receiver.nack_delay.max
        + (RTT + receiver.nack_delay.max) * retries
        + receiver.grace
        + receiver.tick_period;
    let keyframe = DELAY + PERIOD + RTT * 4 + DELAY;
    BLACKOUT + PTO + give_up + keyframe + receiver.refresh_repeat
}

/// The simulated network's sockets.
struct SimSockets(Net);

impl Sockets for SimSockets {
    fn endpoint(&self, local: SocketAddr, seed: Option<u64>) -> io::Result<noq::Endpoint> {
        endpoint_on(Box::new(self.0.bind(local)?), true, seed)
    }
}

const ALL: HostGrants = HostGrants {
    screen_recording: true,
    accessibility: true,
    post_event: true,
};

/// A clear link, [`DELAY`] each way.
const fn clear() -> Faults {
    Faults::over(Link {
        delay: DELAY,
        ..Link::CLEAR
    })
}

/// What the network does once the stream is warm, and what happens halfway through.
#[derive(Clone, Copy, Debug)]
struct Weather {
    faults: Faults,
    blackout: bool,
}

impl Weather {
    const BLACKOUT: Self = Self {
        faults: clear(),
        blackout: true,
    };
    const CLEAR: Self = Self {
        faults: clear(),
        blackout: false,
    };

    const fn loss(share: f32) -> Self {
        Self {
            faults: Faults::over(Link {
                delay: DELAY,
                loss: share,
                ..Link::CLEAR
            }),
            blackout: false,
        }
    }

    const fn reorder(share: f32) -> Self {
        Self {
            faults: Faults {
                link: Link {
                    delay: DELAY,
                    ..Link::CLEAR
                },
                reorder: share,
                reorder_by: REORDER_BY,
            },
            blackout: false,
        }
    }
}

/// A drawn picture as the test's decoder made it: what its bytes say.
#[derive(Clone, Copy, Debug)]
struct Shown(Coded);

impl Image for Shown {
    fn width(&self) -> usize {
        usize::try_from(self.0.picture.width).unwrap_or(usize::MAX)
    }

    fn height(&self) -> usize {
        usize::try_from(self.0.picture.height).unwrap_or(usize::MAX)
    }
}

/// Decodes the fake encoder's bytes in place: a keyframe configures it, and every frame after
/// is the picture its bytes name.
struct Glass {
    sink: OutcomeSink<Shown>,
    ready: bool,
}

impl VideoDecode for Glass {
    type Image = Shown;

    const INLINE: bool = true;

    fn start(_codec: VideoCodec, sink: OutcomeSink<Shown>) -> Self {
        Self { sink, ready: false }
    }

    fn decode(&mut self, access_unit: &Bytes, pts_us: u64) -> Result<(), Refused> {
        let coded = Coded::read(access_unit, pts_us).ok_or(Refused { status: None })?;
        self.ready |= coded.keyframe;
        if !self.ready {
            return Err(Refused { status: None });
        }
        (self.sink)(Outcome::Picture(DecodedFrame {
            image: Shown(coded),
            pts_us,
        }));
        Ok(())
    }

    fn ready(&self) -> bool {
        self.ready
    }
}

/// A picture put up, and when ([`synthetic::now_us`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Seen {
    coded: Coded,
    at_us: u64,
}

/// desktopd on the node `mac`.
struct Mac {
    tailnet: SimTailnet,
    node: NodeId,
    _grants: watch::Sender<HostGrants>,
    _host: Arc<Host<Drawn, sized::Cg>>,
    _endpoint: Arc<PeerEndpoint>,
}

async fn mac(seed: u64) -> Mac {
    let tailnet = SimTailnet::new(clear(), seed);
    let node = tailnet.add(NodeSpec::owned("mac", USER));
    let (grants, read) = watch::channel(ALL);
    let host = Host::<Drawn, sized::Cg>::new("mac".to_owned(), read, None);
    let endpoint = session::bind(
        tailnet.local_api(node),
        &SimSockets(tailnet.net().clone()),
        "dev-mac-desktop".to_owned(),
        Some(seed),
    )
    .await
    .expect("desktopd binds its tailnet address");
    tokio::spawn(Arc::clone(&host).serve(endpoint.start()));
    Mac {
        tailnet,
        node,
        _grants: grants,
        _host: host,
        _endpoint: endpoint,
    }
}

/// One of the owner's devices, its desktop session linked to the Mac's desktopd.
struct Viewer {
    node: NodeId,
    session: DesktopSession<Glass>,
}

/// One stream on a viewer, and what its glass put up.
struct Screen {
    stream: StreamId,
    handle: ScreenHandle<Shown>,
    glass: Arc<Mutex<Vec<Seen>>>,
}

impl Mac {
    /// `name` joins, asking for the controls when `control`.
    async fn viewer(&self, name: &str, seed: u64, control: bool) -> Viewer {
        let node = self.tailnet.add(NodeSpec::owned(name, USER));
        let client = DesktopClient::bind_on(
            self.tailnet.local_api(node),
            &SimSockets(self.tailnet.net().clone()),
            PeerConfig::new(format!("dev-{name}"), Role::Viewer).with_seed(seed),
        )
        .await
        .expect("the viewer binds");
        let session = client.connect_with::<Glass>(
            DesktopEndpoint {
                device_id: "dev-mac-desktop".to_owned(),
                host: "mac".to_owned(),
                addrs: vec![SocketAddr::new(ipv4(self.node), PORT)],
                wire: WIRE,
                grants: ALL,
                displays: synthetic::DISPLAYS.to_vec(),
            },
            Join {
                name: name.to_owned(),
                control,
                sound: false,
            },
        );
        let viewer = Viewer { node, session };
        viewer.until(|s| s.phase == Phase::Joined).await;
        viewer
    }

    /// Packets between the Mac and `viewer` run over `faults` from now on.
    fn weather(&self, viewer: &Viewer, faults: Faults) {
        let net = self.tailnet.net();
        net.set_path(ipv4(self.node), ipv4(viewer.node), faults);
        net.set_path(ipv6(self.node), ipv6(viewer.node), faults);
    }
}

impl Viewer {
    /// The session's state once `wanted` holds of it.
    async fn until(&self, wanted: impl Fn(&SessionState) -> bool) -> Arc<SessionState> {
        let mut state = self.session.state();
        let state = tokio::time::timeout(PATIENCE, state.wait_for(|s| wanted(s)))
            .await
            .expect("the session gets there in time")
            .expect("the session runs");
        Arc::clone(&*state)
    }

    /// Open display 1 and watch its glass from its first picture on.
    async fn open(&self) -> Screen {
        let opened = self.session.state().borrow().streams.len();
        self.session
            .open(Open::Display(DisplayId(1)), Quality::default());
        let state = self.until(|s| s.streams.len() > opened).await;
        let view = state.streams.last().expect("a stream is open");
        assert_eq!(view.display, Some(DisplayId(1)));
        let stream = view.stream;
        let handle = tokio::time::timeout(PATIENCE, async {
            loop {
                if let Some(handle) = self.session.screen(stream) {
                    return handle;
                }
                tokio::time::sleep(TICK).await;
            }
        })
        .await
        .expect("the stream reaches the glass");
        let glass = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&glass);
        let present: Present<Shown> = Arc::new(move |picture: &Presentable<Shown>| {
            log.lock().push(Seen {
                coded: picture.frame.image.0,
                at_us: synthetic::now_us(),
            });
        });
        handle.set_present(Some(present));
        let screen = Screen {
            stream,
            handle,
            glass,
        };
        tokio::time::timeout(PATIENCE, async {
            while screen.glass.lock().is_empty() {
                tokio::time::sleep(TICK).await;
            }
        })
        .await
        .expect("a first picture reaches the glass");
        screen
    }

    fn type_text(&self, screen: &Screen, tag: &str, i: u32) {
        self.session.input(screen.stream, typed(tag, i));
    }
}

fn typed(tag: &str, i: u32) -> ScreenInput {
    ScreenInput::SmartMagnify {
        x: mark(tag),
        y: f32::from_bits(numbered(i)),
    }
}

/// `tag` as the x of the taps its inputs are: each tag its own, the same every time.
fn mark(tag: &str) -> f32 {
    let mark = tag.bytes().fold(0_u16, |mark, byte| {
        mark.wrapping_mul(31).wrapping_add(u16::from(byte))
    });
    f32::from(mark)
}

/// Input `i` as the bits of the y its tap carries.
fn numbered(i: u32) -> u32 {
    f32::from(u16::try_from(i).unwrap_or(u16::MAX)).to_bits()
}

/// What the drawn Mac was tapped with by `tag`, in order, and on which display: each tap's y,
/// as its bits.
fn typed_so_far(tag: &str) -> Vec<(CaptureTarget, u32)> {
    let mark = mark(tag).to_bits();
    synthetic::recorded()
        .into_iter()
        .filter_map(|(target, input)| match input {
            ScreenInput::SmartMagnify { x, y } if x.to_bits() == mark => {
                Some((target, y.to_bits()))
            }
            _other => None,
        })
        .collect()
}

/// `tag`'s `count` inputs reached display 1, each once and in order.
async fn assert_typed(tag: &str, count: u32) {
    let typed = tokio::time::timeout(PATIENCE, async {
        loop {
            let typed = typed_so_far(tag);
            if typed.len() >= usize::try_from(count).unwrap_or(usize::MAX) {
                return typed;
            }
            tokio::time::sleep(PERIOD).await;
        }
    })
    .await
    .unwrap_or_else(|_elapsed| typed_so_far(tag));
    let wanted: Vec<_> = (0..count)
        .map(|i| (CaptureTarget::Display(DisplayId(1)), numbered(i)))
        .collect();
    assert_eq!(
        typed, wanted,
        "{tag}'s typing reached the Mac once each, in order"
    );
}

/// What one run saw.
#[derive(Debug)]
struct Run {
    /// The glass, from the first picture on.
    seen: Vec<Seen>,
    /// What the host coded for this stream's capture.
    coded: Vec<Coded>,
    /// Captures drawn from here...
    from_us: u64,
    /// ...to here are judged.
    to_us: u64,
    /// When the blackout ended, if there was one.
    blackout_end_us: Option<u64>,
    /// The receiver's counters as the window opened, and after it settled.
    before: ScreenStats,
    after: ScreenStats,
    /// Every delivery the network made.
    digest: u64,
}

/// The laptop takes the controls, opens display 1, and watches it for [`WATCH`] under
/// `weather`, typing [`INPUTS`] texts as it goes.
async fn watch(seed: u64, weather: Weather) -> Run {
    let mac = mac(seed).await;
    let laptop = mac.viewer("laptop", seed.wrapping_add(1), true).await;
    let screen = laptop.open().await;
    tokio::time::sleep(WARM).await;
    let run = watch_screen(&mac, &laptop, &screen, weather).await;
    assert_typed("laptop", INPUTS).await;
    run
}

async fn watch_screen(mac: &Mac, laptop: &Viewer, screen: &Screen, weather: Weather) -> Run {
    mac.weather(laptop, weather.faults);
    let before = screen.handle.stats();
    let from_us = synthetic::now_us();
    let mut blackout_end_us = None;
    for i in 0..INPUTS {
        if weather.blackout && i == INPUTS / 2 {
            mac.tailnet.net().blackout(BLACKOUT);
            blackout_end_us = Some(synthetic::now_us() + micros(BLACKOUT));
        }
        laptop.type_text(screen, "laptop", i);
        tokio::time::sleep(WATCH / INPUTS).await;
    }
    let to_us = synthetic::now_us();
    tokio::time::sleep(SETTLE).await;
    finish(
        screen,
        from_us,
        to_us,
        blackout_end_us,
        before,
        mac.tailnet.net(),
    )
}

fn finish(
    screen: &Screen,
    from_us: u64,
    to_us: u64,
    blackout_end_us: Option<u64>,
    before: ScreenStats,
    net: &Net,
) -> Run {
    let seen = screen.glass.lock().clone();
    let capture = seen
        .first()
        .expect("pictures were put up")
        .coded
        .picture
        .capture;
    let coded = synthetic::coded()
        .into_iter()
        .filter(|c| c.picture.capture == capture)
        .collect();
    Run {
        seen,
        coded,
        from_us,
        to_us,
        blackout_end_us,
        before,
        after: screen.handle.stats(),
        digest: net.digest(),
    }
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// What the person saw of a run, judged.
#[derive(Debug)]
struct Judged {
    /// Pictures of captures drawn in the window, first showing each.
    shown: BTreeSet<u64>,
    /// Captures coded in the window that never reached the glass.
    missing: BTreeSet<u64>,
    /// Captures in the window the host never coded: dropped on purpose, by its pacing or for a
    /// congestion window with no room.
    uncoded: BTreeSet<u64>,
    /// Captures in the window.
    drawn: u64,
    /// Frames the receiver gave up on in the window.
    lost: u64,
    /// Pictures skipped on purpose in the window: frames nothing refers to, and pictures the
    /// pacer dropped as stale.
    skipped: u64,
    /// Capture to glass, first showing of each capture, microseconds, ascending.
    latency: Vec<u64>,
}

impl Run {
    /// Judge the captures drawn in `from_us..=to_us` (the whole window by default).
    fn judge_between(&self, from_us: u64, to_us: u64) -> Judged {
        let capture = self.seen[0].coded.picture.capture;
        // In order: one capture, each picture newer than the one before it.
        for pair in self.seen.windows(2) {
            let [a, b] = pair else { unreachable!() };
            assert_eq!(b.coded.picture.capture, capture, "one capture on the glass");
            assert!(
                b.coded.pts_us > a.coded.pts_us,
                "pictures go up in order: {a:?} then {b:?}"
            );
            assert!(
                b.coded.picture.seq >= a.coded.picture.seq,
                "never an older capture after a newer one: {a:?} then {b:?}"
            );
            assert!(b.at_us >= a.at_us);
        }
        let inside = |c: &Coded| (from_us..=to_us).contains(&c.picture.captured_us);
        let coded: BTreeSet<u64> = self
            .coded
            .iter()
            .filter(|c| inside(c))
            .map(|c| c.picture.seq)
            .collect();
        let mut shown = BTreeSet::new();
        let mut latency = Vec::new();
        for seen in self.seen.iter().filter(|s| inside(&s.coded)) {
            if shown.insert(seen.coded.picture.seq) {
                latency.push(seen.at_us.saturating_sub(seen.coded.picture.captured_us));
            }
        }
        latency.sort_unstable();
        assert!(
            shown.is_subset(&coded),
            "the glass shows only what was coded"
        );
        let missing = coded.difference(&shown).copied().collect();
        let (first, last) = (
            coded.first().copied().unwrap_or(0),
            coded.last().copied().unwrap_or(0),
        );
        let every: BTreeSet<u64> = self.coded.iter().map(|c| c.picture.seq).collect();
        let uncoded = (first..=last).filter(|s| !every.contains(s)).collect();
        let (before, after) = (&self.before, &self.after);
        Judged {
            shown,
            missing,
            uncoded,
            drawn: last.saturating_sub(first).saturating_add(1),
            lost: after.frames_lost.saturating_sub(before.frames_lost),
            skipped: after.frames_skipped.saturating_sub(before.frames_skipped)
                + after.pacing.skipped.saturating_sub(before.pacing.skipped)
                + after.pacing.late.saturating_sub(before.pacing.late),
            latency,
        }
    }

    fn judge(&self) -> Judged {
        self.judge_between(self.from_us, self.to_us)
    }

    /// The relative glass: each picture's number and its capture and showing instants from the
    /// window's start, for comparing two runs.
    fn relative(&self) -> Vec<(u64, u64, u64)> {
        self.seen
            .iter()
            .filter(|s| s.at_us >= self.from_us)
            .map(|s| {
                (
                    s.coded.picture.seq,
                    s.coded.picture.captured_us.wrapping_sub(self.from_us),
                    s.at_us - self.from_us,
                )
            })
            .collect()
    }
}

impl Judged {
    /// The `p`th percentile (nearest rank) of the latencies.
    fn percentile(&self, p: u64) -> Duration {
        let n = u64::try_from(self.latency.len()).unwrap_or(u64::MAX);
        assert!(n > 0, "no pictures to time");
        let rank = (p * n).div_ceil(100).clamp(1, n) - 1;
        Duration::from_micros(self.latency[usize::try_from(rank).unwrap_or(usize::MAX)])
    }

    /// The latency's p50, p95 and p99 are at most these.
    fn assert_latency(&self, p50: Duration, p95: Duration, p99: Duration) {
        let seen = (
            self.percentile(50),
            self.percentile(95),
            self.percentile(99),
        );
        assert!(
            seen.0 <= p50 && seen.1 <= p95 && seen.2 <= p99,
            "capture to glass p50/p95/p99 {seen:?}, bounds {:?}",
            (p50, p95, p99)
        );
    }

    /// Every capture coded in the window went up, or was skipped on purpose.
    fn assert_accounted(&self) {
        assert!(
            u64::try_from(self.missing.len()).unwrap_or(u64::MAX) <= self.skipped + self.lost,
            "captures coded and never shown, {:?}, beyond the {} skipped and {} lost",
            self.missing,
            self.skipped,
            self.lost
        );
    }

    /// At most a tenth of the window's captures were left uncoded, and at least nine in ten
    /// reached the glass.
    fn assert_most_shown(&self) {
        let shown = u64::try_from(self.shown.len()).unwrap_or(0);
        let uncoded = u64::try_from(self.uncoded.len()).unwrap_or(u64::MAX);
        assert!(
            uncoded * 10 <= self.drawn,
            "{uncoded} of {} captures never coded: {:?}",
            self.drawn,
            self.uncoded
        );
        assert!(
            shown * 10 >= self.drawn * 9,
            "{shown} of {} captures shown",
            self.drawn
        );
    }
}

/// On a clear link every capture is coded and put up, none skipped or lost, a few milliseconds
/// after it was drawn, and the controller's typing reaches the Mac in order.
#[tokio::test(start_paused = true)]
async fn a_clear_link_shows_every_capture() {
    const SEED: u64 = 81;
    let _seed = report_seed("a_clear_link_shows_every_capture", SEED);
    let run = watch(SEED, Weather::CLEAR).await;
    let judged = run.judge();
    assert!(
        judged.missing.is_empty(),
        "never shown: {:?}",
        judged.missing
    );
    assert!(
        judged.uncoded.is_empty(),
        "never coded: {:?}",
        judged.uncoded
    );
    assert_eq!((judged.lost, judged.skipped), (0, 0));
    // Two seconds of a 60 Hz display: one capture every period, give or take the timers'
    // millisecond.
    let periods = micros(WATCH) / micros(PERIOD);
    assert!(
        judged.drawn + 2 >= periods,
        "{} captures in {WATCH:?}",
        judged.drawn
    );
    // One way across, plus the network's and the timers' millisecond; a keyframe's packets
    // spread by the sender's pacing are inside one period.
    judged.assert_latency(DELAY + TICK * 2, DELAY + TICK * 4, DELAY + PERIOD);
}

/// With 2% of packets lost each way, every loss is repaired: no frame given up on, every coded
/// capture put up or skipped on purpose, and the typing arrives in order. Latency stays one
/// way across for most pictures; the repaired ones and those waiting behind them pay one
/// repair, rarely two.
#[tokio::test(start_paused = true)]
async fn two_percent_loss_is_repaired() {
    const SEED: u64 = 82;
    let _seed = report_seed("two_percent_loss_is_repaired", SEED);
    let run = watch(SEED, Weather::loss(0.02)).await;
    let judged = run.judge();
    assert_eq!(judged.lost, 0, "no unrecovered loss");
    judged.assert_accounted();
    judged.assert_most_shown();
    judged.assert_latency(DELAY + TICK * 2, DELAY + repair(), DELAY + repair() * 2);
}

/// With 5% of packets held back by 8 ms and overtaken, nothing is lost: the receiver waits for
/// them or NACKs them and takes whichever copy comes first.
#[tokio::test(start_paused = true)]
async fn reordering_loses_nothing() {
    const SEED: u64 = 83;
    let _seed = report_seed("reordering_loses_nothing", SEED);
    let run = watch(SEED, Weather::reorder(0.05)).await;
    let judged = run.judge();
    assert_eq!(judged.lost, 0, "no unrecovered loss");
    judged.assert_accounted();
    judged.assert_most_shown();
    judged.assert_latency(
        DELAY + TICK * 2,
        DELAY + REORDER_BY + TICK * 2,
        DELAY + repair(),
    );
}

/// 300 ms of nothing either way, mid-stream: the glass shows a capture drawn after the link came
/// back within [`recovery`], everything coded after that goes up, and the typing sent into the
/// blackout still arrives once each, in order.
#[tokio::test(start_paused = true)]
async fn a_blackout_ends_in_a_fresh_picture() {
    const SEED: u64 = 84;
    let _seed = report_seed("a_blackout_ends_in_a_fresh_picture", SEED);
    let run = watch(SEED, Weather::BLACKOUT).await;
    let end = run.blackout_end_us.expect("the blackout happened");
    let start = end - micros(BLACKOUT);

    let fresh = run
        .seen
        .iter()
        .find(|s| s.coded.picture.captured_us >= end)
        .expect("a capture drawn after the blackout reached the glass");
    let waited = Duration::from_micros(fresh.at_us - end);
    assert!(
        waited <= recovery(),
        "a fresh picture {waited:?} after the link came back, more than {:?}: {fresh:?}",
        recovery()
    );

    let before = run.judge_between(run.from_us, start.saturating_sub(micros(repair() * 2)));
    assert!(before.missing.is_empty(), "before it: {:?}", before.missing);
    before.assert_latency(DELAY + TICK * 2, DELAY + TICK * 4, DELAY + PERIOD);

    let recovered = end + micros(recovery());
    let after = run.judge_between(recovered, run.to_us);
    assert!(
        after.missing.is_empty(),
        "coded after it recovered and never shown: {:?}",
        after.missing
    );
    after.assert_most_shown();
    after.assert_latency(DELAY + TICK * 2, DELAY + repair(), DELAY + repair() * 2);
}

/// The laptop holds the controls when the phone joins and opens the same display: the phone is
/// told who holds them, its typing is refused and never reaches the Mac, the laptop's still
/// does, in order, and the laptop's picture carries on through the join without a capture lost.
#[tokio::test(start_paused = true)]
async fn a_second_viewer_watches_while_the_first_drives() {
    const SEED: u64 = 85;
    let _seed = report_seed("a_second_viewer_watches_while_the_first_drives", SEED);
    let mac = mac(SEED).await;
    let laptop = mac.viewer("laptop", SEED + 1, true).await;
    let on_laptop = laptop.open().await;
    tokio::time::sleep(WARM).await;
    let before = on_laptop.handle.stats();
    let from_us = synthetic::now_us();
    for i in 0..INPUTS / 2 {
        laptop.type_text(&on_laptop, "laptop", i);
        tokio::time::sleep(WATCH / INPUTS).await;
    }

    let phone = mac.viewer("phone", SEED + 2, false).await;
    let seat = phone.until(|s| s.seat.clients == 2).await.seat.clone();
    assert!(!seat.you, "the phone did not ask for the controls");
    assert_eq!(seat.controller.as_deref(), Some("laptop"));
    let seat = laptop.until(|s| s.seat.clients == 2).await.seat.clone();
    assert!(seat.you, "the laptop keeps them");
    let on_phone = phone.open().await;
    phone.type_text(&on_phone, "phone", 0);
    let refused = phone.until(|s| s.input_refused).await;
    assert!(!refused.seat.you);

    for i in INPUTS / 2..INPUTS {
        laptop.type_text(&on_laptop, "laptop", i);
        phone.type_text(&on_phone, "phone", i);
        tokio::time::sleep(WATCH / INPUTS).await;
    }
    let to_us = synthetic::now_us();
    tokio::time::sleep(SETTLE).await;
    assert_typed("laptop", INPUTS).await;
    assert!(
        typed_so_far("phone").is_empty(),
        "the viewer's typing never reached the Mac"
    );
    assert!(
        !laptop.session.state().borrow().input_refused,
        "the controller was never refused"
    );

    let run = finish(&on_laptop, from_us, to_us, None, before, mac.tailnet.net());
    let judged = run.judge();
    assert!(
        judged.missing.is_empty(),
        "never shown: {:?}",
        judged.missing
    );
    assert_eq!(judged.lost, 0);
    judged.assert_most_shown();
    judged.assert_latency(DELAY + TICK * 2, DELAY + TICK * 4, DELAY + PERIOD);

    // The phone's own capture of the same display, put up in order.
    let phone_run = finish(
        &on_phone,
        synthetic::now_us().saturating_sub(micros(WATCH)),
        to_us,
        None,
        on_phone.handle.stats(),
        mac.tailnet.net(),
    );
    assert_ne!(
        phone_run.seen[0].coded.picture.capture, run.seen[0].coded.picture.capture,
        "each viewer's stream is a capture of its own"
    );
    let _in_order = phone_run.judge();
}

/// The same seed is the same run: every delivery at the same instant, the same pictures put up
/// at the same instants and the same typing, each run on a runtime and a drawn Mac of its own.
#[test]
fn a_seed_repeats_its_glass() {
    const SEED: u64 = 86;
    let _seed = report_seed("a_seed_repeats_its_glass", SEED);
    let once = || {
        std::thread::spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .start_paused(true)
                .build()
                .expect("a runtime");
            let run = runtime.block_on(watch(SEED, Weather::loss(0.02)));
            (run.digest, run.relative(), typed_so_far("laptop"))
        })
        .join()
        .expect("the run finishes")
    };
    let (first, second) = (once(), once());
    assert_eq!(
        first.0, second.0,
        "the network delivered the same packets at the same times"
    );
    assert_eq!(
        first.1, second.1,
        "the glass showed the same pictures at the same times"
    );
    assert_eq!(first.2, second.2);
}
