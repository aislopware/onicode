//! Both ends of a client's feedback.
//!
//! On the host, NACKs are answered from the packetizer's history, receiver reports are folded
//! into the bitrate controller and the parity ratio, sound reports into the audio copies, and
//! clock probes are echoed; on the client, the echoes are placed on its clock.
//!
//! Every field is the peer's to choose. A retransmission must be a datagram of the frame asked
//! for, a data fragment of it, marked as a retransmission; the bitrate must stay between the
//! floor and the client's ceiling; the parity ratio and the audio copies must stay within their
//! bounds. A probe's echo must read back as sent, and whatever echoes come, the clock estimate
//! must keep its drift within what it follows and never state a bound tighter than half the
//! fastest round trip.

use std::time::{Duration, Instant};

use arbitrary::{Arbitrary, Unstructured};
use onicode_desktop_media::wire::{
    ClockEcho, Feedback, Kind, MediaHeader, ReceiverReport, SoundReport, StreamId, flags,
};
use onicode_desktop_media::{
    AudioCopies, ClockSync, EncodedFrame, MAX_AUDIO_COPIES, Packetizer, PathSample, RateController,
    Redundancy,
};

const STREAM: StreamId = StreamId(3);

/// The floor under every target (the rate controller's `MIN_BPS`).
const MIN_BPS: u32 = 1_000_000;

#[derive(Arbitrary, Debug)]
struct Plan {
    max_bps: u32,
    parity_permille: u16,
    ops: Vec<Op>,
}

#[derive(Arbitrary, Debug)]
enum Op {
    /// The host sends a frame of `len` bytes.
    Frame { len: u16, keyframe: bool },
    /// A NACK, as its fields.
    Nack { frame: u32, fragments: Vec<u16> },
    /// A feedback datagram, as its bytes: it encodes back to them, and what it asks is done.
    Datagram(Vec<u8>),
    /// A receiver report, with the path the transport sees when it arrives.
    Report {
        report: Report,
        datagrams_sent: u32,
        rtt_ns: Option<(u64, u64)>,
    },
    /// A report of the host's sound, as its fields.
    Sound { received: u16, lost: u16 },
    /// The client's ceiling moves.
    Ceiling(u32),
    /// A media datagram reaches the client `arrived_us` after its clock's epoch: a clock echo
    /// among them is placed on the client's clock.
    Echo { bytes: Vec<u8>, arrived_us: u32 },
    /// An echo's readings, as a host may fill them, reach the client.
    Readings {
        sent_us: u64,
        received_us: u64,
        echoed_us: u64,
        arrived_us: u64,
    },
}

/// [`ReceiverReport`]'s fields, as the client may fill them.
#[derive(Arbitrary, Debug)]
struct Report {
    frames_ok: u32,
    frames_fec: u32,
    frames_lost: u32,
    datagrams_lost: u32,
    last_host_send_ts_us: u32,
    hold_p50_ns: u64,
    hold_p95_ns: u64,
    owd_jitter_ns: u64,
    queue_depth: u8,
    late_frames: u32,
    acked_ltr: [u64; 4],
    acked_ltr_len: u8,
    stalled_ms: u16,
    stalls: u16,
}

impl From<&Report> for ReceiverReport {
    fn from(r: &Report) -> Self {
        Self {
            frames_ok: r.frames_ok,
            frames_fec: r.frames_fec,
            frames_lost: r.frames_lost,
            datagrams_lost: r.datagrams_lost,
            last_host_send_ts_us: r.last_host_send_ts_us,
            hold_p50: Duration::from_nanos(r.hold_p50_ns),
            hold_p95: Duration::from_nanos(r.hold_p95_ns),
            owd_jitter: Duration::from_nanos(r.owd_jitter_ns),
            queue_depth: r.queue_depth,
            late_frames: r.late_frames,
            acked_ltr: r.acked_ltr,
            acked_ltr_len: r.acked_ltr_len,
            stalled_ms: r.stalled_ms,
            stalls: r.stalls,
        }
    }
}

/// Run the plan the fuzzer's bytes describe.
pub fn run(data: &[u8]) {
    let input = Unstructured::new(data);
    let Ok(plan) = Plan::arbitrary_take_rest(input) else {
        return;
    };
    let mut host = Host::new(&plan);
    for op in &plan.ops {
        host.step(op);
    }
}

struct Host {
    packetizer: Packetizer,
    rate: RateController,
    ceiling: u32,
    redundancy: Redundancy,
    audio: AudioCopies,
    /// Sound reports taken, each a 50 ms report period on the audio copies' clock.
    reports: u64,
    /// The client's clock estimate, and the moment its probes are stamped from.
    clock: ClockSync,
    epoch: Instant,
}

impl Host {
    fn new(plan: &Plan) -> Self {
        let epoch = Instant::now();
        let mut packetizer = Packetizer::new(STREAM);
        packetizer.set_parity_permille(plan.parity_permille);
        Self {
            packetizer,
            rate: RateController::new(plan.max_bps),
            ceiling: plan.max_bps.max(MIN_BPS),
            redundancy: Redundancy::default(),
            audio: AudioCopies::default(),
            reports: 0,
            clock: ClockSync::new(epoch),
            epoch,
        }
    }

