//! A client's one sound from this Mac (slopty's audio decisions, "one sound per worker on a
//! client, not one per stream").
//!
//! A client that asks for sound (`Up::Join { sound }`, `Up::Sound`) gets the whole Mac's sound,
//! this process's own left out, once however many displays it streams: one capture, one Opus
//! encoder, 10 ms packets on the [`SOUND`] stream. Each packet carries copies of the one or two
//! before it while the client reports loss ([`AudioCopies`]), and the video of the client's
//! streams goes through their lane while the sound is live, so a keyframe never queues in front
//! of it ([`crate::pipeline::lane`]). Silence past [`AUDIO_HOLD_US`] sends nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use onicode_desktop_capture::{
    AUDIO_CHANNELS, AUDIO_RATE, CaptureError, CaptureSource as _, CapturedAudio, Heard,
};
use onicode_desktop_codec::AudioEncoder as _;
use onicode_desktop_media::wire::{SOUND, SoundReport};
use onicode_desktop_media::{AudioCopies, MAX_AUDIO_COPIES, audio_datagram};
use parking_lot::Mutex;
use tokio::sync::{Notify, oneshot};

use crate::pipeline::DatagramSink;
use crate::pipeline::fit::send_ms_lo;
use crate::platform::Platform;

/// How long sound under `AUDIO_FLOOR` keeps being sent, so a fade's tail is not cut.
pub const AUDIO_HOLD_US: u64 = 300_000;
/// The level under which a chunk is silence.
const AUDIO_FLOOR: f32 = 1e-4;
/// How long after a failed start the capture is tried again.
const RETRY: Duration = Duration::from_secs(2);

type Capture<P> = <P as Platform>::Capture;
type Running<P> = <Capture<P> as onicode_desktop_capture::CaptureSource>::Sound;

/// When the sound last went out, which the client's streams read to keep their video behind it.
#[derive(Debug, Default)]
pub struct SoundClock {
    last_us: AtomicU64,
    packets: AtomicU64,
}

impl SoundClock {
    /// The capture clock's time of the last packet sent; `0` before one.
    #[must_use]
    pub fn last_us(&self) -> u64 {
        self.last_us.load(Ordering::Relaxed)
    }

    /// Packets sent.
    #[must_use]
    pub fn packets(&self) -> u64 {
        self.packets.load(Ordering::Relaxed)
    }

    /// A packet went out at `now_us`.
    pub fn sent_at(&self, now_us: u64) {
        self.last_us.store(now_us.max(1), Ordering::Relaxed);
    }
}

/// The encoder and its packets' numbers.
struct AudioState<A> {
    encoder: Option<A>,
    seq: u32,
    last_loud_us: u64,
    /// The encoder could not be had: no sound, for good.
    refused: bool,
}

/// What turns captured sound into datagrams.
struct Voice<P: Platform> {
    sink: Arc<dyn DatagramSink>,
    audio: Mutex<AudioState<P::Audio>>,
    copies: Mutex<AudioCopies>,
    sent: Mutex<[Bytes; MAX_AUDIO_COPIES]>,
    clock: Arc<SoundClock>,
}

impl<P: Platform> Voice<P> {
    fn new(sink: Arc<dyn DatagramSink>) -> Self {
        Self {
            sink,
            audio: Mutex::new(AudioState {
                encoder: None,
                seq: 0,
                last_loud_us: 0,
                refused: false,
            }),
            copies: Mutex::new(AudioCopies::default()),
            sent: Mutex::new(Default::default()),
            clock: Arc::default(),
        }
    }

