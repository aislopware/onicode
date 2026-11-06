//! The Mac's sound on this device: one Opus decoder and one player for the whole link.
//!
//! desktopd captures the Mac's sound once for each client that asked to hear it
//! (`Up::Join { sound }`, `Up::Sound`) and sends it on media stream [`SOUND`]. The session
//! starts this task with the link: it reads [`SOUND`] off the router, fills short gaps from the
//! copies a later datagram carries or conceals them, plays through the platform's output, and
//! tells desktopd what arrived every [`REPORT_EVERY`], which sizes those copies.
//!
//! Ported from slopty's client `screen/sound.rs` (MIT, same authors). The player is
//! `onicode_desktop_codec::audio`'s, Apple only; elsewhere the packets are counted and dropped.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use onicode_desktop_media::wire::{Feedback, HEADER_BYTES, Kind, MediaHeader, SOUND};
use onicode_desktop_media::{MAX_AUDIO_COPIES, SoundCount, parse_audio};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::router::{Arrival, ScreenRouter};
use crate::screen::{Delivery, REPORT_EVERY};

/// What the sound has done so far.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SoundStats {
    /// Datagrams that came on [`SOUND`].
    pub datagrams: u64,
    /// Packets decoded and played (or held, while muted).
    pub packets: u64,
    /// Packets missing from the sequence, late, or landed before the player was open.
    pub lost: u64,
    /// Lost packets stood in for.
    pub concealed: u64,
    /// Lost packets decoded from a copy a later datagram carried.
    pub recovered: u64,
    /// Times playback ran dry under sound.
    pub underruns: u64,
    /// Audio dropped to keep the delay at the target.
    pub trimmed: Duration,
    /// Audio played twice to keep the depth at the target.
    pub stretched: Duration,
    /// Depth the player aims for.
    pub target: Duration,
}

/// Opens the platform's player, on a blocking thread: `CoreAudio`'s first client in a process
/// initialises the HAL, which slopty measured at seconds.
pub type OpenPlayer = fn() -> Option<Box<dyn Playback>>;

/// Where decoded sound goes.
pub trait Playback: Send {
    /// Decode one Opus packet; `None` when it does not decode.
    fn decode(&mut self, packet: &[u8]) -> Option<Vec<f32>>;
    /// Queue `pcm`, a packet that arrived at `at` numbered `seq`; `muted` keeps its timing but
    /// not its sound.
    fn play(&mut self, pcm: &[f32], seq: u32, at: Instant, muted: bool);
    /// Queue a stand-in for lost packets.
    fn conceal(&mut self, pcm: &[f32]);
    /// The stand-in for `missing` lost packets, after what was played.
    fn stand_in(&mut self, missing: u32, out: &mut Vec<f32>);
    /// `pcm` as it is to be played: crossfaded in from a stand-in before it.
    fn take(&mut self, pcm: &[f32]) -> Vec<f32>;
    /// Counters to lay over [`SoundStats`].
    fn stats(&self, stats: &mut SoundStats);
}

/// The platform's player: `onicode_desktop_codec::audio` on Apple platforms, none elsewhere.
#[must_use]
pub fn platform_player() -> Option<Box<dyn Playback>> {
    #[cfg(target_vendor = "apple")]
    {
        match apple::Opus::new() {
            Ok(playback) => Some(Box::new(playback)),
            Err(e) => {
                tracing::warn!(error = %e, "audio playback unavailable");
                None
            }
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        None
    }
}

#[cfg(target_vendor = "apple")]
mod apple {
    use std::time::Instant;

    use onicode_desktop_codec::CodecError;
    use onicode_desktop_codec::audio::{Arrival, Conceal, OpusDecoder, Player};

    use super::{Playback, SoundStats};

    pub(super) struct Opus {
        decoder: OpusDecoder,
        player: Player,
        conceal: Conceal,
    }

    impl Opus {
        pub(super) fn new() -> Result<Self, CodecError> {
            Ok(Self {
                decoder: OpusDecoder::new()?,
                player: Player::new()?,
                conceal: Conceal::default(),
            })
        }
    }

    impl Playback for Opus {
        fn decode(&mut self, packet: &[u8]) -> Option<Vec<f32>> {
            match self.decoder.decode(packet) {
                Ok(pcm) => Some(pcm.to_vec()),
                Err(e) => {
                    tracing::debug!(error = %e, "opus decode");
                    None
                }
            }
        }

        fn play(&mut self, pcm: &[f32], seq: u32, at: Instant, muted: bool) {
            let arrival = Arrival { seq, at };
            if muted {
                self.player.hold(arrival);
            } else {
                self.player.push(pcm, arrival);
            }
        }

        fn conceal(&mut self, pcm: &[f32]) {
            self.player.conceal(pcm);
        }

        fn stand_in(&mut self, missing: u32, out: &mut Vec<f32>) {
            self.conceal.fill(missing, out);
        }

        fn take(&mut self, pcm: &[f32]) -> Vec<f32> {
            self.conceal.take(pcm).to_vec()
        }

        fn stats(&self, stats: &mut SoundStats) {
            let playout = self.player.stats();
            stats.underruns = playout.underruns;
            stats.trimmed = playout.trimmed;
            stats.stretched = playout.stretched;
            stats.target = playout.target;
        }
    }
}

/// The link's sound. Dropping it ends the task and lets go of [`SOUND`].
pub struct Sound {
    muted: Arc<AtomicBool>,
    stats: watch::Receiver<SoundStats>,
    router: ScreenRouter,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for Sound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sound")
            .field("stats", &*self.stats.borrow())
            .finish_non_exhaustive()
    }
}

