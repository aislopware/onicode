//! A client's one sound from this Mac (slopty's audio decisions, "one sound per worker on a
//! client, not one per stream").
//!
//! A client that asks for sound (`Up::Join { sound }`, `Up::Sound`) gets the whole Mac's sound,
//! this process's own left out, once however many displays it streams: one capture, one Opus
//! encoder, 10 ms packets on the [`SOUND`] stream, one a datagram. A lost packet is not sent
//! again: the client fades over the gap. Silence past [`AUDIO_HOLD_US`] sends nothing.

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bytes::Bytes;
use onicode_desktop_capture::{
    AUDIO_CHANNELS, AUDIO_RATE, CaptureError, CaptureSource as _, CapturedAudio, Heard,
};
use onicode_desktop_codec::AudioEncoder as _;
use onicode_desktop_media::audio_datagram;
use onicode_desktop_media::wire::SOUND;
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
        }
    }

    fn on_audio(&self, chunk: &CapturedAudio) {
        let now = Capture::<P>::now_us();
        let Some(packets) = self.encode(&chunk.samples, now) else {
            return;
        };
        let datagrams: Vec<Bytes> = packets
            .iter()
            .filter_map(|(seq, packet)| audio_datagram(SOUND, *seq, send_ms_lo(now), packet))
            .collect();
        if datagrams.is_empty() {
            return;
        }
        if let Err(refused) = self.sink.send(&datagrams) {
            tracing::debug!(?refused, "sound datagrams not sent");
        }
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
    platform: PhantomData<fn() -> P>,
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
        let voice = Arc::new(Voice::<P>::new(sink));
        tokio::spawn(follow::<P>(Arc::clone(&wish), voice));
        Self {
            wish,
            platform: PhantomData,
        }
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
