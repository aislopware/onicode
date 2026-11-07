//! The Mac's sound on this device: one Opus decoder and one player for the whole link.
//!
//! desktopd captures the Mac's sound once for each client that asked to hear it
//! (`Up::Join { sound }`, `Up::Sound`) and sends it on media stream [`SOUND`], one packet a
//! datagram. The session starts this task with the link: it reads [`SOUND`] off the router,
//! fades over the packets that never came (`onicode_desktop_codec::audio::LossFade`), and plays
//! through the platform's output.
//!
//! Ported from slopty's client `screen/sound.rs` (MIT, same authors). The player is
//! `onicode_desktop_codec::audio`'s.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use onicode_desktop_media::parse_audio;
use onicode_desktop_media::wire::{HEADER_BYTES, Kind, MediaHeader, SOUND};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::router::{Arrival, ScreenRouter};
use crate::screen::REPORT_EVERY;

/// What the sound has done so far.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SoundStats {
    /// Datagrams that came on [`SOUND`].
    pub datagrams: u64,
    /// Packets decoded and played (or held, while muted).
    pub packets: u64,
    /// Packets missing from the sequence, late, or landed before the player was open.
    pub lost: u64,
    /// Lost packets faded over.
    pub concealed: u64,
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
    /// Queue `pcm`, the next packet, faded in when a gap went before it; `muted` keeps its
    /// place but not its sound.
    fn play(&mut self, pcm: &[f32], muted: bool);
    /// Stand in for `missing` lost packets before the next: what was playing fades to silence,
    /// which keeps their time. `false` when nothing stands in (nothing was played yet, or the
    /// gap is a pause).
    fn conceal(&mut self, missing: u32, muted: bool) -> bool;
    /// Counters to lay over [`SoundStats`].
    fn stats(&self, stats: &mut SoundStats);
}

/// The platform's player: `onicode_desktop_codec::audio`'s; `None` when it cannot start.
#[must_use]
pub fn platform_player() -> Option<Box<dyn Playback>> {
    match apple::Opus::new() {
        Ok(playback) => Some(Box::new(playback)),
        Err(e) => {
            tracing::warn!(error = %e, "audio playback unavailable");
            None
        }
    }
}

mod apple {
    use onicode_desktop_codec::CodecError;
    use onicode_desktop_codec::audio::{LossFade, OpusDecoder, Player};

    use super::{Playback, SoundStats};

    pub(super) struct Opus {
        decoder: OpusDecoder,
        player: Player,
        fade: LossFade,
        /// Scratch for a stand-in's samples.
        stand_in: Vec<f32>,
    }