impl Drop for Sound {
    fn drop(&mut self) {
        self.task.abort();
        self.router.detach(SOUND);
    }
}

impl Sound {
    /// Play what `router` brings on [`SOUND`] through what `open` opens, reporting on
    /// `feedback`; `muted` silences it here.
    pub fn spawn(
        runtime: &tokio::runtime::Handle,
        router: &ScreenRouter,
        open: OpenPlayer,
        muted: bool,
        feedback: Box<dyn Fn(Bytes) -> Delivery + Send>,
    ) -> Self {
        let muted = Arc::new(AtomicBool::new(muted));
        let (stats_tx, stats) = watch::channel(SoundStats::default());
        let task = Task {
            datagrams: router.attach(SOUND).datagrams,
            feedback,
            muted: Arc::clone(&muted),
            stats: stats_tx,
            counters: SoundStats::default(),
            count: SoundCount::default(),
            player: Slot::Unopened(open),
            seq: None,
            stand_in: Vec::new(),
        };
        Self {
            muted,
            stats,
            router: router.clone(),
            task: runtime.spawn(task.run()),
        }
    }

    /// What it has done so far.
    #[must_use]
    pub fn stats(&self) -> SoundStats {
        *self.stats.borrow()
    }

    /// Silence or resume playback here. Packets keep being decoded, so the decoder's state stays
    /// continuous.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Whether playback is silenced.
    #[must_use]
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
}

/// Playback, opened on the first packet.
enum Slot {
    Unopened(OpenPlayer),
    Opening(oneshot::Receiver<Option<Box<dyn Playback>>>),
    Open(Box<dyn Playback>),
    /// Playback could not start; packets are counted lost.
    Failed,
}

struct Task {
    datagrams: tokio::sync::mpsc::Receiver<Arrival>,
    feedback: Box<dyn Fn(Bytes) -> Delivery + Send>,
    muted: Arc<AtomicBool>,
    stats: watch::Sender<SoundStats>,
    counters: SoundStats,
    /// What arrived since the last report.
    count: SoundCount,
    player: Slot,
    /// The last packet played.
    seq: Option<u32>,
    /// Scratch for the stand-in samples.
    stand_in: Vec<f32>,
}

