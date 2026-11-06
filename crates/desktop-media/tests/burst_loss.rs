//! Parity against loss that comes in clumps.
//!
//! A simulated link loses datagrams the way Wi-Fi and a congested queue do, several in a row
//! (a Gilbert channel: a good state that delivers, a bad one that drops, a mean burst length).
//! The host's packetizer and redundancy controller sit on one end and the client's
//! reassembler on the other; NACKs, refresh requests and receiver reports travel back over the
//! same one-way delay, and retransmissions cross the same lossy link. What it counts is what a
//! viewer pays for a loss (a frame that waited a round trip for a retransmission, a frame given
//! up on, a refresh, the time the picture stood still) and what parity costs on the wire.
//! One clock drives both ends in 1 ms steps, so a run repeats exactly and takes no wall time.
//!
//! The same link prices temporal layers (`layers_under_burst_loss`): the encoder writes every
//! other frame as one nothing refers to, at the sizes the host's session gives them, and the
//! receiver either treats it as any other frame or skips it when it cannot repair it.
//!
//! ```text
//! cargo nextest run -p onicode-desktop-media --release --run-ignored only -E 'test(parity_under_burst_loss)' --no-capture
//! cargo nextest run -p onicode-desktop-media --release --run-ignored only -E 'test(layers_under_burst_loss)' --no-capture
//! ```
//!
//! MEASUREMENTS ("two parity fragments on small frames", "temporal layers under
//! clumped loss") records runs.