    impl Opus {
        pub(super) fn new() -> Result<Self, CodecError> {
            Ok(Self {
                decoder: OpusDecoder::new()?,
                player: Player::new()?,
                fade: LossFade::default(),
                stand_in: Vec::new(),
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

        fn play(&mut self, pcm: &[f32], muted: bool) {
            let pcm = self.fade.take(pcm);
            if muted {
                self.player.hold();
            } else {
                self.player.push(pcm);
            }
        }

        fn conceal(&mut self, missing: u32, muted: bool) -> bool {
            self.stand_in.clear();
            self.fade.fill(missing, &mut self.stand_in);
            if self.stand_in.is_empty() {
                return false;
            }
            if !muted {
                self.player.conceal(&self.stand_in);
            }
            true
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
    /// Play what `router` brings on [`SOUND`] through what `open` opens; `muted` silences it
    /// here.
    pub fn spawn(
        runtime: &tokio::runtime::Handle,
        router: &ScreenRouter,
        open: OpenPlayer,
        muted: bool,
    ) -> Self {
        let muted = Arc::new(AtomicBool::new(muted));
        let (stats_tx, stats) = watch::channel(SoundStats::default());
        let task = Task {
            datagrams: router.attach(SOUND).datagrams,
            muted: Arc::clone(&muted),
            stats: stats_tx,
            counters: SoundStats::default(),
            player: Slot::Unopened(open),
            seq: None,
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
    muted: Arc<AtomicBool>,
    stats: watch::Sender<SoundStats>,
    counters: SoundStats,
    player: Slot,
    /// The last packet played.
    seq: Option<u32>,
}

impl Task {
    async fn run(mut self) {
        let mut publish = tokio::time::interval(REPORT_EVERY);
        publish.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _instant = publish.tick() => self.publish(),
                arrival = self.datagrams.recv() => {
                    let Some((_at, datagram)) = arrival else { break };
                    self.ingest(&datagram);
                    while let Ok((_at, datagram)) = self.datagrams.try_recv() {
                        self.ingest(&datagram);
                    }
                }
            }
        }
        tracing::debug!("the sound finished");
    }

    fn ingest(&mut self, datagram: &Bytes) {
        self.counters.datagrams = self.counters.datagrams.saturating_add(1);
        let Some((header, _payload)) = MediaHeader::parse(datagram) else {
            return;
        };
        if header.kind() != Some(Kind::Audio) {
            return;
        }
        let (data_count, seq) = (header.data_count.get(), header.frame.get());
        let Some(packet) = parse_audio(data_count, &datagram.slice(HEADER_BYTES..)) else {
            return;
        };
        self.play(seq, &packet);
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

    /// Decode and queue packet `seq`; late duplicates are dropped, and a gap is counted and
    /// faded over. A packet that lands while the player is still opening is lost.
    fn play(&mut self, seq: u32, packet: &[u8]) {
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
            let missing = gap.saturating_sub(1);
            if missing > 0 {
                self.counters.lost = self.counters.lost.saturating_add(u64::from(missing));
                if playback.conceal(missing, muted) {
                    self.counters.concealed =
                        self.counters.concealed.saturating_add(u64::from(missing));
                }
            }
        }
        self.seq = Some(seq);
        if let Some(pcm) = playback.decode(packet) {
            playback.play(&pcm, muted);
            self.counters.packets = self.counters.packets.saturating_add(1);
        }
    }

    /// Publish the counters.
    fn publish(&mut self) {
        if let Slot::Open(playback) = &self.player {
            playback.stats(&mut self.counters);
        }
        let counters = self.counters;
        self.stats
            .send_if_modified(|stats| std::mem::replace(stats, counters) != counters);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use onicode_desktop_media::audio_datagram;

    use super::*;

    /// A player that decodes a packet to its bytes as samples, plays nothing, and stands in for
    /// any gap once something was played.
    #[derive(Default)]
    struct Echo {
        played: bool,
    }

    impl Playback for Echo {
        fn decode(&mut self, packet: &[u8]) -> Option<Vec<f32>> {
            Some(packet.iter().map(|b| f32::from(*b)).collect())
        }

        fn play(&mut self, _pcm: &[f32], _muted: bool) {
            self.played = true;
        }

        fn conceal(&mut self, _missing: u32, _muted: bool) -> bool {
            self.played
        }

        fn stats(&self, _stats: &mut SoundStats) {}
    }

    fn echo() -> Option<Box<dyn Playback>> {
        Some(Box::new(Echo::default()))
    }

    fn never() -> Option<Box<dyn Playback>> {
        None
    }

    struct Harness {
        rt: tokio::runtime::Runtime,
        router: ScreenRouter,
        sound: Sound,
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
            let sound = Sound::spawn(rt.handle(), &router, open, false);
            Self {
                rt,
                router,
                sound,
                routed: 0,
            }
        }

        fn route(&mut self, seq: u32) {
            self.routed += 1;
            let datagram = audio_datagram(SOUND, seq, 0, &[1, 2, 3]).unwrap();
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
    /// open a gap is counted and faded over, and a late duplicate is not played.
    #[test]
    fn a_gap_is_concealed_and_a_late_packet_dropped() {
        let mut h = Harness::start(echo);
        let mut seq = 1;
        h.route(seq);
        while h.settle().packets == 0 {
            seq += 1;
            h.route(seq);
        }
        let before = h.settle();
        assert!(
            before.lost >= 1,
            "the packets that landed while opening: {before:?}"
        );
        seq += 2;
        h.route(seq);
        let after = h.settle();
        assert_eq!(after.packets, before.packets + 1);
        assert_eq!(after.lost, before.lost + 1, "one missing");
        assert_eq!(after.concealed, before.concealed + 1, "and faded over");
        h.route(seq - 1);
        let late = h.settle();
        assert_eq!(late.lost, after.lost + 1, "too late to play");
        assert_eq!(late.packets, after.packets, "not played");
        seq += 3;
        h.route(seq);
        let gap = h.settle();
        assert_eq!(gap.lost, late.lost + 2);
        assert_eq!(gap.concealed, late.concealed + 2, "both faded over");
    }

    /// A platform with no player counts every packet lost.
    #[test]
    fn no_player_counts_everything_lost() {
        let mut h = Harness::start(never);
        for seq in 1..=5 {
            h.route(seq);
        }
        let stats = h.settle();
        assert_eq!((stats.packets, stats.lost), (0, 5));
        drop(h.sound);
        assert_eq!(h.router.attached(), 0, "the sound lets go of its stream");
    }
}