impl Task {
    async fn run(mut self) {
        let mut report = tokio::time::interval(REPORT_EVERY);
        report.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _instant = report.tick() => {
                    if !self.report() {
                        break;
                    }
                }
                arrival = self.datagrams.recv() => {
                    let Some((at, datagram)) = arrival else { break };
                    self.ingest(&datagram, at);
                    while let Ok((at, datagram)) = self.datagrams.try_recv() {
                        self.ingest(&datagram, at);
                    }
                }
            }
        }
        tracing::debug!("the sound finished");
    }

    fn ingest(&mut self, datagram: &Bytes, at: Instant) {
        self.counters.datagrams = self.counters.datagrams.saturating_add(1);
        let Some((header, _payload)) = MediaHeader::parse(datagram) else {
            return;
        };
        if header.kind() != Some(Kind::Audio) {
            return;
        }
        let (data_count, seq) = (header.data_count.get(), header.frame.get());
        let payload = datagram.slice(HEADER_BYTES..);
        let Some(packets) = parse_audio(data_count, &payload) else {
            return;
        };
        self.count.arrived(seq);
        self.play(seq, &packets.packet, &packets.earlier, at);
    }

    /// Move the player towards open: start opening it on the first packet, pick it up once the
    /// blocking thread is done. Never waits.
    fn open(&mut self) {
        self.player = match std::mem::replace(&mut self.player, Slot::Failed) {
            Slot::Unopened(open) => {
                let (tx, rx) = oneshot::channel();
                drop(tokio::task::spawn_blocking(move || {
                    let _no_task = tx.send(open());
                }));
                Slot::Opening(rx)
            }
            Slot::Opening(mut rx) => match rx.try_recv() {
                Ok(Some(playback)) => Slot::Open(playback),
                Ok(None) | Err(oneshot::error::TryRecvError::Closed) => Slot::Failed,
                Err(oneshot::error::TryRecvError::Empty) => Slot::Opening(rx),
            },
            open_or_failed => open_or_failed,
        };
    }

    /// Decode and queue one packet that arrived at `at`; late duplicates are dropped, gaps
    /// counted. The end of a gap that `earlier` covers is decoded from those copies, and only
    /// the rest is concealed. A packet that lands while the player is still opening is lost.
    fn play(
        &mut self,
        seq: u32,
        payload: &Bytes,
        earlier: &[Option<Bytes>; MAX_AUDIO_COPIES],
        at: Instant,
    ) {
        self.open();
        let Slot::Open(playback) = &mut self.player else {
            self.counters.lost = self.counters.lost.saturating_add(1);
            return;
        };
        let muted = self.muted.load(Ordering::Relaxed);
        if let Some(last) = self.seq {
            let gap = seq.wrapping_sub(last);
            if gap == 0 || gap > u32::MAX / 2 {
                self.counters.lost = self.counters.lost.saturating_add(1);
                return;
            }
            if gap > 1 {
                let missing = gap.saturating_sub(1);
                self.counters.lost = self.counters.lost.saturating_add(u64::from(missing));
                let copies = earlier.iter().take_while(|copy| copy.is_some()).count();
                let recovered = missing.min(u32::try_from(copies).unwrap_or(0));
                let concealed = missing.saturating_sub(recovered);
                self.stand_in.clear();
                if concealed > 0 {
                    playback.stand_in(concealed, &mut self.stand_in);
                    if !self.stand_in.is_empty() {
                        self.counters.concealed =
                            self.counters.concealed.saturating_add(u64::from(concealed));
                    }
                }
                // Oldest first; a recovered packet keeps the gap's time.
                let copies = earlier
                    .get(..usize::try_from(recovered).unwrap_or(0))
                    .unwrap_or_default();
                for copy in copies.iter().rev().flatten() {
                    if let Some(pcm) = playback.decode(copy) {
                        let pcm = playback.take(&pcm);
                        self.stand_in.extend_from_slice(&pcm);
                        self.counters.recovered = self.counters.recovered.saturating_add(1);
                    }
                }
                if !self.stand_in.is_empty() && !muted {
                    playback.conceal(&self.stand_in);
                }
            }
        }
        self.seq = Some(seq);
        if let Some(pcm) = playback.decode(payload) {
            let pcm = playback.take(&pcm);
            playback.play(&pcm, seq, at, muted);
            self.counters.packets = self.counters.packets.saturating_add(1);
        }
    }

    /// Publish the counters, and tell desktopd what arrived while anything did. `false` once
    /// the link is gone.
    fn report(&mut self) -> bool {
        if let Slot::Open(playback) = &self.player {
            playback.stats(&mut self.counters);
        }
        let counters = self.counters;
        self.stats
            .send_if_modified(|stats| std::mem::replace(stats, counters) != counters);
        let report = self.count.take();
        if report.received == 0 && report.lost == 0 {
            return true;
        }
        // A report that does not go loses the window's count; the next brings the next one.
        (self.feedback)(Feedback::Sound(report).encode()) != Delivery::Gone
    }
}

#[cfg(test)]
mod tests {
    use onicode_desktop_media::audio_datagram;
    use onicode_desktop_media::wire::SoundReport;
    use parking_lot::Mutex;

    use super::*;

    /// A player that decodes a packet to its bytes as samples, and plays nothing.
    struct Echo;

    impl Playback for Echo {
        fn decode(&mut self, packet: &[u8]) -> Option<Vec<f32>> {
            Some(packet.iter().map(|b| f32::from(*b)).collect())
        }

        fn play(&mut self, _pcm: &[f32], _seq: u32, _at: Instant, _muted: bool) {}

        fn conceal(&mut self, _pcm: &[f32]) {}