    fn step(&mut self, op: &Op) {
        match op {
            Op::Frame { len, keyframe } => {
                let data = vec![0xa5; usize::from(*len)];
                let frame = EncodedFrame {
                    data: &data,
                    keyframe: *keyframe,
                    ltr_token: None,
                    ltr_refresh: false,
                    capture_ts_us: 0,
                    discardable: false,
                    stripes: 0,
                };
                let _sent = self.packetizer.packetize(&frame, 0, |_| {});
            }
            Op::Nack { frame, fragments } => self.nack(*frame, fragments),
            Op::Datagram(bytes) => {
                let Some(feedback) = Feedback::decode(bytes) else {
                    return;
                };
                assert!(
                    feedback.encode() == bytes.as_slice(),
                    "{feedback:?} encodes otherwise"
                );
                match feedback {
                    Feedback::Nack {
                        frame, fragments, ..
                    } => self.nack(frame, &fragments),
                    Feedback::Clock { stream, sent_us } => Self::echo(stream, sent_us),
                    Feedback::Report { report, .. } => self.report(&report, 0, None),
                    Feedback::Sound(sound) => self.sound(sound),
                    Feedback::Refresh { .. } => {}
                }
            }
            Op::Echo { bytes, arrived_us } => {
                if let Some((header, payload)) = MediaHeader::parse(bytes)
                    && header.kind() == Some(Kind::Clock)
                    && let Some(echo) = ClockEcho::parse(payload)
                {
                    let (sent, received, echoed) =
                        (echo.sent.get(), echo.received.get(), echo.echoed.get());
                    self.observe(sent, received, echoed, u64::from(*arrived_us));
                }
            }
            Op::Readings {
                sent_us,
                received_us,
                echoed_us,
                arrived_us,
            } => {
                self.observe(*sent_us, *received_us, *echoed_us, *arrived_us);
            }
            Op::Report {
                report,
                datagrams_sent,
                rtt_ns,
            } => {
                let path = rtt_ns.map(|(rtt, cwnd)| PathSample {
                    rtt: Duration::from_nanos(rtt),
                    cwnd,
                });
                self.report(&ReceiverReport::from(report), *datagrams_sent, path);
            }
            Op::Sound { received, lost } => {
                self.sound(SoundReport {
                    received: *received,
                    lost: *lost,
                });
            }
            Op::Ceiling(max) => {
                self.rate.set_max(*max);
                self.ceiling = (*max).max(MIN_BPS);
            }
        }
    }

    /// A receiver report reaches the rate controller and the parity ratio, which stay in bounds.
    fn report(&mut self, report: &ReceiverReport, datagrams_sent: u32, path: Option<PathSample>) {
        if let Some(decision) = self.rate.on_report(report, datagrams_sent, path) {
            assert!(
                (MIN_BPS..=self.ceiling).contains(&decision.target_bps),
                "target {} outside {MIN_BPS}..={}",
                decision.target_bps,
                self.ceiling
            );
        }
        let permille = self.redundancy.on_report(report, datagrams_sent);
        assert!(
            (Redundancy::MIN..=Redundancy::MAX).contains(&permille),
            "parity ratio {permille}‰ outside its bounds"
        );
    }

    /// A sound report reaches the audio copies, which stay in bounds.
    fn sound(&mut self, report: SoundReport) {
        self.reports = self.reports.saturating_add(1);
        let copies = self
            .audio
            .on_report(report, self.reports.saturating_mul(50_000));
        assert!(copies <= MAX_AUDIO_COPIES, "{copies} audio copies");
    }

    /// The host answers a probe, and the client reads the echo back as it was sent.
    fn echo(stream: StreamId, sent_us: u64) {
        let (received, echoed) = (sent_us.wrapping_mul(3), sent_us.wrapping_mul(3) | 1);
        let datagram = ClockEcho::new(sent_us, received, echoed).datagram(stream, 7);
        let Some((header, payload)) = MediaHeader::parse(&datagram) else {
            panic!("an echo that is not a media datagram");
        };
        assert_eq!(
            (header.kind(), header.stream.get()),
            (Some(Kind::Clock), stream.0),
            "an echo's header"
        );
        let Some(echo) = ClockEcho::parse(payload) else {
            panic!("an echo that does not parse")
        };
        assert_eq!(
            (echo.sent.get(), echo.received.get()),
            (sent_us, received),
            "an echo's times"
        );
        assert_eq!(echo.echoed.get(), echoed, "an echo's echo time");
    }

    /// The client takes an echo's readings, and its estimate stays one it can stand behind.
    fn observe(&mut self, sent_us: u64, received_us: u64, echoed_us: u64, arrived_us: u64) {
        let Some(arrived) = self.epoch.checked_add(Duration::from_micros(arrived_us)) else {
            return;
        };
        let (probes, _jumps) = self.clock.counts();
        self.clock.observe(sent_us, received_us, echoed_us, arrived);
        assert!(
            self.clock.counts().0 >= probes,
            "the probes counted went back"
        );
        if let Some(estimate) = self.clock.estimate() {
            assert!(estimate.drift_ppm.unsigned_abs() <= 500, "{estimate:?}");
            let half = estimate.rtt.checked_div(2).unwrap_or_default();
            let least = half.saturating_sub(Duration::from_micros(1));
            assert!(
                estimate.bound >= least,
                "tighter than half the fastest trip: {estimate:?}"
            );
        }
    }

    fn nack(&mut self, frame: u32, fragments: &[u16]) {
        let data_count = self
            .packetizer
            .retransmit(frame, &[])
            .iter()
            .find_map(|d| MediaHeader::parse(d).map(|(h, _)| h.data_count.get()));
        for datagram in self.packetizer.retransmit(frame, fragments) {
            let Some((header, _payload)) = MediaHeader::parse(&datagram) else {
                panic!("a retransmission that is not a media datagram");
            };
            assert_eq!(
                header.frame.get(),
                frame,
                "a retransmission of another frame"
            );
            assert!(
                header.flags & flags::RETRANSMIT != 0,
                "a retransmission not marked as one"
            );
            assert!(
                data_count.is_some_and(|count| header.index.get() < count),
                "a retransmission of a parity fragment or past the frame"
            );
        }
    }
}