#[cfg(test)]
mod tests {
    #![expect(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        reason = "simulation arithmetic on small, bounded counts"
    )]

    use std::collections::VecDeque;
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use onicode_desktop_media::wire::{ReceiverReport, StreamId};
    use onicode_desktop_media::{
        Action, Config, EncodedFrame, Packetizer, Reassembler, Redundancy, layout,
    };

    const STREAM: StreamId = StreamId(7);
    /// One-way delay; the round trip is twice this.
    const ONE_WAY_US: u64 = 10_000;
    const RTT: Duration = Duration::from_micros(2 * ONE_WAY_US);
    /// 60 frames a second.
    const FRAME_US: u64 = 16_667;
    /// The client's receiver-report cadence.
    const REPORT_US: u64 = 50_000;
    /// The largest payload a datagram carries on a 1200-byte path.
    const PAYLOAD: usize = 1_168;

    /// xorshift64*: a fixed sequence per seed.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        /// Uniform in `0.0..1.0`.
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1_u64 << 53) as f64
        }

        fn between(&mut self, low: usize, high: usize) -> usize {
            low + (self.next() % (high - low + 1) as u64) as usize
        }
    }

    /// A Gilbert channel: every datagram in the bad state is lost. `loss` is the long-run share
    /// lost and `burst` the mean run of losses (1 is independent loss).
    struct Gilbert {
        bad: bool,
        enter: f64,
        leave: f64,
        rng: Rng,
    }

    impl Gilbert {
        fn new(loss: f64, burst: f64, seed: u64) -> Self {
            let leave = 1.0 / burst;
            Self {
                bad: false,
                enter: loss * leave / (1.0 - loss),
                leave,
                rng: Rng(seed),
            }
        }

        fn drops(&mut self) -> bool {
            let flip = if self.bad { self.leave } else { self.enter };
            if self.rng.unit() < flip {
                self.bad = !self.bad;
            }
            self.bad
        }
    }

    /// What goes back from the client to the host.
    enum Back {
        Action(Action),
        Report(ReceiverReport),
    }

    /// Frame sizes a picture makes: `(least, most)` bytes of bitstream.
    #[derive(Clone, Copy)]
    struct Scene {
        name: &'static str,
        bytes: (usize, usize),
    }

    /// How the stream uses temporal layers.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Layers {
        /// Every frame a reference, as the host's session is without layers.
        Off,
        /// Every other frame one nothing refers to, but not said on the wire: the receiver
        /// treats it as any other frame.
        Unflagged,
        /// Every other frame one nothing refers to, flagged, and skipped when it is not repaired.
        Flagged,
    }

    /// What layers do to frame sizes on the host's session at 1080p, switched on once it has
    /// run a while as the host switches them (MEASUREMENTS, "temporal layers switched on a
    /// live session": 16.9 KB base and 5.5 KB layer-1 frames against 12.2 KB without layers),
    /// in thousandths: base frames and layer-1 frames.
    const BASE_SIZE: usize = 1_378;
    const LAYER1_SIZE: usize = 450;

    /// A still window: a caret and a few cells change, 1 to 5 fragments a frame.
    const STILL: Scene = Scene {
        name: "still",
        bytes: (600, 5_500),
    };
    /// Text scrolling at 1080p (MEASUREMENTS "120 fps against 60": 14 KiB a frame at 60).
    const SCROLL: Scene = Scene {
        name: "scroll",
        bytes: (8_000, 20_000),
    };

    #[derive(Debug, Default)]
    struct Outcome {
        frames: u64,
        shown: u64,
        fec: u64,
        retransmitted: u64,
        lost: u64,
        refreshes: u64,
        data: u64,
        parity: u64,
        resent: u64,
        /// Bytes of every datagram sent, parity and retransmissions included.
        wire: u64,
        /// Time the picture stood still past one frame interval, summed over the run, ms.
        frozen_ms: u64,
        /// The longest such stretch, ms.
        worst_ms: u64,
        /// Frames shown more than a frame interval after their first fragment arrived.
        late: u64,
        /// Frames nothing refers to that were skipped.
        skipped: u64,
        /// Refreshes asked for after a loss (repeats of the same request not counted).
        episodes: u64,
        /// Of those, the ones whose lost frame was one nothing refers to: the frame after the
        /// last one delivered. Only read without the flag; with it that frame may have been
        /// skipped, and what refreshes is the loss of a later one.
        episodes_layer1: u64,
    }

    impl Outcome {
        fn add(&mut self, o: &Self) {
            self.frames += o.frames;
            self.shown += o.shown;
            self.fec += o.fec;
            self.retransmitted += o.retransmitted;
            self.lost += o.lost;
            self.refreshes += o.refreshes;
            self.data += o.data;
            self.parity += o.parity;
            self.resent += o.resent;
            self.wire += o.wire;
            self.frozen_ms += o.frozen_ms;
            self.worst_ms = self.worst_ms.max(o.worst_ms);
            self.late += o.late;
            self.skipped += o.skipped;
            self.episodes += o.episodes;
            self.episodes_layer1 += o.episodes_layer1;
        }
    }

    /// Stream `seconds` of `scene` at 60 fps over the channel; the parity ratio follows the
    /// receiver's reports through [`Redundancy`], as it does on the host.
    fn run(
        scene: Scene,
        layers: Layers,
        loss: f64,
        burst: f64,
        seconds: u64,
        seed: u64,
    ) -> Outcome {
        let epoch = Instant::now();
        let at = |us: u64| epoch + Duration::from_micros(us);
        let mut tx = Packetizer::new(STREAM);
        tx.set_max_datagram(PAYLOAD + 32);
        let mut redundancy = Redundancy::new();
        tx.set_parity_permille(redundancy.permille());
        let mut rx = Reassembler::new(STREAM, Config::default(), epoch);
        let mut channel = Gilbert::new(loss, burst, seed);
        let mut sizes = Rng(seed ^ 0x9e37_79b9_7f4a_7c15);
        let mut forward: VecDeque<(u64, Bytes)> = VecDeque::new();
        let mut back: VecDeque<(u64, Back)> = VecDeque::new();
        let mut out = Outcome::default();
        let (mut next_frame, mut next_report) = (0_u64, REPORT_US);
        let (mut keyframe, mut refresh) = (true, false);
        let mut sent_since_report = 0_u32;
        let mut last_shown: Option<u64> = None;
        // Which frames nothing refers to, by number, and the last refresh request's frame.
        let mut layer1: Vec<bool> = Vec::new();
        let mut last_request: Option<u32> = None;
        let end = seconds * 1_000_000;
        let mut send = |now: u64, datagram: &Bytes, forward: &mut VecDeque<(u64, Bytes)>| {
            if !channel.drops() {
                forward.push_back((now + ONE_WAY_US, datagram.clone()));
            }
            datagram.len() as u64
        };
        let mut now = 0_u64;
        while now < end {
            // The host: feedback that has arrived, then the next frame when it is due.
            while back.front().is_some_and(|(t, _)| *t <= now) {
                let Some((_, message)) = back.pop_front() else {
                    break;
                };
                match message {
                    Back::Action(Action::Nack { frame, fragments }) => {
                        for datagram in tx.retransmit(frame, &fragments) {
                            out.resent += 1;
                            out.wire += send(now, &datagram, &mut forward);
                            sent_since_report += 1;
                        }
                    }
                    Back::Action(Action::RequestRefresh { keyframe: key, .. }) => {
                        keyframe |= key;
                        refresh = true;
                    }
                    Back::Report(report) => {
                        let permille = redundancy.on_report(&report, sent_since_report);
                        tx.set_parity_permille(permille);
                        sent_since_report = 0;
                    }
                }
            }
            if now >= next_frame {
                // The encoder alternates layers by its own count of frames; a keyframe or a
                // refresh takes its slot but is never one nothing refers to.
                let slot_layer1 = layers != Layers::Off && tx.next_frame() % 2 == 1;
                let is_layer1 = slot_layer1 && !keyframe && !refresh;
                let mut drawn = || {
                    let drawn = sizes.between(scene.bytes.0, scene.bytes.1);
                    match layers {
                        Layers::Off => drawn,
                        Layers::Unflagged | Layers::Flagged if is_layer1 => {
                            drawn * LAYER1_SIZE / 1000
                        }
                        Layers::Unflagged | Layers::Flagged => drawn * BASE_SIZE / 1000,
                    }
                };
                let size = if keyframe {
                    60_000
                } else if refresh {
                    3 * drawn()
                } else {
                    drawn()
                };
                layer1.push(is_layer1);
                let data = vec![(now % 251) as u8; size];
                let frame = EncodedFrame {
                    data: &data,
                    keyframe,
                    ltr_token: keyframe.then_some(1),
                    ltr_refresh: refresh && !keyframe,
                    discardable: is_layer1 && layers == Layers::Flagged,
                    capture_ts_us: now as u32,
                    stripes: 0,
                };
                let stamp = ((now / 1_000) % 256) as u8;
                let sent = tx
                    .packetize(&frame, stamp, |_| {})
                    .expect("packetize")
                    .clone();
                out.frames += 1;
                out.data += u64::from(sent.layout.data_count);
                out.parity += u64::from(sent.layout.parity_count);
                for datagram in &sent.datagrams {
                    out.wire += send(now, datagram, &mut forward);
                    sent_since_report += 1;
                }
                (keyframe, refresh) = (false, false);
                next_frame += FRAME_US;
            }
            // The client: arrivals, the policy's timers, a report on its cadence.
            while forward.front().is_some_and(|(t, _)| *t <= now) {
                let Some((_, datagram)) = forward.pop_front() else {
                    break;
                };
                let _ingest = rx.ingest(&datagram, at(now));
            }
            while let Some(frame) = rx.next_frame() {
                out.shown += 1;
                if frame.hold > Duration::from_micros(FRAME_US) {
                    out.late += 1;
                }
                if let Some(last) = last_shown {
                    let still = (now - last).saturating_sub(FRAME_US) / 1_000;
                    if now - last > 2 * FRAME_US {
                        out.frozen_ms += still;
                        out.worst_ms = out.worst_ms.max(still);
                    }
                }
                last_shown = Some(now);
            }
            for action in rx.tick(at(now), RTT) {
                if let Action::RequestRefresh {
                    last_good_frame,
                    keyframe: false,
                } = action
                    && out.shown > 0
                    && last_request != Some(last_good_frame)
                {
                    last_request = Some(last_good_frame);
                    out.episodes += 1;
                    let lost = last_good_frame as usize + 1;
                    if layer1.get(lost).copied().unwrap_or(false) {
                        out.episodes_layer1 += 1;
                    }
                }
                back.push_back((now + ONE_WAY_US, Back::Action(action)));
            }
            if now >= next_report {
                back.push_back((now + ONE_WAY_US, Back::Report(rx.take_report(at(now), 0))));
                next_report += REPORT_US;
            }
            now += 1_000;
        }
        let stats = rx.stats();
        out.fec = stats.frames_fec;
        out.retransmitted = stats.frames_retransmit;
        out.lost = stats.frames_lost;
        out.refreshes = stats.refreshes;
        out.skipped = stats.frames_skipped;
        out
    }

    /// Every scene at 1, 3 and 5 % loss in bursts of 1, 2 and 4, a minute each over three
    /// seeds. Prints one `MEASURE` line per case; asserts only that the simulation ran.
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture"]
    fn parity_under_burst_loss() {
        let floor = |permille| layout(1, permille, PAYLOAD).map_or(0, |l| l.parity_count);
        eprintln!(
            "MEASURE burst parity floor: {} at {}‰, {} above it",
            floor(Redundancy::MIN),
            Redundancy::MIN,
            floor(Redundancy::MIN + 1)
        );
        for scene in [STILL, SCROLL] {
            for loss in [0.0, 0.01, 0.03, 0.05] {
                for burst in [1.0, 2.0, 4.0] {
                    if loss == 0.0 && burst > 1.0 {
                        continue;
                    }
                    let mut sum = Outcome::default();
                    for seed in [1, 2, 3] {
                        let o = run(
                            scene,
                            Layers::Off,
                            loss,
                            burst,
                            60,
                            seed * 0x1234_5678_9abc_def1,
                        );
                        sum.add(&o);
                    }
                    assert!(
                        sum.shown > 0,
                        "{scene_name}: nothing shown",
                        scene_name = scene.name
                    );
                    eprintln!(
                        "MEASURE burst scene={} loss={:.0}% burst={burst} frames={} shown={} \
                         fec={} retransmitted={} late={} lost={} refreshes={} frozen={}ms \
                         worst={}ms parity/data={:.1}% resent={} wire={:.2}Mbit/s",
                        scene.name,
                        loss * 100.0,
                        sum.frames,
                        sum.shown,
                        sum.fec,
                        sum.retransmitted,
                        sum.late,
                        sum.lost,
                        sum.refreshes,
                        sum.frozen_ms,
                        sum.worst_ms,
                        sum.parity as f64 * 100.0 / sum.data.max(1) as f64,
                        sum.resent,
                        sum.wire as f64 * 8.0 / 180.0 / 1e6,
                    );
                }
            }
        }
    }

    /// Temporal layers against none, at every scene and loss of the parity measurement plus
    /// 10 %, a minute each over three seeds (the same seeds, so the same channel). Prints one
    /// `MEASURE` line per case and mode; asserts only that the simulation ran.
    #[test]
    #[ignore = "a measurement; run with --run-ignored only --no-capture"]
    fn layers_under_burst_loss() {
        for scene in [STILL, SCROLL] {
            for loss in [0.0, 0.01, 0.03, 0.05, 0.10] {
                for burst in [1.0, 2.0, 4.0] {
                    if loss == 0.0 && burst > 1.0 {
                        continue;
                    }
                    for layers in [Layers::Off, Layers::Unflagged, Layers::Flagged] {
                        let mut sum = Outcome::default();
                        for seed in [1, 2, 3] {
                            sum.add(&run(
                                scene,
                                layers,
                                loss,
                                burst,
                                60,
                                seed * 0x1234_5678_9abc_def1,
                            ));
                        }
                        assert!(sum.shown > 0, "{}: nothing shown", scene.name);
                        eprintln!(
                            "MEASURE layers scene={} loss={:.0}% burst={burst} mode={layers:?} \
                             frames={} shown={} skipped={} waited={} late={} lost={} \
                             refresh_episodes={} of_them_layer1={} refreshes={} frozen={}ms \
                             worst={}ms parity/data={:.1}% wire={:.2}Mbit/s",
                            scene.name,
                            loss * 100.0,
                            sum.frames,
                            sum.shown,
                            sum.skipped,
                            sum.retransmitted,
                            sum.late,
                            sum.lost,
                            sum.episodes,
                            sum.episodes_layer1,
                            sum.refreshes,
                            sum.frozen_ms,
                            sum.worst_ms,
                            sum.parity as f64 * 100.0 / sum.data.max(1) as f64,
                            sum.wire as f64 * 8.0 / 180.0 / 1e6,
                        );
                    }
                }
            }
        }
    }
}