        fn stand_in(&mut self, missing: u32, out: &mut Vec<f32>) {
            out.extend(std::iter::repeat_n(
                0.0,
                usize::try_from(missing).unwrap() * 4,
            ));
        }

        fn take(&mut self, pcm: &[f32]) -> Vec<f32> {
            pcm.to_vec()
        }

        fn stats(&self, _stats: &mut SoundStats) {}
    }

    fn echo() -> Option<Box<dyn Playback>> {
        Some(Box::new(Echo))
    }

    fn never() -> Option<Box<dyn Playback>> {
        None
    }

    struct Harness {
        rt: tokio::runtime::Runtime,
        router: ScreenRouter,
        sound: Sound,
        reports: Arc<Mutex<Vec<SoundReport>>>,
        routed: u64,
    }

    impl Harness {
        fn start(open: OpenPlayer) -> Self {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            let router = ScreenRouter::new();
            let reports = Arc::new(Mutex::new(Vec::new()));
            let caught = Arc::clone(&reports);
            let feedback = Box::new(move |bytes: Bytes| {
                if let Some(Feedback::Sound(report)) = Feedback::decode(&bytes) {
                    caught.lock().push(report);
                }
                Delivery::Sent
            });
            let sound = Sound::spawn(rt.handle(), &router, open, false, feedback);
            Self {
                rt,
                router,
                sound,
                reports,
                routed: 0,
            }
        }

        fn route(&mut self, seq: u32, earlier: &[&[u8]]) {
            self.routed += 1;
            let datagram = audio_datagram(SOUND, seq, 0, &[1, 2, 3], earlier).unwrap();
            self.router.route(datagram, Instant::now());
        }

        fn wait_for(&self, what: &str, mut done: impl FnMut(&Self) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(3);
            while !done(self) {
                assert!(
                    Instant::now() < deadline,
                    "{what}: {:?}",
                    self.sound.stats()
                );
                self.rt
                    .block_on(async { tokio::time::sleep(Duration::from_millis(5)).await });
            }
        }

        fn settle(&self) -> SoundStats {
            self.wait_for("catching up", |h| h.sound.stats().datagrams == h.routed);
            self.sound.stats()
        }
    }

    /// The first packet starts opening the player off the task and is lost to it; once it is
    /// open a gap is concealed, a late duplicate is not played, and a gap the next datagram
    /// carries again is decoded from its copies. desktopd hears of every packet that arrived.
    #[test]
    fn a_gap_is_concealed_or_recovered_and_a_late_packet_dropped() {
        let mut h = Harness::start(echo);
        let mut seq = 1;
        h.route(seq, &[]);
        while h.settle().packets == 0 {
            seq += 1;
            h.route(seq, &[]);
        }
        let before = h.settle();
        assert!(
            before.lost >= 1,
            "the packets that landed while opening: {before:?}"
        );
        seq += 2;
        h.route(seq, &[]);
        let after = h.settle();
        assert_eq!(after.packets, before.packets + 1);
        assert_eq!(after.lost, before.lost + 1, "one missing");
        assert_eq!(after.concealed, before.concealed + 1, "and concealed");
        h.route(seq - 1, &[]);
        let late = h.settle();
        assert_eq!(late.lost, after.lost + 1, "too late to play");
        assert_eq!(late.packets, after.packets, "not played");
        seq += 3;
        h.route(seq, &[&[9], &[8]]);
        let recovered = h.settle();
        assert_eq!(recovered.lost, late.lost + 2);
        assert_eq!(
            recovered.recovered,
            late.recovered + 2,
            "both from the copies"
        );
        assert_eq!(recovered.concealed, late.concealed, "nothing concealed");
        h.wait_for("the last report", |h| {
            h.reports
                .lock()
                .iter()
                .map(|r| u64::from(r.received))
                .sum::<u64>()
                == h.routed - 1
        });
        let lost: u32 = h.reports.lock().iter().map(|r| u32::from(r.lost)).sum();
        assert_eq!(lost, 1 + 2, "the gap of one and the gap of two");
    }

    /// A platform with no player counts every packet lost, and still reports what arrived.
    #[test]
    fn no_player_counts_everything_lost_and_still_reports() {
        let mut h = Harness::start(never);
        for seq in 1..=5 {
            h.route(seq, &[]);
        }
        let stats = h.settle();
        assert_eq!((stats.packets, stats.lost), (0, 5));
        h.wait_for("a report", |h| {
            h.reports.lock().iter().map(|r| r.received).sum::<u16>() == 5
        });
        drop(h.sound);
        assert_eq!(h.router.attached(), 0, "the sound lets go of its stream");
    }
}