    fn on_audio(&self, chunk: &CapturedAudio) {
        let now = Capture::<P>::now_us();
        let Some(packets) = self.encode(&chunk.samples, now) else {
            return;
        };
        let datagrams = self.datagrams(&packets, now);
        if datagrams.is_empty() {
            return;
        }
        self.clock.sent_at(now);
        if self.sink.send(&datagrams).is_ok() {
            let n = u64::try_from(datagrams.len()).unwrap_or(u64::MAX);
            self.clock.packets.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Each packet as its datagram, with the copies in force of the ones before it.
    fn datagrams(&self, packets: &[(u32, Bytes)], now: u64) -> Vec<Bytes> {
        let copies = self.copies.lock().copies();
        let mut sent = self.sent.lock();
        let mut datagrams = Vec::with_capacity(packets.len());
        for (seq, packet) in packets {
            let earlier: Vec<&[u8]> = sent.iter().take(copies).map(|b| &b[..]).collect();
            datagrams.extend(audio_datagram(
                SOUND,
                *seq,
                send_ms_lo(now),
                packet,
                &earlier,
            ));
            sent.rotate_right(1);
            if let Some(nearest) = sent.first_mut() {
                nearest.clone_from(packet);
            }
        }
        datagrams
    }

    fn encode(&self, samples: &[f32], now: u64) -> Option<Vec<(u32, Bytes)>> {
        let loud = samples.iter().any(|s| s.abs() > AUDIO_FLOOR);
        let mut audio = self.audio.lock();
        if audio.refused {
            return None;
        }
        if loud {
            audio.last_loud_us = now;
        } else if now.saturating_sub(audio.last_loud_us) > AUDIO_HOLD_US {
            return None;
        }
        if audio.encoder.is_none() {
            match <P::Audio as onicode_desktop_codec::AudioEncoder>::new() {
                Ok(encoder) => audio.encoder = Some(encoder),
                Err(e) => {
                    tracing::warn!(error = %e, "no Opus encoder; no sound");
                    audio.refused = true;
                    return None;
                }
            }
        }
        let AudioState {
            encoder: Some(encoder),
            seq,
            ..
        } = &mut *audio
        else {
            return None;
        };
        let mut packets = Vec::new();
        let encoded = encoder.push(samples, |packet| {
            *seq = seq.wrapping_add(1);
            packets.push((*seq, Bytes::copy_from_slice(packet)));
        });
        drop(audio);
        match encoded {
            Ok(()) => Some(packets),
            Err(e) => {
                tracing::debug!(error = %e, "opus encode");
                None
            }
        }
    }
}

/// Whether the client wants sound, and whether the capture died under it.
#[derive(Debug, Default)]
struct Wish {
    on: AtomicBool,
    closed: AtomicBool,
    died: AtomicBool,
    changed: Notify,
}

/// A client's sound: on while it asks for it. Dropping it stops the capture.
pub struct Sound<P: Platform> {
    wish: Arc<Wish>,
    voice: Arc<Voice<P>>,
}

impl<P: Platform> std::fmt::Debug for Sound<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sound")
            .field("on", &self.wish.on.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<P: Platform> Sound<P> {
    /// The client's sound, off, going to `sink`.
    #[must_use]
    pub fn new(sink: Arc<dyn DatagramSink>) -> Self {
        let wish = Arc::new(Wish::default());
        let voice = Arc::new(Voice::new(sink));
        tokio::spawn(follow::<P>(Arc::clone(&wish), Arc::clone(&voice)));
        Self { wish, voice }
    }

    /// Turn the sound on or off.
    pub fn set(&self, on: bool) {
        if self.wish.on.swap(on, Ordering::AcqRel) != on {
            self.wish.changed.notify_one();
        }
    }

    /// Whether the client asked for sound.
    #[must_use]
    pub fn on(&self) -> bool {
        self.wish.on.load(Ordering::Acquire)
    }

    /// The client's report of what its player heard: the copies follow its losses.
    pub fn report(&self, report: SoundReport) {
        let (was, copies) = {
            let mut control = self.voice.copies.lock();
            (
                control.copies(),
                control.on_report(report, Capture::<P>::now_us()),
            )
        };
        if copies != was {
            tracing::debug!(was, copies, lost = report.lost, "audio copies");
        }
    }

    /// When the sound last went out.
    #[must_use]
    pub fn clock(&self) -> Arc<SoundClock> {
        Arc::clone(&self.voice.clock)
    }
}

impl<P: Platform> Drop for Sound<P> {
    fn drop(&mut self) {
        // The task stops the capture and ends; aborting it would leave the capture running.
        self.wish.closed.store(true, Ordering::Release);
        self.wish.changed.notify_one();
    }
}

/// Start, keep or stop the capture as the client's wish says, until the sound is dropped.
async fn follow<P: Platform>(wish: Arc<Wish>, voice: Arc<Voice<P>>) {
    let mut running: Option<Running<P>> = None;
    loop {
        let changed = wish.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if wish.died.swap(false, Ordering::AcqRel) {
            running = None;
        }
        let wanted = wish.on.load(Ordering::Acquire) && !wish.closed.load(Ordering::Acquire);
        let mut settled = true;
        match (running.take(), wanted) {
            (None, false) => {}
            (Some(sound), true) => running = Some(sound),
            (Some(sound), false) => {
                let (tx, rx) = oneshot::channel();
                Capture::<P>::stop_sound(&sound, move |done| {
                    let _waiting = tx.send(done);
                });
                if let Ok(Err(e)) = rx.await {
                    tracing::debug!(error = %e, "the sound's capture would not stop");
                }
                drop(sound);
            }
            (None, true) => match start::<P>(&wish, &voice).await {
                Ok(sound) => running = Some(sound),
                Err(e) => {
                    tracing::warn!(error = %e, "no capture of the sound");
                    settled = false;
                }
            },
        }
        if running.is_none() && wish.closed.load(Ordering::Acquire) {
            break;
        }
        if settled {
            changed.await;
        } else {
            let _retry = tokio::time::timeout(RETRY, changed).await;
        }
    }
}

async fn start<P: Platform>(
    wish: &Arc<Wish>,
    voice: &Arc<Voice<P>>,
) -> Result<Running<P>, CaptureError> {
    let fed = Arc::clone(voice);
    let sink = Box::new(move |chunk: CapturedAudio| fed.on_audio(&chunk));
    let watched = Arc::clone(wish);
    let on_stop = move |e: CaptureError| {
        tracing::warn!(error = %e, "the sound's capture stopped");
        watched.died.store(true, Ordering::Release);
        watched.changed.notify_one();
    };
    let (tx, rx) = oneshot::channel();
    Capture::<P>::start_sound(&Heard::Every, sink, on_stop, move |started| {
        let _waiting = tx.send(started);
    });
    rx.await
        .unwrap_or_else(|_gone| Err(CaptureError::Stopped("never answered".to_owned())))
}

/// The length of `samples` of interleaved stereo at the capture's rate, microseconds.
#[must_use]
pub fn length_us(samples: usize) -> u64 {
    let samples = u64::try_from(samples).unwrap_or(u64::MAX);
    let frames = samples.checked_div(u64::from(AUDIO_CHANNELS)).unwrap_or(0);
    frames
        .saturating_mul(1_000_000)
        .checked_div(u64::from(AUDIO_RATE))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_milliseconds_of_stereo() {
        let ten_ms = usize::try_from(AUDIO_RATE / 100 * AUDIO_CHANNELS).unwrap();
        assert_eq!(length_us(ten_ms), 10_000);
    }
}
