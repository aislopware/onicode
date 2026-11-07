//! Opus over `AudioToolbox`.
//!
//! The system encoder and decoder behind `AudioConverter`, and an output audio unit for
//! playback that pulls from a jitter ring on the device's I/O thread. No third-party codec:
//! Apple ships Opus in the OS and the same calls work on macOS and iOS.
//!
//! Everything is 48 kHz stereo float, interleaved, 10 ms packets (480 frames): the shortest Opus
//! frame that keeps full CELT quality (RFC 6716 allows 2.5 to 60 ms), and one packet fits a
//! datagram at any sane bitrate.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::Ordering;
#[cfg(not(onicode_loom))]
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};
use std::time::Duration;

// The ring on loom's atomics, which `ring_model` checks over every interleaving.
#[cfg(onicode_loom)]
use loom::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize};
use objc2_audio_toolbox::{
    AURenderCallbackStruct, AudioComponentDescription, AudioComponentFindNext,
    AudioComponentInstanceDispose, AudioComponentInstanceNew, AudioConverterDispose,
    AudioConverterFillComplexBuffer, AudioConverterGetProperty, AudioConverterNew,
    AudioConverterRef, AudioConverterSetProperty, AudioOutputUnitStart, AudioOutputUnitStop,
    AudioUnit, AudioUnitInitialize, AudioUnitRenderActionFlags, AudioUnitSetProperty,
    AudioUnitUninitialize, kAudioComponentErr_UnsupportedType, kAudioConverterEncodeBitRate,
    kAudioConverterPropertyMaximumOutputPacketSize, kAudioUnitManufacturer_Apple,
    kAudioUnitProperty_MaximumFramesPerSlice, kAudioUnitProperty_SetRenderCallback,
    kAudioUnitProperty_StreamFormat, kAudioUnitScope_Global, kAudioUnitScope_Input,
    kAudioUnitType_Output,
};
#[cfg(target_os = "macos")]
use objc2_audio_toolbox::{AudioUnitGetProperty, kAudioUnitSubType_DefaultOutput};
#[cfg(target_os = "macos")]
use objc2_core_audio::{
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyBufferFrameSizeRange,
};
#[cfg(target_os = "macos")]
use objc2_core_audio_types::AudioValueRange;
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioStreamPacketDescription,
    AudioTimeStamp, kAudioFormatFlagsNativeFloatPacked, kAudioFormatLinearPCM, kAudioFormatOpus,
};
use parking_lot::Mutex;

use crate::CodecError;
use crate::video::AudioEncoder;

/// Sample rate, Hz.
pub const SAMPLE_RATE: u32 = 48_000;
/// Channels.
pub const CHANNELS: u32 = 2;
/// Frames per Opus packet (10 ms).
pub const FRAME_SAMPLES: u32 = 480;
/// Encoder target, bits per second.
pub const BITRATE: u32 = 96_000;
/// Samples per frame, one a channel.
const SAMPLES_PER_FRAME: usize = CHANNELS as usize;
/// Samples (both channels) per packet.
const PACKET_SAMPLES: usize = FRAME_SAMPLES as usize * CHANNELS as usize;
/// Bytes per f32 sample.
const SAMPLE_BYTES: usize = size_of::<f32>();

/// Longest gap stood in for, in packets (60 ms). A longer one is a pause: holding its time
/// would only hold the sound after it that much later, and the ring runs dry to silence on its
/// own.
pub const MAX_CONCEALED: u32 = 6;

/// Frames a packet lasts.
const PACKET_FRAMES: usize = FRAME_SAMPLES as usize;
/// How long what was playing takes to fade to silence when packets are lost, and the packet
/// after the gap to fade back in, frames: 5 ms. Long enough that neither edge clicks, short
/// enough that nothing is guessed.
const LOSS_FADE_FRAMES: usize = 240;

/// Packet-loss concealment by a short fade to silence.
///
/// Apple's Opus decoder takes no empty packet for its own concealment, and a guess at the lost
/// sound (repeating its pitch) is a DSP stage of its own for a link that seldom loses a packet.
/// So a gap is stood in for by the last frame played, ramped down to silence over
/// `LOSS_FADE_FRAMES` (5 ms), and silence for the rest of it, which keeps the ring's timing: the
/// gap took 10 ms a packet on the host, and playing that long keeps the next packet from arriving
/// early. The packet after the gap fades in from silence over the same length ([`Self::take`]).
/// A gap longer than [`MAX_CONCEALED`] is a pause and is not stood in for.
#[derive(Debug, Default)]
pub struct LossFade {
    /// The last frame played.
    last: [f32; SAMPLES_PER_FRAME],
    /// Something was played since the last pause.
    heard: bool,
    /// A stand-in went before: the next packet fades in.
    faded: bool,
    /// The next packet with its fade-in.
    merged: Vec<f32>,
}

impl LossFade {
    /// The packet to play for `pcm`, just decoded: faded in from silence when a stand-in went
    /// before it.
    pub fn take<'a>(&'a mut self, pcm: &'a [f32]) -> &'a [f32] {
        let played: &[f32] = if std::mem::take(&mut self.faded) {
            self.merged.clear();
            self.merged.extend_from_slice(pcm);
            for (k, frame) in self
                .merged
                .as_chunks_mut::<SAMPLES_PER_FRAME>()
                .0
                .iter_mut()
                .take(LOSS_FADE_FRAMES)
                .enumerate()
            {
                let w = ramp(k, LOSS_FADE_FRAMES);
                for sample in frame {
                    *sample *= w;
                }
            }
            &self.merged
        } else {
            pcm
        };
        if let Some(last) = played.as_chunks::<SAMPLES_PER_FRAME>().0.last() {
            self.last = *last;
            self.heard = true;
        }
        played
    }

    /// Append samples standing in for `missing` packets: the last frame played fading to
    /// silence, then silence. Nothing when nothing was played yet or the gap is longer than
    /// [`MAX_CONCEALED`]: that is a pause, which the ring plays as silence.
    pub fn fill(&mut self, missing: u32, out: &mut Vec<f32>) {
        if missing > MAX_CONCEALED {
            self.heard = false;
            self.faded = false;
            return;
        }
        if missing == 0 || !self.heard {
            return;
        }
        let frames = usize::try_from(missing)
            .unwrap_or(0)
            .saturating_mul(PACKET_FRAMES);
        out.reserve(frames.saturating_mul(SAMPLES_PER_FRAME));
        for t in 0..frames {
            let gain = (1.0 - ramp(t, LOSS_FADE_FRAMES)).max(0.0);
            out.extend(self.last.map(|sample| sample * gain));
        }
        self.last = [0.0; SAMPLES_PER_FRAME];
        self.faded = true;
    }
}

/// The weight of the incoming side `k` frames into a fade of `len`, rising towards 1.
fn ramp(k: usize, len: usize) -> f32 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "frame counts under a few thousand"
    )]
    let w = (k as f32 + 1.0) / (len as f32 + 1.0);
    w
}

/// A byte or sample count as the `u32` the C structs carry; saturates on nonsense sizes.
fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
/// Status the input procs return once they have handed over everything they hold; the
/// converter surfaces it when it wanted more, and the caller treats it as "done for now".
const NO_MORE_INPUT: i32 = 0x534c_4f50; // 'SLOP'

fn pcm_format() -> AudioStreamBasicDescription {
    let bytes_per_frame = CHANNELS * 4;
    AudioStreamBasicDescription {
        mSampleRate: f64::from(SAMPLE_RATE),
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: CHANNELS,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

fn opus_format() -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: f64::from(SAMPLE_RATE),
        mFormatID: kAudioFormatOpus,
        mFormatFlags: 0,
        mBytesPerPacket: 0,
        mFramesPerPacket: FRAME_SAMPLES,
        mBytesPerFrame: 0,
        mChannelsPerFrame: CHANNELS,
        mBitsPerChannel: 0,
        mReserved: 0,
    }
}

const fn check(call: &'static str, status: i32) -> Result<(), CodecError> {
    if status == 0 {
        Ok(())
    } else {
        Err(CodecError::Os { call, status })
    }
}

/// An `AudioConverter` that is disposed with the struct.
struct Converter(AudioConverterRef);

// SAFETY: AudioConverter objects have no thread affinity; every call goes through `&mut`.
unsafe impl Send for Converter {}

impl Converter {
    fn new(
        from: &AudioStreamBasicDescription,
        to: &AudioStreamBasicDescription,
    ) -> Result<Self, CodecError> {
        let mut from = *from;
        let mut to = *to;
        let mut raw: AudioConverterRef = ptr::null_mut();
        // SAFETY: AudioToolbox rule: both descriptions and the out pointer are valid for the call.
        let status = unsafe {
            AudioConverterNew(
                NonNull::from(&mut from),
                NonNull::from(&mut to),
                NonNull::from(&mut raw),
            )
        };
        check("AudioConverterNew", status)?;
        Ok(Self(raw))
    }

    fn set_u32(&mut self, property: u32, value: u32) -> Result<(), CodecError> {
        let mut value = value;
        // SAFETY: AudioToolbox rule: a live converter and a property buffer of the stated size.
        let status = unsafe {
            AudioConverterSetProperty(
                self.0,
                property,
                u32_of(size_of::<u32>()),
                NonNull::from(&mut value).cast::<c_void>(),
            )
        };
        check("AudioConverterSetProperty", status)
    }

    fn get_u32(&mut self, property: u32) -> Result<u32, CodecError> {
        let mut value: u32 = 0;
        let mut size = u32_of(size_of::<u32>());
        // SAFETY: AudioToolbox rule: a live converter and a property buffer of the stated size.
        let status = unsafe {
            AudioConverterGetProperty(
                self.0,
                property,
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast::<c_void>(),
            )
        };
        check("AudioConverterGetProperty", status)?;
        Ok(value)
    }
}

impl Drop for Converter {
    fn drop(&mut self) {
        // SAFETY: AudioToolbox rule: dispose once, after the last call; `&mut` guarantees that.
        let _ignored = unsafe { AudioConverterDispose(self.0) };
    }
}

/// What an input proc hands the converter: one slice, given once.
struct Input<'a> {
    data: &'a [u8],
    frames: u32,
    packet: AudioStreamPacketDescription,
    given: bool,
}

/// `AudioConverterComplexInputDataProc`: supplies the input once, then reports no more data.
unsafe extern "C-unwind" fn feed_input(
    _converter: AudioConverterRef,
    io_packets: NonNull<u32>,
    list: NonNull<AudioBufferList>,
    descriptions: *mut *mut AudioStreamPacketDescription,
    user: *mut c_void,
) -> i32 {
    // SAFETY: AudioToolbox rule: `user` is the `Input` passed to `FillComplexBuffer`, alive for
    // the duration of that call, and the proc runs synchronously inside it.
    let input = unsafe { &mut *user.cast::<Input<'_>>() };
    // SAFETY: the converter hands a list with at least one buffer; we only fill the first.
    let list = unsafe { list.as_ptr().as_mut() };
    let Some(list) = list else {
        return NO_MORE_INPUT;
    };
    if input.given {
        // SAFETY: `io_packets` is the converter's own out-parameter.
        unsafe {
            *io_packets.as_ptr() = 0;
        }
        return NO_MORE_INPUT;
    }
    input.given = true;
    list.mNumberBuffers = 1;
    list.mBuffers[0] = AudioBuffer {
        mNumberChannels: CHANNELS,
        mDataByteSize: u32_of(input.data.len()),
        mData: input.data.as_ptr().cast_mut().cast::<c_void>(),
    };
    if !descriptions.is_null() {
        input.packet.mDataByteSize = u32_of(input.data.len());
        // SAFETY: the converter asked for packet descriptions; it reads them within this call.
        unsafe {
            *descriptions = &raw mut input.packet;
        }
    }
    // SAFETY: as above.
    unsafe {
        *io_packets.as_ptr() = input.frames;
    }
    0
}

/// [`OpusEncoder`] as the worker's [`AudioEncoder`].
#[derive(Debug)]
pub struct Opus(OpusEncoder);

impl AudioEncoder for Opus {
    fn new() -> Result<Self, CodecError> {
        OpusEncoder::new().map(Self)
    }

    fn push(&mut self, samples: &[f32], out: impl FnMut(&[u8])) -> Result<(), CodecError> {
        self.0.push(samples, out)
    }
}

/// PCM in, Opus packets out.
pub struct OpusEncoder {
    converter: Converter,
    pending: VecDeque<f32>,
    packet: Vec<u8>,
}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("pending", &self.pending.len())
            .finish_non_exhaustive()
    }
}

impl OpusEncoder {
    /// A stereo 48 kHz encoder at [`BITRATE`].
    pub fn new() -> Result<Self, CodecError> {
        let mut converter = Converter::new(&pcm_format(), &opus_format())?;
        converter.set_u32(kAudioConverterEncodeBitRate, BITRATE)?;
        let max = converter.get_u32(kAudioConverterPropertyMaximumOutputPacketSize)?;
        let max = usize::try_from(max.max(1)).unwrap_or(1);
        Ok(Self {
            converter,
            pending: VecDeque::new(),
            packet: vec![0; max],
        })
    }

    /// Queue interleaved stereo samples and encode every whole 10 ms packet they complete.
    pub fn push(&mut self, samples: &[f32], mut out: impl FnMut(&[u8])) -> Result<(), CodecError> {
        self.pending.extend(samples);
        while self.pending.len() >= PACKET_SAMPLES {
            // Encoded in place: `make_contiguous` only moves samples once the queue has wrapped.
            let frame = self
                .pending
                .make_contiguous()
                .get(..PACKET_SAMPLES)
                .unwrap_or_default();
            let n = encode_one(&mut self.converter, &mut self.packet, frame)?;
            self.pending.drain(..PACKET_SAMPLES);
            if let Some(packet) = self.packet.get(..n).filter(|p| !p.is_empty()) {
                out(packet);
            }
        }
        Ok(())
    }
}

/// Encode one 10 ms packet of `frame` into `packet`; the packet's length, 0 when the converter held
/// it back.
fn encode_one(
    converter: &mut Converter,
    packet: &mut [u8],
    frame: &[f32],
) -> Result<usize, CodecError> {
    let bytes: &[u8] = bytemuck_cast(frame);
    let mut input = Input {
        data: bytes,
        frames: FRAME_SAMPLES,
        packet: AudioStreamPacketDescription {
            mStartOffset: 0,
            mVariableFramesInPacket: 0,
            mDataByteSize: 0,
        },
        given: false,
    };
    let mut packets: u32 = 1;
    let mut list = AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: CHANNELS,
            mDataByteSize: u32_of(packet.len()),
            mData: packet.as_mut_ptr().cast::<c_void>(),
        }],
    };
    let mut description = AudioStreamPacketDescription {
        mStartOffset: 0,
        mVariableFramesInPacket: 0,
        mDataByteSize: 0,
    };
    // SAFETY: AudioToolbox rule: the proc, its user data and the output list outlive the
    // call; the output buffer is at least `MaximumOutputPacketSize` bytes.
    let status = unsafe {
        AudioConverterFillComplexBuffer(
            converter.0,
            Some(feed_input),
            (&raw mut input).cast::<c_void>(),
            NonNull::from(&mut packets),
            NonNull::from(&mut list),
            &raw mut description,
        )
    };
    if status != 0 && status != NO_MORE_INPUT {
        return Err(CodecError::Os {
            call: "AudioConverterFillComplexBuffer",
            status,
        });
    }
    Ok(if packets == 0 {
        0
    } else {
        usize::try_from(description.mDataByteSize).unwrap_or(0)
    })
}

/// Opus packets in, interleaved stereo PCM out.
pub struct OpusDecoder {
    converter: Converter,
    pcm: Vec<f32>,
}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder").finish_non_exhaustive()
    }
}

impl OpusDecoder {
    /// A stereo 48 kHz decoder.
    pub fn new() -> Result<Self, CodecError> {
        let converter = Converter::new(&opus_format(), &pcm_format())?;
        Ok(Self {
            converter,
            pcm: vec![0.0; PACKET_SAMPLES * 3],
        })
    }

    /// Decode one packet; the samples are valid until the next call.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32], CodecError> {
        let mut input = Input {
            data: packet,
            frames: 1,
            packet: AudioStreamPacketDescription {
                mStartOffset: 0,
                mVariableFramesInPacket: 0,
                mDataByteSize: 0,
            },
            given: false,
        };
        let mut frames = u32_of(self.pcm.len().checked_div(CHANNELS as usize).unwrap_or(0));
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: CHANNELS,
                mDataByteSize: u32_of(self.pcm.len().saturating_mul(SAMPLE_BYTES)),
                mData: self.pcm.as_mut_ptr().cast::<c_void>(),
            }],
        };
        // SAFETY: as in the encoder; the output buffer holds `frames` frames of PCM.
        let status = unsafe {
            AudioConverterFillComplexBuffer(
                self.converter.0,
                Some(feed_input),
                (&raw mut input).cast::<c_void>(),
                NonNull::from(&mut frames),
                NonNull::from(&mut list),
                ptr::null_mut(),
            )
        };
        if status != 0 && status != NO_MORE_INPUT {
            return Err(CodecError::Os {
                call: "AudioConverterFillComplexBuffer",
                status,
            });
        }
        let n = usize::try_from(frames)
            .unwrap_or(0)
            .saturating_mul(CHANNELS as usize);
        Ok(self.pcm.get(..n).unwrap_or_default())
    }
}

/// Reinterpret a float slice as bytes (native endian, which is what the PCM format says).
const fn bytemuck_cast(frame: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding or invalid bit patterns; the byte view covers exactly the
    // slice's memory and shares its lifetime.
    unsafe { std::slice::from_raw_parts(frame.as_ptr().cast::<u8>(), size_of_val(frame)) }
}

/// What playback has done so far, for the stream's counters.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct PlayoutStats {
    /// Times the ring ran dry while it was playing sound. Running dry after silence is the
    /// host's gate closing and is not counted.
    pub underruns: u64,
    /// Audio dropped to shed delay: a stall's backlog at once, a drifting clock a slice at a time.
    pub trimmed: Duration,
    /// Audio played twice to add depth, a slice at a time.
    pub stretched: Duration,
    /// Depth the ring aims to hold when a packet arrives (`target_us`).
    pub target: Duration,
}

/// Samples waiting to be played, between the thread that decodes packets and the device's
/// I/O thread: a single-producer, single-consumer ring of atomics, allocated once.
///
/// The device's side ([`Ring::pull`]) is wait-free: a handful of atomic loads and stores, one
/// compare-and-swap it never retries, and a copy. It takes no lock, so a decoder descheduled
/// mid-packet cannot hold up a buffer. Everything that steers the depth is the decoder's
/// ([`Steer`]), done to a packet before it is published or to samples the device is not
/// reading.
///
/// Ownership, in positions that count samples since the ring was made:
/// - `write` is the decoder's. Samples below it are published; the decoder writes past it and
///   publishes with a release store.
/// - `read` is the device's. The decoder writes nothing at or past `read + RING_SAMPLES`.
/// - `run` says whether the device plays: its epoch times two, plus one while playing. The decoder
///   starts a run (a new, odd epoch word) and stops one when the stream is muted; the device stops
///   one when it runs dry. The epoch keeps a stale stop from ending a newer run.
/// - `from` is where the current run starts, the decoder's: a new run begins there, and a stopped
///   ring frees everything below it. While stopped, the samples from `from` on are the decoder's to
///   fade in.
///
/// Samples are `f32` bits in relaxed `AtomicU32`s, which on Apple silicon are the plain loads
/// and stores a copy would make: the ordering comes from `write`, `read` and `run`, and a race
/// the protocol rules out could not be undefined behaviour even if it happened.
struct Ring {
    samples: Box<[AtomicU32]>,
    write: AtomicUsize,
    read: AtomicUsize,
    from: AtomicUsize,
    run: AtomicU32,
    /// The device thread's own state, atomics so the callback needs only a shared reference:
    /// the `run` word it last played under, the last frame it gave the device, and how many
    /// frames of the ramp from that frame down to silence are still to play. A ring that runs
    /// dry at a buffer's edge has nothing left to fade, so the fade continues from what was
    /// last heard.
    seen: AtomicU32,
    tail: [AtomicU32; 2],
    ramp: AtomicUsize,
    /// The most frames the device has asked for in one render lately: its I/O buffer.
    render: RenderSize,
}

/// Frames of one window of the device's render sizes: two seconds of its own clock.
const RENDER_WINDOW_FRAMES: usize = SAMPLE_RATE as usize * 2;

/// The largest render the device has asked for lately: the most over the window of
/// [`RENDER_WINDOW_FRAMES`] now filling and the one before it, so a size seen once goes after two
/// to four seconds. One large render (an iPhone's screen locking asks 4 096 frames at a time, a
/// Mac's output moving to another device asks one large buffer) would otherwise hold the depth
/// it adds, 85 ms, for the rest of the player's life. A device that keeps asking that much keeps
/// it.
///
/// The device's thread alone writes it, wait-free: a load and a store of each word. The decoder
/// reads the two maxima as one word, so it never sees half a window's turn.
#[derive(Debug, Default)]
struct RenderSize {
    /// The largest render in the window now filling, low 32 bits, and in the one before, high.
    largest: AtomicU64,
    /// Frames rendered in the window now filling.
    rendered: AtomicUsize,
}

impl RenderSize {
    /// The device asked for `frames`; the device's side.
    fn note(&self, frames: usize) {
        let (mut now, mut before) = split(self.largest.load(Ordering::Relaxed));
        let mut rendered = self.rendered.load(Ordering::Relaxed);
        if rendered >= RENDER_WINDOW_FRAMES {
            (now, before, rendered) = (0, now, 0);
        }
        now = now.max(u32::try_from(frames).unwrap_or(u32::MAX));
        self.largest.store(
            (u64::from(before) << 32) | u64::from(now) | u64::from(now),
            Ordering::Relaxed,
        );
        self.rendered
            .store(rendered.saturating_add(frames), Ordering::Relaxed);
    }

    /// The largest render lately, 0 before the first.
    fn frames(&self) -> usize {
        let (now, before) = split(self.largest.load(Ordering::Relaxed));
        usize::try_from(now.max(before)).unwrap_or(usize::MAX)
    }
}

/// The two halves of a [`RenderSize::largest`] word: the window now filling's, then the last's.
const fn split(word: u64) -> (u32, u32) {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "each half is a u32 by construction"
    )]
    (word as u32, (word >> 32) as u32)
}

/// The decoder's side of the ring: when to start, and the depth, steered from the level the
/// device's `read` shows towards a fixed target (`target_us`).
///
/// The ring starts, and restarts after running dry, only once it holds the target and a packet
/// more, so the next packet, on time, finds the target. The depth an arrival reads is the
/// ring's level before the packet goes in. A recent depth that stays off the target is
/// corrected one [`SLICE_FRAMES`] slice per packet, dropped from the arriving packet or played
/// twice in it with a [`FADE_FRAMES`] crossfade, which is how a host clock running ±100 ppm off
/// the device's is absorbed. A depth past the target by more than [`BURST_US`] is the backlog a
/// stall releases, and is cut from each packet as it arrives, crossfaded, rather than kept as
/// lasting delay.
///
/// Only the thread that decodes packets takes it, never the device's callback, so a slice's
/// crossfade cannot hold up a buffer.
#[derive(Debug)]
struct Steer {
    /// A run this side started and has not seen end.
    playing: bool,
    /// The last run's epoch (see [`Ring`]).
    epoch: u32,
    /// The decoder's copies of `write` and `from`.
    write: usize,
    from: usize,
    /// Cutting a stall's backlog (see [`Steer::converge`]).
    cutting: bool,
    /// Depths the recent arrivals read, microseconds, newest last.
    recent: VecDeque<i64>,
    /// The last packet pushed had a sample above the host's silence floor.
    last_loud: bool,
    /// The arriving packet, sliced or stretched before it is published.
    packet: Vec<f32>,
    underruns: u64,
    trimmed_frames: u64,
    stretched_frames: u64,
}

/// Microseconds of audio in one packet, [`FRAME_SAMPLES`] at [`SAMPLE_RATE`].
const PACKET_US: i64 = 10_000;
/// Microseconds of one device buffer until the device has rendered (a Mac's 512 frames, give or
/// take).
const DEVICE_US: i64 = 10_000;
/// The depth the ring holds for late packets and the device's buffer: 40 ms. Fixed, where an
/// adaptive target followed the lateness percentile between 20 and 120 ms: on a tailnet a
/// packet runs a few milliseconds late, a keyframe on the link holds sound back by tens of
/// them, and 40 ms covers both and a 1 024-frame render (21 ms) beside them, for a few
/// milliseconds more delay than the adaptive target held on a clean path and none of its
/// estimate, rebase and stall bookkeeping. 15 ms ran the ring dry on scheduling alone
/// (`docs/MEASUREMENTS.md`, 2026-09-29, "audio: a smaller device buffer and 10 ms packets").
const JITTER_TARGET_US: i64 = 40_000;
/// Depth past the target by more than this is a stall's backlog, cut in one go.
const BURST_US: i64 = 40_000;
/// Depths averaged for a slice's decision (half a second of packets), and how many it waits for.
const RECENT: usize = 50;
const RECENT_MIN: usize = 20;
/// Frames dropped or played twice per correction: 5 ms, half a packet.
const SLICE_FRAMES: usize = 240;
/// Frames each join is crossfaded over: 2.5 ms, long enough that no step is a click.
#[cfg(not(onicode_loom))]
const FADE_FRAMES: usize = 120;
/// One frame under loom, so the model stays small enough to explore every interleaving.
#[cfg(onicode_loom)]
const FADE_FRAMES: usize = 1;
/// A sample above this is sound: the host's gate floor, -80 dBFS.
const LOUD: f32 = 1e-4;

/// The depth the ring aims for: [`JITTER_TARGET_US`], or a device's render and a packet when the
/// device renders more at once than that covers (iOS asks 4 096 frames, 85 ms, with the screen
/// locked; see [`RenderSize`]).
fn target_us(device_us: i64) -> i64 {
    JITTER_TARGET_US.max(device_us.saturating_add(PACKET_US))
}

/// How far the recent depth may sit off the target before a slice corrects it: past half a
/// device buffer plus half a slice, so the device's phase against the packets' arrival and one
/// slice's correction cannot alternate a drop and a stretch.
fn deadband_us(device_us: i64) -> i64 {
    (device_us / 2).saturating_add(frames_us(SLICE_FRAMES) / 2)
}

/// Microseconds of audio in `frames` frames.
fn frames_us(frames: usize) -> i64 {
    i64::try_from(frames)
        .unwrap_or(i64::MAX)
        .saturating_mul(1_000_000)
        .checked_div(i64::from(SAMPLE_RATE))
        .unwrap_or(0)
}

/// Frames of audio in `us` microseconds, rounded down.
fn us_frames(us: i64) -> usize {
    usize::try_from(us.max(0).saturating_mul(i64::from(SAMPLE_RATE)) / 1_000_000).unwrap_or(0)
}

/// A duration from microseconds, zero for anything negative.
fn duration_us(us: i64) -> Duration {
    Duration::from_micros(u64::try_from(us).unwrap_or(0))
}

/// The crossfade's weight on the incoming side at frame `k`: never quite 0 or 1, so both ends
/// join their neighbours without a step.
#[expect(
    clippy::cast_precision_loss,
    reason = "fade lengths are a few hundred frames"
)]
fn fade_in_weight(k: usize) -> f32 {
    (k as f32 + 1.0) / (FADE_FRAMES as f32 + 1.0)
}

/// Whether a `run` word is a playing run's (see [`Ring`]).
const fn playing(run: u32) -> bool {
    run & 1 == 1
}

/// The `run` word of the same run, stopped.
const fn stopped(run: u32) -> u32 {
    run & !1
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ring")
            .field("run", &self.run.load(Ordering::Relaxed))
            .field("queued", &self.queued())
            .finish_non_exhaustive()
    }
}

/// What a cell holds before the decoder first writes it: silence, and under loom a NaN, which
/// no published sample, ramp or silence equals. A render that reads a cell whose publish it was
/// not shown (a missing release or acquire) then plays a frame the model rejects, where silence
/// would pass as the ring running dry (`ring_model`, and its canary: `cargo xtask deep canary
/// loom`).
#[cfg(not(onicode_loom))]
const UNWRITTEN: u32 = 0;
#[cfg(onicode_loom)]
const UNWRITTEN: u32 = f32::NAN.to_bits();

/// The order [`Steer::publish`] stores `write` in: `Release`, so the device's `Acquire` load of
/// it shows the samples stored before it. Under loom, `ONICODE_CANARY=loom` weakens it to
/// `Relaxed`, the defect the model must catch (`cargo xtask deep canary loom`).
#[cfg(not(onicode_loom))]
const fn publish_order() -> Ordering {
    Ordering::Release
}

#[cfg(onicode_loom)]
fn publish_order() -> Ordering {
    static WEAKENED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let weakened =
        *WEAKENED.get_or_init(|| std::env::var_os("ONICODE_CANARY").is_some_and(|v| v == "loom"));
    if weakened {
        Ordering::Relaxed
    } else {
        Ordering::Release
    }
}

impl Ring {
    /// An empty, stopped ring of [`RING_SAMPLES`] samples.
    fn new() -> Self {
        Self {
            samples: std::iter::repeat_with(|| AtomicU32::new(UNWRITTEN))
                .take(RING_SAMPLES)
                .collect(),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            from: AtomicUsize::new(0),
            run: AtomicU32::new(0),
            seen: AtomicU32::new(0),
            tail: [AtomicU32::new(0), AtomicU32::new(0)],
            ramp: AtomicUsize::new(0),
            render: RenderSize::default(),
        }
    }

    /// The `len` cells from position `pos` on, wrapping at the ring's end.
    fn cells(&self, pos: usize, len: usize) -> impl Iterator<Item = &AtomicU32> {
        let start = pos & RING_MASK;
        let first = len.min(RING_SAMPLES.saturating_sub(start));
        let head = self
            .samples
            .get(start..start.saturating_add(first))
            .unwrap_or_default();
        let wrapped = self
            .samples
            .get(..len.saturating_sub(first))
            .unwrap_or_default();
        head.iter().chain(wrapped)
    }

    /// Samples queued, both channels: published, and neither played nor discarded.
    fn queued(&self) -> usize {
        let from = self.from.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        self.write
            .load(Ordering::Acquire)
            .saturating_sub(read.max(from))
    }

    /// Microseconds of the device's buffer: the most it has asked for in one render lately
    /// ([`RenderSize`]), or [`DEVICE_US`] until it has rendered.
    fn device_us(&self) -> i64 {
        match self.render.frames() {
            0 => DEVICE_US,
            frames => frames_us(frames),
        }
    }

    /// A run is playing.
    fn playing(&self) -> bool {
        playing(self.run.load(Ordering::Acquire))
    }

    /// Fill `out` for the device; the device's side, wait-free.
    ///
    /// A run plays what is published. One that runs dry stops itself and ramps from the last
    /// sample it played down to silence, and the ring plays again once the decoder has filled
    /// it to the target and started a new run.
    fn pull(&self, out: &mut [f32]) {
        self.render.note(out.len() / SAMPLES_PER_FRAME);
        let run = self.run.load(Ordering::Acquire);
        let seen = self.seen.load(Ordering::Relaxed);
        let mut ramp = self.ramp.load(Ordering::Relaxed);
        let mut have = 0;
        if playing(run) {
            let mut read = self.read.load(Ordering::Relaxed);
            if run != seen {
                // A new run starts at its front; what came before it was discarded. The front
                // was stored before the run, whose acquire load above makes it visible.
                read = read.max(self.from.load(Ordering::Relaxed));
            }
            let write = self.write.load(Ordering::Acquire);
            let n = write.saturating_sub(read).min(out.len());
            for (slot, cell) in out.iter_mut().zip(self.cells(read, n)) {
                *slot = f32::from_bits(cell.load(Ordering::Relaxed));
            }
            if self.run.load(Ordering::Acquire) == run {
                have = n;
                self.read.store(read.saturating_add(n), Ordering::Release);
                if n < out.len() {
                    // Dry. A failed swap is the decoder's mute stopping the run first.
                    let _stopped = self.run.compare_exchange(
                        run,
                        stopped(run),
                        Ordering::AcqRel,
                        Ordering::Relaxed,
                    );
                    self.seen.store(stopped(run), Ordering::Relaxed);
                    ramp = FADE_FRAMES;
                } else {
                    self.seen.store(run, Ordering::Relaxed);
                }
            } else {
                // Muted while this render copied. A `write` the copy saw past the mute would
                // show the mute in the second load of `run`, so what was copied may be the
                // next run's: none of it plays, and the decoder has discarded the rest.
                self.seen.store(stopped(run), Ordering::Relaxed);
                ramp = FADE_FRAMES;
            }
        } else {
            if playing(seen) {
                // Muted since the last render.
                ramp = FADE_FRAMES;
            }
            self.seen.store(run, Ordering::Relaxed);
            // What the stop discarded is free for the decoder to write over.
            let from = self.from.load(Ordering::Acquire);
            if from > self.read.load(Ordering::Relaxed) {
                self.read.store(from, Ordering::Release);
            }
        }
        let channels = CHANNELS as usize;
        if let Some(last) = out
            .get(..have)
            .and_then(|played| played.chunks_exact(channels).last())
        {
            for (tail, &sample) in self.tail.iter().zip(last) {
                tail.store(sample.to_bits(), Ordering::Relaxed);
            }
        }
        let tail = self
            .tail
            .each_ref()
            .map(|t| f32::from_bits(t.load(Ordering::Relaxed)));
        for frame in out.get_mut(have..).unwrap_or_default().chunks_mut(channels) {
            #[expect(clippy::cast_precision_loss, reason = "a few hundred frames")]
            let gain = ramp as f32 / (FADE_FRAMES as f32 + 1.0);
            ramp = ramp.saturating_sub(1);
            for (sample, &tail) in frame.iter_mut().zip(&tail) {
                *sample = tail * gain;
            }
        }
        self.ramp.store(ramp, Ordering::Relaxed);
    }
}

impl Default for Steer {
    fn default() -> Self {
        Self {
            playing: false,
            epoch: 0,
            write: 0,
            from: 0,
            cutting: false,
            recent: VecDeque::with_capacity(RECENT + 1),
            last_loud: false,
            packet: Vec::with_capacity(PACKET_ROOM),
            underruns: 0,
            trimmed_frames: 0,
            stretched_frames: 0,
        }
    }
}

impl Steer {
    /// Microseconds of audio in the ring.
    fn level_us(ring: &Ring) -> i64 {
        frames_us(ring.queued() / SAMPLES_PER_FRAME)
    }

    /// The `run` word of this side's last run, playing.
    const fn run(&self) -> u32 {
        self.epoch.wrapping_shl(1) | 1
    }

    /// Take in a run the device ended by running dry since the last look: it is stopped, and
    /// counted an underrun when it ran dry under sound.
    fn observe(&mut self, ring: &Ring) {
        if !self.playing || ring.playing() {
            return;
        }
        self.playing = false;
        self.recent.clear();
        if self.last_loud {
            self.underruns = self.underruns.saturating_add(1);
        }
    }

    /// Queue one decoded packet and steer the depth.
    fn push(&mut self, ring: &Ring, pcm: &[f32]) {
        // A run that ran dry since the last packet is filled again from this one on.
        self.observe(ring);
        let device_us = ring.device_us();
        let target = target_us(device_us);
        let before = Self::level_us(ring);
        self.last_loud = pcm.iter().any(|s| s.abs() > LOUD);
        if self.playing {
            let level = before.saturating_add(frames_us(pcm.len() / SAMPLES_PER_FRAME));
            let mut packet = std::mem::take(&mut self.packet);
            packet.clear();
            packet.extend_from_slice(pcm);
            self.converge(&mut packet, before, target, level, device_us);
            self.publish(ring, &packet);
            self.packet = packet;
        } else {
            self.publish(ring, pcm);
            if Self::level_us(ring).saturating_sub(PACKET_US) >= target {
                // Full enough that the next packet, on time, finds the target.
                self.start(ring);
            }
        }
    }

    /// Write `pcm` past `write` and publish it. What finds no room, which only a device that
    /// has stopped rendering leaves, is dropped and counted as trimmed.
    fn publish(&mut self, ring: &Ring, pcm: &[f32]) {
        let channels = CHANNELS as usize;
        let read = ring.read.load(Ordering::Acquire);
        let room = RING_SAMPLES.saturating_sub(self.write.saturating_sub(read));
        let fits = pcm.len().min(room);
        let n = fits.saturating_sub(fits.checked_rem(channels).unwrap_or(0));
        for (cell, &sample) in ring.cells(self.write, n).zip(pcm) {
            cell.store(sample.to_bits(), Ordering::Relaxed);
        }
        self.write = self.write.saturating_add(n);
        ring.write.store(self.write, publish_order());
        let dropped = pcm.len().saturating_sub(n) / SAMPLES_PER_FRAME;
        self.trimmed_frames = self.trimmed_frames.saturating_add(dropped as u64);
    }

    /// Start a run from the front of what is queued, ramped up from silence. The device reads
    /// none of it until the run's word is stored.
    fn start(&mut self, ring: &Ring) {
        let channels = CHANNELS as usize;
        let front = ring.read.load(Ordering::Acquire).max(self.from);
        let fade = FADE_FRAMES
            .saturating_mul(channels)
            .min(self.write.saturating_sub(front));
        for (i, cell) in ring.cells(front, fade).enumerate() {
            let sample = f32::from_bits(cell.load(Ordering::Relaxed));
            let faded = sample * fade_in_weight(i / SAMPLES_PER_FRAME);
            cell.store(faded.to_bits(), Ordering::Relaxed);
        }
        self.from = front;
        ring.from.store(front, Ordering::Relaxed);
        self.epoch = self.epoch.wrapping_add(1);
        ring.run.store(self.run(), Ordering::Release);
        self.playing = true;
        self.recent.clear();
    }

    /// The stream is muted: the run stops, what is queued is discarded, and the ring fills to
    /// the target again when sound is wanted. The device ramps down from what it last played.
    fn hold(&mut self, ring: &Ring) {
        self.observe(ring);
        if self.playing
            && ring
                .run
                .compare_exchange(
                    self.run(),
                    stopped(self.run()),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_err()
        {
            // The device ran dry first.
            self.observe(ring);
        }
        // Stored after the stop: a device that sees this `from` sees the run stopped too.
        self.from = self.write;
        ring.from.store(self.write, Ordering::Release);
        self.playing = false;
        self.last_loud = false;
    }

    /// What playback has done, from the ring's counts and the target.
    fn stats(&mut self, ring: &Ring) -> PlayoutStats {
        self.observe(ring);
        let frames = |n: u64| {
            let us = n
                .saturating_mul(1_000_000)
                .checked_div(u64::from(SAMPLE_RATE));
            Duration::from_micros(us.unwrap_or(0))
        };
        PlayoutStats {
            underruns: self.underruns,
            trimmed: frames(self.trimmed_frames),
            stretched: frames(self.stretched_frames),
            target: duration_us(target_us(ring.device_us())),
        }
    }

    /// Move the depth towards the target, in the arriving `packet`: a stall's backlog at once,
    /// a drift one slice at a time. `level` is the ring's with the whole packet in it.
    fn converge(
        &mut self,
        packet: &mut Vec<f32>,
        depth: i64,
        target_us: i64,
        level: i64,
        device_us: i64,
    ) {
        let deadband = deadband_us(device_us);
        let excess = depth.saturating_sub(target_us);
        // A backlog arrives packet by packet, so the cut goes on with each packet until the
        // depth is back at the target. A packet gives all but the fade it joins over.
        self.cutting = excess > BURST_US || (self.cutting && excess > deadband);
        if self.cutting {
            let room = level
                .saturating_sub(device_us)
                .saturating_sub(frames_us(FADE_FRAMES));
            let most = (packet.len() / SAMPLES_PER_FRAME).saturating_sub(FADE_FRAMES);
            let cut = us_frames(excess.min(room)).min(most);
            if cut >= SLICE_FRAMES && drop_frames(packet, cut) {
                self.trimmed_frames = self.trimmed_frames.saturating_add(cut as u64);
            }
            self.recent.clear();
            return;
        }
        self.recent.push_back(depth);
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        if self.recent.len() < RECENT_MIN {
            return;
        }
        let sum = self
            .recent
            .iter()
            .fold(0_i64, |sum, &d| sum.saturating_add(d));
        let mean = sum
            .checked_div(i64::try_from(self.recent.len()).unwrap_or(1))
            .unwrap_or(0);
        let slice = frames_us(SLICE_FRAMES);
        let spare = level.saturating_sub(device_us) >= slice.saturating_add(frames_us(FADE_FRAMES));
        let shift = if mean.saturating_sub(target_us) > deadband
            && spare
            && drop_frames(packet, SLICE_FRAMES)
        {
            self.trimmed_frames = self.trimmed_frames.saturating_add(SLICE_FRAMES as u64);
            slice.saturating_neg()
        } else if target_us.saturating_sub(mean) > deadband && stretch_frames(packet, SLICE_FRAMES)
        {
            self.stretched_frames = self.stretched_frames.saturating_add(SLICE_FRAMES as u64);
            slice
        } else {
            return;
        };
        for d in &mut self.recent {
            *d = d.saturating_add(shift);
        }
    }
}

/// Drop `n` frames from the front of `buf`, crossfading what came before them into what follows.
fn drop_frames(buf: &mut Vec<f32>, n: usize) -> bool {
    let channels = CHANNELS as usize;
    let (cut, fade) = (
        n.saturating_mul(channels),
        FADE_FRAMES.saturating_mul(channels),
    );
    if buf.len() < cut.saturating_add(fade) {
        return false;
    }
    for k in 0..fade {
        let w = fade_in_weight(k / SAMPLES_PER_FRAME);
        let outgoing = buf.get(k).copied().unwrap_or(0.0);
        if let Some(incoming) = buf.get_mut(cut.saturating_add(k)) {
            *incoming = incoming.mul_add(w, outgoing * (1.0 - w));
        }
    }
    buf.drain(..cut);
    true
}

/// Play `n` frames at the front of `buf` twice: after them, crossfade back to its start. `n` is
/// at least a fade long, and `buf` has room for `n` more frames.
fn stretch_frames(buf: &mut Vec<f32>, n: usize) -> bool {
    let channels = CHANNELS as usize;
    let (span, fade) = (
        n.saturating_mul(channels),
        FADE_FRAMES.saturating_mul(channels),
    );
    let len = buf.len();
    if len < span.saturating_add(fade) || span < fade {
        return false;
    }
    // The packet moves up a span, leaving its first span in place ahead of itself. Its first
    // `fade` samples, now a span in, are crossfaded into what follows the span the first time,
    // a span later again. What follows the crossfade is the packet from the end of the fade.
    buf.resize(len.saturating_add(span), 0.0);
    buf.copy_within(..len, span);
    for k in 0..fade {
        let w = fade_in_weight(k / SAMPLES_PER_FRAME);
        let outgoing = buf
            .get(span.saturating_mul(2).saturating_add(k))
            .copied()
            .unwrap_or(0.0);
        if let Some(incoming) = buf.get_mut(span.saturating_add(k)) {
            *incoming = incoming.mul_add(w, outgoing * (1.0 - w));
        }
    }
    true
}

/// The output unit's subtype: on macOS the system's default output, which follows the device
/// the user picks.
#[cfg(target_os = "macos")]
const OUTPUT_SUBTYPE: u32 = kAudioUnitSubType_DefaultOutput;
/// The output unit's subtype on iOS: `kAudioUnitSubType_RemoteIO` ('rioc'), from
/// `AudioToolbox/AUComponent.h`. It sits in the header's iOS-only branch, which the objc2
/// bindings, generated from the macOS SDK, do not carry.
#[cfg(target_os = "ios")]
const OUTPUT_SUBTYPE: u32 = 0x7269_6f63;
/// Frames the macOS output device is asked to render at a time: 2.7 ms at 48 kHz, where the Mac
/// Studio's speakers run 512 (10.7 ms) unasked. A sample waits that buffer out before the DAC,
/// and the render callback is a copy that costs the same per frame at any size
/// (`docs/MEASUREMENTS.md`, 2026-09-29, "audio: a smaller device buffer and 10 ms packets").
#[cfg(target_os = "macos")]
const IO_FRAMES: u32 = 128;
/// Most frames a render may ask for: iOS asks up to 4 096 at a time with the screen locked, and a
/// unit asked for more than its maximum fails the render.
const MAX_FRAMES_PER_SLICE: u32 = 4096;
/// Samples the ring holds, allocated once: 341 ms, past the deepest it gets (the target over a
/// 4 096-frame render, 95 ms, and a packet more while it fills, a stall's backlog as it is being
/// cut, 60 ms of stand-in). A power of two, so a position finds its cell with a mask.
#[cfg(not(onicode_loom))]
const RING_SAMPLES: usize = 1 << 15;
/// Four frames under loom.
#[cfg(onicode_loom)]
const RING_SAMPLES: usize = 8;
const RING_MASK: usize = RING_SAMPLES - 1;
#[cfg(not(onicode_loom))]
const _: () = assert!(RING_SAMPLES >= PACKET_SAMPLES * 26, "260 ms at least");
/// Room for the largest packet a decoder returns, played with a slice twice.
const PACKET_ROOM: usize = PACKET_SAMPLES * 3 + SLICE_FRAMES * SAMPLES_PER_FRAME;

/// An output audio unit playing interleaved stereo float at 48 kHz from the decoder's ring.
///
/// Its render callback runs on the device's I/O thread and fills each I/O buffer straight from
/// the ring, so nothing is queued between the ring and the device.
pub struct Player {
    unit: AudioUnit,
    ring: Arc<Ring>,
    /// Taken by whoever pushes packets, never by the callback. Boxed so a player stays small
    /// beside the other states of a stream's audio.
    feed: Box<Mutex<Steer>>,
}

impl std::fmt::Debug for Player {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Player").finish_non_exhaustive()
    }
}

// SAFETY: an audio unit's property and start/stop calls may come from any thread; the ring is
// atomics only, and the decoder's side of it is behind a mutex.
unsafe impl Send for Player {}

/// `AURenderCallback`: fill the device's I/O buffer from the ring (see [`Ring::pull`]).
///
/// Wait-free: no lock, no allocation, no system call. It never waits on the thread that decodes
/// packets, whatever that thread is doing (see [`Ring`]).
///
/// `cargo xtask deep sanitize-realtime` builds it under `RealtimeSanitizer`, which aborts on any
/// allocation, lock or blocking call reached from here.
#[cfg_attr(onicode_rtsan, sanitize(realtime = "nonblocking"))]
unsafe extern "C-unwind" fn render(
    user: NonNull<c_void>,
    _flags: NonNull<AudioUnitRenderActionFlags>,
    _stamp: NonNull<AudioTimeStamp>,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList,
) -> i32 {
    // SAFETY: `user` is the `Arc<Ring>` leaked into the unit when it was opened, and reclaimed
    // in `Drop` only after the unit is stopped and disposed, which ends all renders.
    let ring = unsafe { user.cast::<Ring>().as_ref() };
    // SAFETY: AudioUnit rule: `ioData` is the unit's own list, valid for this call. The stream
    // format is interleaved, so the list has one buffer.
    let Some(list) = (unsafe { data.as_mut() }) else {
        return 0;
    };
    if list.mNumberBuffers == 0 {
        return 0;
    }
    let Some(buffer) = list.mBuffers.first_mut() else {
        return 0;
    };
    let Some(samples) = NonNull::new(buffer.mData.cast::<f32>()) else {
        return 0;
    };
    let capacity = usize::try_from(buffer.mDataByteSize).unwrap_or(0) / SAMPLE_BYTES;
    let want = usize::try_from(frames)
        .unwrap_or(0)
        .saturating_mul(CHANNELS as usize)
        .min(capacity);
    // SAFETY: the buffer holds `mDataByteSize` bytes of f32 samples, and `want` is within them.
    let out = unsafe { std::slice::from_raw_parts_mut(samples.as_ptr(), want) };
    ring.pull(out);
    #[cfg(onicode_rtsan)]
    rtsan_probe::allocate_if_armed();
    buffer.mDataByteSize = u32_of(want.saturating_mul(SAMPLE_BYTES));
    0
}

impl Player {
    /// Open the output unit and start it (it plays silence until samples arrive). On macOS the
    /// device is asked for `IO_FRAMES` a render first.
    pub fn new() -> Result<Self, CodecError> {
        let player = Self::open()?;
        #[cfg(target_os = "macos")]
        player.ask_io_frames(IO_FRAMES);
        player.start()?;
        Ok(player)
    }

    fn start(&self) -> Result<(), CodecError> {
        // SAFETY: AudioToolbox rule: start an initialised output unit.
        check("AudioOutputUnitStart", unsafe {
            AudioOutputUnitStart(self.unit)
        })
    }

    /// Ask the device the unit plays through for `frames` a render, within the range it takes. A
    /// device that refuses keeps its own size, which the player works at too, only later.
    #[cfg(target_os = "macos")]
    fn ask_io_frames(&self, frames: u32) {
        let zero = AudioValueRange {
            mMinimum: 0.0,
            mMaximum: 0.0,
        };
        let range = self.get(
            kAudioDevicePropertyBufferFrameSizeRange,
            kAudioUnitScope_Global,
            zero,
        );
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a device's frame counts, between 1 and a few thousand"
        )]
        let frames = range
            .filter(|r| r.mMinimum > 0.0 && r.mMinimum <= r.mMaximum)
            .map_or(frames, |r| {
                f64::from(frames).max(r.mMinimum).min(r.mMaximum) as u32
            });
        if let Err(error) = self.set(
            kAudioDevicePropertyBufferFrameSize,
            kAudioUnitScope_Global,
            &frames,
        ) {
            tracing::debug!(%error, frames, "the output device keeps its own I/O buffer size");
        }
    }

    /// One property of the unit's output element (element 0), `None` when the unit has none.
    #[cfg(target_os = "macos")]
    fn get<T: Copy>(&self, property: u32, scope: u32, zero: T) -> Option<T> {
        let mut value = zero;
        let mut size = u32_of(size_of::<T>());
        // SAFETY: AudioToolbox rule: a live unit, and an out buffer of the stated size for a
        // property whose documented type is `T`.
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                property,
                scope,
                0,
                NonNull::from(&mut value).cast::<c_void>(),
                NonNull::from(&mut size),
            )
        };
        (status == 0).then_some(value)
    }

    /// The output unit found, given the ring's format and callback, and initialised, but not
    /// started: nothing reaches the device yet.
    fn open() -> Result<Self, CodecError> {
        let mut description = AudioComponentDescription {
            componentType: kAudioUnitType_Output,
            componentSubType: OUTPUT_SUBTYPE,
            componentManufacturer: kAudioUnitManufacturer_Apple,
            componentFlags: 0,
            componentFlagsMask: 0,
        };
        // SAFETY: AudioToolbox rule: a null component searches from the first; the description
        // is valid for the call.
        let component =
            unsafe { AudioComponentFindNext(ptr::null_mut(), NonNull::from(&mut description)) };
        if component.is_null() {
            return Err(CodecError::Os {
                call: "AudioComponentFindNext",
                status: kAudioComponentErr_UnsupportedType,
            });
        }
        let mut unit: AudioUnit = ptr::null_mut();
        // SAFETY: AudioToolbox rule: a component the system returned and an out pointer.
        let status = unsafe { AudioComponentInstanceNew(component, NonNull::from(&mut unit)) };
        check("AudioComponentInstanceNew", status)?;
        let ring = Arc::new(Ring::new());
        let user = Arc::into_raw(Arc::clone(&ring)).cast_mut().cast::<c_void>();
        // From here `Drop` disposes the unit and reclaims `user`.
        let player = Self {
            unit,
            ring,
            feed: Box::default(),
        };
        player.set(
            kAudioUnitProperty_StreamFormat,
            kAudioUnitScope_Input,
            &pcm_format(),
        )?;
        player.set(
            kAudioUnitProperty_MaximumFramesPerSlice,
            kAudioUnitScope_Global,
            &MAX_FRAMES_PER_SLICE,
        )?;
        let callback = AURenderCallbackStruct {
            inputProc: Some(render),
            inputProcRefCon: user,
        };
        player.set(
            kAudioUnitProperty_SetRenderCallback,
            kAudioUnitScope_Input,
            &callback,
        )?;
        // SAFETY: AudioToolbox rule: initialise a configured unit once, before starting it.
        check("AudioUnitInitialize", unsafe {
            AudioUnitInitialize(player.unit)
        })?;
        Ok(player)
    }

    /// Set one property on the unit's output element (element 0).
    fn set<T>(&self, property: u32, scope: u32, value: &T) -> Result<(), CodecError> {
        // SAFETY: AudioToolbox rule: a live unit, and a value of the property's documented type
        // and stated size, which the unit copies during the call.
        let status = unsafe {
            AudioUnitSetProperty(
                self.unit,
                property,
                scope,
                0,
                ptr::from_ref(value).cast::<c_void>(),
                u32_of(size_of::<T>()),
            )
        };
        check("AudioUnitSetProperty", status)
    }

    /// Queue one decoded packet.
    pub fn push(&self, samples: &[f32]) {
        self.feed.lock().push(&self.ring, samples);
    }

    /// Queue stand-ins for lost packets (see [`LossFade`]); they take the gap's time.
    pub fn conceal(&self, samples: &[f32]) {
        self.feed.lock().publish(&self.ring, samples);
    }

    /// A packet that arrived but is not to be heard (the stream is muted): what is queued goes,
    /// and the ring fills to the target again when sound is wanted.
    pub fn hold(&self) {
        self.feed.lock().hold(&self.ring);
    }

    /// What playback has done so far.
    #[must_use]
    pub fn stats(&self) -> PlayoutStats {
        self.feed.lock().stats(&self.ring)
    }

    /// Samples waiting to be played, both channels.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.ring.queued()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // SAFETY: AudioToolbox rule: stopping an output unit from outside its I/O thread returns
        // once the render in flight has finished; stopping one that never started only returns
        // an error.
        let _stopped = unsafe { AudioOutputUnitStop(self.unit) };
        // SAFETY: AudioToolbox rule: a live unit; one never initialised only returns an error.
        let _uninitialised = unsafe { AudioUnitUninitialize(self.unit) };
        // SAFETY: AudioToolbox rule: dispose once, last; a disposed unit renders no more.
        let _disposed = unsafe { AudioComponentInstanceDispose(self.unit) };
        let user = Arc::as_ptr(&self.ring);
        // SAFETY: this is the clone `open` leaked with `Arc::into_raw`; no render runs any more.
        drop(unsafe { Arc::from_raw(user) });
    }
}

/// The ring under loom: `cargo xtask deep loom`.
#[cfg(onicode_loom)]
#[cfg(test)]
mod ring_model;

#[cfg(test)]
#[cfg(not(onicode_loom))]
mod tests {
    use super::*;

    #[test]
    fn encodes_and_decodes_a_tone() {
        let mut enc = OpusEncoder::new().unwrap();
        let mut dec = OpusDecoder::new().unwrap();
        // 100 ms of a 440 Hz tone, stereo interleaved.
        let frames = SAMPLE_RATE as usize / 10;
        #[expect(clippy::cast_precision_loss, reason = "small test counts")]
        let pcm: Vec<f32> = (0..frames)
            .flat_map(|i| {
                let s = (i as f32 * 440.0 * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.5;
                [s, s]
            })
            .collect();
        let mut packets = Vec::new();
        enc.push(&pcm, |p| packets.push(p.to_vec())).unwrap();
        assert!(packets.len() >= 4, "got {} packets", packets.len());
        assert!(packets.iter().all(|p| !p.is_empty() && p.len() < 1200));
        let mut out = 0_usize;
        let mut energy = 0.0_f32;
        for p in &packets {
            let s = dec.decode(p).unwrap();
            out = out.saturating_add(s.len());
            energy += s.iter().map(|x| x * x).sum::<f32>();
        }
        // Opus pre-skip: the first packet comes back a few frames short.
        let whole = packets.len().saturating_mul(PACKET_SAMPLES);
        assert!(
            out >= whole.saturating_sub(PACKET_SAMPLES) && out <= whole,
            "{out}"
        );
        assert!(energy > 1.0, "decoded audio is silent: {energy}");
    }

    /// Silence: this opens the machine's real output, which may be carrying someone's audio.
    #[test]

    fn player_starts_and_drains() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::AUDIO_LIVE) {
            return;
        }
        let player = Player::new().unwrap();
        // Six packets, 60 ms: past the 40 ms and a packet the ring fills to before it starts.
        for _ in 1..=6 {
            player.push(&vec![0.0; PACKET_SAMPLES]);
        }
        assert!(player.queued() <= PACKET_SAMPLES * 6);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(player.queued(), 0);
    }

    /// What the default output does at the I/O buffer it runs unasked and at [`IO_FRAMES`]: the
    /// size in effect, the largest render the callback saw, HAL overloads (a missed I/O
    /// deadline, which is the audible glitch) and how many runs the ring started, over a minute
    /// of silent packets fed in real time at each size. Silence only: this is the machine's real
    /// output. `docs/MEASUREMENTS.md`, 2026-09-29, "audio: a smaller device buffer and 10 ms
    /// packets".
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "a measurement: renders silence on this Mac's default output for two minutes"]
    fn device_io_buffer() {
        use std::sync::atomic::AtomicU64;
        use std::time::Instant;

        use objc2_audio_toolbox::kAudioOutputUnitProperty_CurrentDevice;
        use objc2_core_audio::{
            AudioObjectAddPropertyListener, AudioObjectID, AudioObjectPropertyAddress,
            AudioObjectRemovePropertyListener, kAudioDeviceProcessorOverload,
            kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal,
        };

        static OVERLOADS: AtomicU64 = AtomicU64::new(0);
        unsafe extern "C-unwind" fn overload(
            _device: AudioObjectID,
            _count: u32,
            _addresses: NonNull<AudioObjectPropertyAddress>,
            _user: *mut c_void,
        ) -> i32 {
            OVERLOADS.fetch_add(1, Ordering::Relaxed);
            0
        }

        const SECONDS: u64 = 60;
        let packet_us = u64::try_from(PACKET_US).unwrap();
        for asked in [None, Some(IO_FRAMES)] {
            let player = Player::open().unwrap();
            if let Some(frames) = asked {
                player.ask_io_frames(frames);
            }
            let in_effect = player.get(
                kAudioDevicePropertyBufferFrameSize,
                kAudioUnitScope_Global,
                0_u32,
            );
            let device = player
                .get(
                    kAudioOutputUnitProperty_CurrentDevice,
                    kAudioUnitScope_Global,
                    0_u32,
                )
                .unwrap();
            let mut address = AudioObjectPropertyAddress {
                mSelector: kAudioDeviceProcessorOverload,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            };
            // SAFETY: CoreAudio HAL rule: a device the HAL named, an address valid for the call,
            // and a listener that lives for the whole process (a plain `fn`), removed below.
            let status = unsafe {
                AudioObjectAddPropertyListener(
                    device,
                    NonNull::from(&mut address),
                    Some(overload),
                    ptr::null_mut(),
                )
            };
            assert_eq!(status, 0, "AudioObjectAddPropertyListener");
            let before = OVERLOADS.load(Ordering::Relaxed);
            player.start().unwrap();
            let silent = vec![0.0_f32; PACKET_SAMPLES];
            let start = Instant::now();
            let packets = u32::try_from(SECONDS * 1_000_000 / packet_us).unwrap();
            for seq in 1..=packets {
                let due = start + Duration::from_micros(u64::from(seq) * packet_us);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                player.push(&silent);
            }
            let overloads = OVERLOADS.load(Ordering::Relaxed) - before;
            // SAFETY: as above, the same listener and address.
            let _removed = unsafe {
                AudioObjectRemovePropertyListener(
                    device,
                    NonNull::from(&mut address),
                    Some(overload),
                    ptr::null_mut(),
                )
            };
            let runs = player.feed.lock().epoch;
            eprintln!(
                "MEASURE device asked={asked:?} in_effect={in_effect:?} largest_render={} \
                 overloads={overloads} runs_started={runs} packet_us={PACKET_US} {:?}",
                player.ring.render.frames(),
                player.stats()
            );
        }
    }

    /// The output unit takes the ring's format and callback and initialises, and is not
    /// started, so nothing reaches the device. The ring is allocated whole, stopped and empty.
    #[test]
    fn the_output_unit_opens_without_starting() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::AUDIO_LIVE) {
            return;
        }
        let player = Player::open().unwrap();
        assert_eq!(player.ring.samples.len(), RING_SAMPLES);
        assert!(!player.ring.playing());
        assert_eq!(player.queued(), 0);
    }
}

#[cfg(test)]
#[expect(
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    reason = "test signals: small sample counts and bounded arithmetic"
)]
mod loss_fade_tests {
    use super::*;

    /// A stereo sine of `hz` at half scale, frames `from..to`.
    fn sine(hz: f32, from: usize, to: usize) -> Vec<f32> {
        (from..to)
            .flat_map(|i| {
                let s = (i as f32 * hz * std::f32::consts::TAU / SAMPLE_RATE as f32).sin() * 0.5;
                [s, s]
            })
            .collect()
    }

    /// Play `packets` packets of a 170 Hz tone through `f`, as decoded.
    fn play(f: &mut LossFade, packets: usize) -> Vec<f32> {
        let mut played = Vec::new();
        for k in 0..packets {
            let pcm = sine(170.0, k * PACKET_FRAMES, (k + 1) * PACKET_FRAMES);
            assert_eq!(f.take(&pcm), &*pcm, "no gap: played as decoded");
            played.extend_from_slice(&pcm);
        }
        played
    }

    /// The largest step between neighbouring samples of the left channel.
    fn max_step(samples: &[f32]) -> f32 {
        samples
            .as_chunks::<SAMPLES_PER_FRAME>()
            .0
            .windows(2)
            .map(|pair| (pair[1][0] - pair[0][0]).abs())
            .fold(0.0, f32::max)
    }

    /// A gap is stood in for by the last frame played fading to silence over the fade, then
    /// silence for the rest of the gap's time; no step is larger than the tone's own.
    #[test]
    fn a_gap_fades_to_silence_and_keeps_its_time() {
        let mut f = LossFade::default();
        let played = play(&mut f, 3);
        let last = played[played.len() - SAMPLES_PER_FRAME..].to_vec();
        let mut out = Vec::new();
        f.fill(2, &mut out);
        assert_eq!(out.len(), 2 * PACKET_SAMPLES, "the gap's time");
        assert!(
            (out[0] - last[0] * (1.0 - ramp(0, LOSS_FADE_FRAMES))).abs() < 1e-6,
            "from the last frame played"
        );
        let fade = LOSS_FADE_FRAMES * SAMPLES_PER_FRAME;
        assert!(out[fade..].iter().all(|&s| s == 0.0), "then silence");
        let mut joined = played;
        joined.extend_from_slice(&out);
        assert!(
            max_step(&joined) <= max_step(&sine(170.0, 0, PACKET_FRAMES)) + 1e-3,
            "no click into the fade"
        );
    }

    /// The packet after a gap fades in from silence over the fade and is played as decoded after
    /// it; once.
    #[test]
    fn the_next_packet_fades_in() {
        let mut f = LossFade::default();
        play(&mut f, 3);
        let mut out = Vec::new();
        f.fill(1, &mut out);
        let next = sine(170.0, 4 * PACKET_FRAMES, 5 * PACKET_FRAMES);
        let played = f.take(&next).to_vec();
        let fade = LOSS_FADE_FRAMES * SAMPLES_PER_FRAME;
        assert!(
            (played[0] - next[0] * ramp(0, LOSS_FADE_FRAMES)).abs() < 1e-6,
            "starts near silence"
        );
        assert_eq!(&played[fade..], &next[fade..], "then the packet as decoded");
        assert_eq!(f.take(&next), &*next, "one fade-in per gap");
    }

    /// Nothing played yet, no gap, or a gap too long to stand in for: nothing, and after a long
    /// gap nothing until something is played again.
    #[test]
    fn nothing_stands_in_without_sound_or_past_the_cap() {
        let mut out = Vec::new();
        LossFade::default().fill(1, &mut out);
        assert_eq!(out, Vec::<f32>::new());
        let mut f = LossFade::default();
        play(&mut f, 1);
        f.fill(0, &mut out);
        f.fill(MAX_CONCEALED + 1, &mut out);
        assert_eq!(out, Vec::<f32>::new());
        f.fill(1, &mut out);
        assert!(out.is_empty(), "the pause forgot what came before it");
        f.fill(MAX_CONCEALED, &mut out);
        assert!(out.is_empty());
        play(&mut f, 1);
        f.fill(MAX_CONCEALED, &mut out);
        assert_eq!(
            out.len(),
            MAX_CONCEALED as usize * PACKET_SAMPLES,
            "the cap"
        );
    }
}

/// The deep lane's proof that `RealtimeSanitizer` watches [`render`]: once a test arms it, the next
/// render allocates, and the sanitizer must abort the process.
#[cfg(onicode_rtsan)]
mod rtsan_probe {
    use std::sync::atomic::{AtomicBool, Ordering};

    pub(super) static ARMED: AtomicBool = AtomicBool::new(false);

    pub(super) fn allocate_if_armed() {
        if ARMED.load(Ordering::Relaxed) {
            std::hint::black_box(Vec::<f32>::with_capacity(64));
        }
    }
}

#[cfg(test)]
#[cfg(not(onicode_loom))]
#[expect(
    clippy::arithmetic_side_effects,
    reason = "test fixture arithmetic on traces of a few minutes"
)]
mod latency_tests {
    use std::time::Instant;

    use super::*;

    /// The player's two halves, driven the way [`Player`] drives them.
    struct Playout {
        feed: Steer,
        ring: Ring,
    }

    impl Default for Playout {
        fn default() -> Self {
            Self {
                feed: Steer::default(),
                ring: Ring::new(),
            }
        }
    }

    impl Playout {
        fn push(&mut self, pcm: &[f32]) {
            self.feed.push(&self.ring, pcm);
        }

        fn hold(&mut self) {
            self.feed.hold(&self.ring);
        }

        fn pull(&self, out: &mut [f32]) {
            self.ring.pull(out);
        }

        fn stats(&mut self) -> PlayoutStats {
            self.feed.stats(&self.ring)
        }
    }

    /// A 440 Hz tone at half scale, continuous across packets, so any step in the output larger
    /// than the tone's own is a join the playout made.
    fn tone(seq: u32) -> Vec<f32> {
        let first = u64::from(seq.saturating_sub(1)) * u64::from(FRAME_SAMPLES);
        (0..u64::from(FRAME_SAMPLES))
            .flat_map(|i| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "sample indices stay far below 2^52"
                )]
                let t = (first + i) as f64 / f64::from(SAMPLE_RATE);
                #[expect(clippy::cast_possible_truncation, reason = "a sample")]
                let s = ((t * 440.0 * std::f64::consts::TAU).sin() * 0.5) as f32;
                [s, s]
            })
            .collect()
    }

    /// The largest step between neighbouring samples of that tone.
    fn tone_step() -> f32 {
        let one = tone(1);
        one.chunks(2)
            .zip(one.chunks(2).skip(1))
            .map(|(a, b)| (b[0] - a[0]).abs())
            .fold(0.0, f32::max)
    }

    /// What a run did.
    #[derive(Debug, Default)]
    struct Run {
        /// Largest step between neighbouring output samples, left channel.
        max_step: f32,
        /// How far behind the worker the ring holds the listener at each device pull, ms. The
        /// callback writes straight into the device's I/O buffer, so the ring is all the player
        /// adds; the device's own terms come on top (17.3 ms on the Mac Studio's speakers,
        /// `tests/audio_latency.rs`).
        heard: Vec<(i64, i64)>,
        /// The depth the ring aimed at after each device pull, µs.
        targets: Vec<(i64, i64)>,
        stats: PlayoutStats,
    }

    impl Run {
        fn mean_heard(&self, from_us: i64, to_us: i64) -> i64 {
            let span: Vec<i64> = self
                .heard
                .iter()
                .filter(|&&(t, _)| t >= from_us && t < to_us)
                .map(|&(_, ms)| ms)
                .collect();
            span.iter().sum::<i64>() / i64::try_from(span.len().max(1)).unwrap()
        }

        fn max_heard(&self, from_us: i64, to_us: i64) -> i64 {
            self.heard
                .iter()
                .filter(|&&(t, _)| t >= from_us && t < to_us)
                .map(|&(_, ms)| ms)
                .max()
                .unwrap_or(0)
        }
    }

    /// Frames a render asks for: what the player asks a Mac's output device for (`IO_FRAMES`).
    const DEVICE_FRAMES: usize = 128;

    /// Play `arrivals` (arrival µs, sequence), in arrival order, against a device that renders
    /// [`DEVICE_FRAMES`] at a time on its own 48 kHz clock, until `end_us`. Driven through the
    /// same `push` and `pull` the player runs; the clock is the trace's, so nothing sleeps.
    fn play(arrivals: &[(i64, u32)], end_us: i64) -> Run {
        play_with(arrivals, end_us, (0, 0), &|_| DEVICE_FRAMES)
    }

    /// [`play`], with the packets arriving in `muted` (from, to) held rather than played.
    fn play_muted(arrivals: &[(i64, u32)], end_us: i64, muted: (i64, i64)) -> Run {
        play_with(arrivals, end_us, muted, &|_| DEVICE_FRAMES)
    }

    /// [`play_muted`] against a device whose render at each time (µs) asks `frames(time)`.
    fn play_with(
        arrivals: &[(i64, u32)],
        end_us: i64,
        muted: (i64, i64),
        frames: &dyn Fn(i64) -> usize,
    ) -> Run {
        let mut playout = Playout::default();
        let mut run = Run::default();
        let mut buffer = vec![0.0_f32; MAX_FRAMES_PER_SLICE as usize * CHANNELS as usize];
        let mut last = 0.0_f32;
        let mut next = arrivals.iter().peekable();
        // The device's clock in frames, so a render period that is not a whole number of
        // microseconds does not drift against the worker's.
        let mut rendered = 0_usize;
        let mut pull_at = 5_000_i64;
        while pull_at < end_us {
            while let Some(&&(at_us, seq)) = next.peek().filter(|&&&(at, _)| at <= pull_at) {
                if (muted.0..muted.1).contains(&at_us) {
                    playout.hold();
                } else {
                    playout.push(&tone(seq));
                }
                next.next();
            }
            let asked = frames(pull_at);
            let out = &mut buffer[..asked * CHANNELS as usize];
            playout.pull(out);
            for frame in out.chunks(2) {
                run.max_step = run.max_step.max((frame[0] - last).abs());
                last = frame[0];
            }
            let queued = playout.ring.queued() / CHANNELS as usize;
            run.heard.push((pull_at, frames_us(queued) / 1000));
            run.targets
                .push((pull_at, target_us(playout.ring.device_us())));
            rendered += asked;
            pull_at = 5_000 + frames_us(rendered);
        }
        run.stats = playout.stats();
        run
    }

    /// A deterministic scatter of `0..spread_us` per packet (a 64-bit LCG).
    fn scatter(seq: u32, spread_us: i64) -> i64 {
        let x = u64::from(seq)
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        i64::try_from((x >> 33) % u64::try_from(spread_us.max(1)).unwrap()).unwrap()
    }

    /// Packets every 10 ms on a worker clock `ppm` parts per million fast, 3 ms of link delay
    /// and `spread_us` of scatter on top; a window `(from, to)` of the worker's clock in which
    /// the link held everything and let it go at `to`.
    fn trace(seconds: u32, ppm: i64, spread_us: i64, held: &[(i64, i64)]) -> Vec<(i64, u32)> {
        let per_second = u32::try_from(1_000_000 / PACKET_US).unwrap();
        let mut out: Vec<(i64, u32)> = (1..=seconds * per_second)
            .map(|seq| {
                let sent = i64::from(seq) * PACKET_US * (1_000_000 - ppm) / 1_000_000;
                let arrives = sent + 3_000 + scatter(seq, spread_us);
                let released = held
                    .iter()
                    .find(|&&(from, to)| sent >= from && sent < to)
                    .map(|&(_, to)| to);
                (released.map_or(arrives, |to| to.max(arrives)), seq)
            })
            .collect();
        out.sort_by_key(|&(at, seq)| (at, seq));
        out
    }

    /// Steady packets, a 250 ms stall the device keeps pulling through, the held packets in one
    /// burst, then steady again: one underrun, the backlog cut as it arrives and gone within a
    /// quarter of a second, no lasting delay, and no join sharper than the tone itself.
    #[test]
    fn a_stall_burst_does_not_leave_lasting_delay() {
        let run = play(&trace(3, 0, 0, &[(1_000_000, 1_250_000)]), 3_000_000);
        let (before, most_before, after_burst, most_after, settled) = (
            run.mean_heard(500_000, 1_000_000),
            run.max_heard(500_000, 1_000_000),
            run.max_heard(1_260_000, 1_400_000),
            run.max_heard(1_500_000, 3_000_000),
            run.mean_heard(1_500_000, 3_000_000),
        );
        eprintln!(
            "stall: {before} ms before, {after_burst} ms at most after the burst, {settled} ms mean over 1.5–3 s; {:?}, max step {:.4} (tone {:.4})",
            run.stats,
            run.max_step,
            tone_step()
        );
        assert_eq!(run.stats.underruns, 1, "the stall itself");
        assert!(
            after_burst <= 150,
            "the backlog is cut as it arrives: {after_burst} ms"
        );
        assert!(
            most_after <= most_before + 10,
            "and gone within a quarter of a second: {most_after} ms against {most_before} ms"
        );
        assert!(
            settled <= before + 10,
            "and no delay stays: {settled} ms against {before} ms"
        );
        assert!(
            run.max_step < 2.0 * tone_step(),
            "no click: {}",
            run.max_step
        );
    }

    /// A keyframe on the link every 2 s holds the audio behind it for 40 ms, on top of 3 ms of
    /// scatter: about the fixed target, so a burst may starve the ring once, never more, and the
    /// delay stays at the target.
    #[test]
    fn keyframe_bursts_starve_the_ring_at_most_once_each() {
        let bursts: Vec<(i64, i64)> = (1..10)
            .map(|k| (k * 2_000_000, k * 2_000_000 + 40_000))
            .collect();
        let run = play(&trace(20, 0, 3_000, &bursts), 20_000_000);
        let mean = run.mean_heard(3_000_000, 20_000_000);
        eprintln!(
            "keyframe bursts: {mean} ms mean, {} ms at most after 3 s; {:?}, max step {:.4}",
            run.max_heard(3_000_000, 20_000_000),
            run.stats,
            run.max_step
        );
        assert!(
            run.stats.underruns <= u64::try_from(bursts.len()).unwrap(),
            "bounded: {:?}",
            run.stats
        );
        assert!(mean <= 70, "{mean} ms");
        assert!(
            run.max_step < 2.0 * tone_step(),
            "no click: {}",
            run.max_step
        );
    }

    /// A worker clock 100 ppm fast, then one 100 ppm slow, for four minutes each: the depth is
    /// held by dropping or repeating a slice now and then, never by an underrun or a growing
    /// delay. Four minutes move a clock 24 ms, past the deadband either side of the target, so
    /// the slow clock too must be corrected by a slice whatever depth it started from.
    #[test]
    fn clock_drift_is_absorbed_by_slices() {
        for ppm in [100, -100] {
            let run = play(&trace(240, ppm, 1_000, &[]), 240_000_000);
            let (early, late) = (
                run.mean_heard(5_000_000, 15_000_000),
                run.mean_heard(230_000_000, 240_000_000),
            );
            eprintln!(
                "drift {ppm:+} ppm: {early} ms early, {late} ms late; {:?}, max step {:.4}",
                run.stats, run.max_step
            );
            assert_eq!(run.stats.underruns, 0, "{ppm:+} ppm: {:?}", run.stats);
            assert!(
                (late - early).abs() <= 10,
                "{ppm:+} ppm: {early} ms → {late} ms"
            );
            if ppm > 0 {
                assert!(
                    run.stats.trimmed > Duration::ZERO,
                    "a fast worker is trimmed: {:?}",
                    run.stats
                );
            } else {
                assert!(
                    run.stats.stretched > Duration::ZERO,
                    "a slow worker is stretched: {:?}",
                    run.stats
                );
            }
            assert!(
                run.max_step < 2.0 * tone_step(),
                "no click: {}",
                run.max_step
            );
        }
    }

    /// Muted, the host's gate closes for 400 ms and its sequence clock stands still; sound comes
    /// back after the unmute. The mute discarded the queue, so the ring fills to the target again
    /// and nothing is cut.
    #[test]
    fn a_gate_while_muted_leaves_no_lasting_cuts() {
        let gap_us = 400_000;
        let arrivals: Vec<(i64, u32)> = trace(6, 0, 1_000, &[])
            .into_iter()
            .map(|(at, seq)| {
                if seq > 250 {
                    (at + gap_us, seq)
                } else {
                    (at, seq)
                }
            })
            .collect();
        let end_us = 6_000_000 + gap_us;
        let run = play_muted(&arrivals, end_us, (2_000_000, 3_000_000));
        let settled = run.mean_heard(4_000_000, end_us);
        eprintln!(
            "gate while muted: {settled} ms mean from 4 s; {:?}, max step {:.4}",
            run.stats, run.max_step
        );
        assert_eq!(run.stats.underruns, 0, "{:?}", run.stats);
        assert!(
            run.stats.trimmed <= Duration::from_millis(50),
            "no lasting cuts: {:?}",
            run.stats
        );
        assert!(settled <= JITTER_TARGET_US / 1000 + 10, "{settled} ms");
        assert!(
            run.max_step < 2.0 * tone_step(),
            "no click at the mute: {}",
            run.max_step
        );
    }

    /// The ring runs dry after silence: that is the host's gate closing, not an underrun.
    #[test]
    fn the_silence_gate_is_not_an_underrun() {
        let mut playout = Playout::default();
        let mut out = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];
        let silent = vec![0.0_f32; PACKET_SAMPLES];
        for _ in 1..=20 {
            playout.push(&silent);
            playout.pull(&mut out);
            playout.pull(&mut out);
        }
        // Longer than the ring holds.
        for _ in 0..200 {
            playout.pull(&mut out);
        }
        assert!(!playout.ring.playing(), "ran dry");
        assert_eq!(playout.stats().underruns, 0, "{:?}", playout.stats());
    }

    /// A render asks for the device's I/O buffer and the callback fills it straight from the
    /// ring: on-time packets come out whole and in order, 512 frames at a time, with no
    /// underrun, no slice, and nothing written past the frames asked for.
    #[test]
    fn the_render_callback_plays_the_ring_in_order_at_the_devices_size() {
        let mut playout = Playout::default();
        // The ring starts on the fifth packet, and a slice waits for twenty depths after that.
        let packets = 12_u32;
        let packet_frames = FRAME_SAMPLES as usize;
        let input: Vec<f32> = (1..=packets).flat_map(tone).collect();
        let samples = DEVICE_FRAMES * CHANNELS as usize;
        // A frame more room than the render asks for, marked, to see the callback stop at it.
        let mut buffer = vec![f32::MAX; samples + CHANNELS as usize];
        let mut output = Vec::new();
        let mut flags = AudioUnitRenderActionFlags::empty();
        // SAFETY: a plain C struct for which all zeroes is a valid value.
        let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
        let (mut seq, mut rendered) = (1_u32, 0_usize);
        loop {
            // A packet arrives once the worker has captured all of it.
            while seq <= packets && seq as usize * packet_frames <= rendered {
                playout.push(&tone(seq));
                seq += 1;
            }
            let (playing, level) = (playout.ring.playing(), playout.ring.queued());
            if seq > packets && level < samples {
                break;
            }
            let mut list = AudioBufferList {
                mNumberBuffers: 1,
                mBuffers: [AudioBuffer {
                    mNumberChannels: CHANNELS,
                    mDataByteSize: u32_of(buffer.len() * SAMPLE_BYTES),
                    mData: buffer.as_mut_ptr().cast::<c_void>(),
                }],
            };
            let user = NonNull::from(&playout.ring).cast::<c_void>();
            // SAFETY: called as the unit calls it: the ring outlives the call, and the list's one
            // buffer holds the bytes it states.
            let status = unsafe {
                render(
                    user,
                    NonNull::from(&mut flags),
                    NonNull::from(&mut stamp),
                    0,
                    u32_of(DEVICE_FRAMES),
                    &raw mut list,
                )
            };
            assert_eq!(status, 0);
            assert_eq!(
                list.mBuffers[0].mDataByteSize as usize,
                samples * SAMPLE_BYTES
            );
            assert_eq!(
                buffer[samples..],
                [f32::MAX; CHANNELS as usize],
                "past the frames asked"
            );
            if playing {
                output.extend_from_slice(&buffer[..samples]);
            } else {
                assert!(
                    buffer[..samples].iter().all(|&s| s == 0.0),
                    "silence while filling"
                );
            }
            rendered += DEVICE_FRAMES;
        }
        let stats = playout.stats();
        eprintln!(
            "render callback: {} frames played of {}; {stats:?}",
            output.len() / 2,
            input.len() / 2
        );
        assert_eq!(stats.underruns, 0, "{stats:?}");
        assert_eq!(
            (stats.trimmed, stats.stretched),
            (Duration::ZERO, Duration::ZERO)
        );
        assert!(
            output.len() >= input.len() - samples,
            "{} of {}",
            output.len(),
            input.len()
        );
        // The first frames are the start's fade-in; everything after is the input, sample for
        // sample.
        let faded = FADE_FRAMES * CHANNELS as usize;
        assert_eq!(output[faded..], input[faded..output.len()]);
    }

    /// The device's thread never waits on the decoder's. With the decoder's whole side held by
    /// another thread, the render callback still plays what was queued, in order, runs dry,
    /// stops the run and ramps to silence, all while the lock stays held.
    #[test]
    fn the_render_callback_never_waits_on_the_decoder() {
        let player = Player::open().unwrap();
        // The run starts on the fifth packet: the target and a packet more.
        let packets = 5_u32;
        for seq in 1..=packets {
            player.push(&tone(seq));
        }
        assert!(player.ring.playing());
        let input: Vec<f32> = (1..=packets).flat_map(tone).collect();
        let feed = &*player.feed;
        let user = NonNull::from(&*player.ring).cast::<c_void>();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let mut output = Vec::new();
        let took = std::thread::scope(|s| {
            s.spawn(move || {
                let _feed = feed.lock();
                held_tx.send(()).unwrap();
                // Let go once the renders are done, or after 5 s if they wait for it.
                let _done = done_rx.recv_timeout(Duration::from_secs(5));
            });
            held_rx.recv().unwrap();
            let started = Instant::now();
            let mut buffer = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];
            while output.len() < input.len() + buffer.len() {
                render_once(user, &mut buffer);
                output.extend_from_slice(&buffer);
            }
            let took = started.elapsed();
            assert!(feed.is_locked(), "held throughout");
            done_tx.send(()).unwrap();
            took
        });
        assert!(
            took < Duration::from_secs(1),
            "the renders waited: {took:?}"
        );
        let faded = FADE_FRAMES * CHANNELS as usize;
        assert_eq!(output[faded..input.len()], input[faded..]);
        let last = input[input.len() - 1].abs();
        let ramp: Vec<f32> = output[input.len()..]
            .chunks(2)
            .map(|f| f[0].abs())
            .collect();
        assert!(
            ramp[0] < last && ramp.is_sorted_by(|a, b| a >= b),
            "a ramp down: {ramp:?}"
        );
        assert!(
            ramp[FADE_FRAMES..].iter().all(|&s| s == 0.0),
            "then silence"
        );
        assert!(!player.ring.playing(), "the device stopped the run itself");
        assert_eq!(player.stats().underruns, 1, "and the decoder counts it");
    }

    /// A mute stops the run at the next render, which ramps down from what it last played.
    /// What was queued is discarded, and the next run starts from the first packet after it.
    #[test]
    fn a_mute_discards_the_queue_and_the_next_run_starts_after_it() {
        let mut playout = Playout::default();
        let mut out = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];
        for seq in 1..=5 {
            playout.push(&tone(seq));
        }
        assert!(playout.ring.playing(), "the target and a packet");
        playout.pull(&mut out);
        let last = out[out.len() - 2];
        playout.hold();
        assert_eq!(playout.ring.queued(), 0, "discarded");
        playout.pull(&mut out);
        #[expect(clippy::cast_precision_loss, reason = "a fade's length")]
        let first = last * FADE_FRAMES as f32 / (FADE_FRAMES as f32 + 1.0);
        assert!((out[0] - first).abs() < 1e-6, "{} against {first}", out[0]);
        let ramp: Vec<f32> = out.chunks(2).map(|f| f[0].abs()).collect();
        assert!(ramp.is_sorted_by(|a, b| a >= b), "{ramp:?}");
        assert!(ramp[FADE_FRAMES..].iter().all(|&s| s == 0.0));
        for seq in 7..=11 {
            playout.push(&tone(seq));
        }
        assert!(playout.ring.playing(), "full again");
        playout.pull(&mut out);
        let next = tone(7);
        let faded = FADE_FRAMES * CHANNELS as usize;
        assert_eq!(out[faded..], next[faded..out.len()]);
        let first = next[0] * fade_in_weight(0);
        assert!((out[0] - first).abs() < 1e-6, "{} against {first}", out[0]);
        assert_eq!(playout.stats().underruns, 0);
    }

    /// Under `RealtimeSanitizer`, an allocation in [`render`] aborts: a child process runs
    /// [`rtsan_probe_allocates`] and must die of it.
    #[cfg(onicode_rtsan)]
    #[test]
    fn realtime_sanitizer_catches_an_allocation_in_render() {
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .args([
                "--exact",
                "audio::latency_tests::rtsan_probe_allocates",
                "--ignored",
            ])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "the allocation went unnoticed: {stderr}"
        );
        assert!(
            stderr.contains("RealtimeSanitizer") && stderr.contains("malloc"),
            "not RealtimeSanitizer's report: {stderr}"
        );
        assert!(
            stderr.contains("render"),
            "the report names the callback: {stderr}"
        );
    }

    /// Arms the probe and renders once: aborts under `RealtimeSanitizer`, by design.
    #[cfg(onicode_rtsan)]
    #[test]
    #[ignore = "aborts the process; realtime_sanitizer_catches_an_allocation_in_render runs it"]
    fn rtsan_probe_allocates() {
        let ring = Ring::new();
        let user = NonNull::from(&ring).cast::<c_void>();
        let mut buffer = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];
        rtsan_probe::ARMED.store(true, Ordering::Relaxed);
        render_once(user, &mut buffer);
    }

    /// One call of the render callback on a 512-frame buffer.
    fn render_once(user: NonNull<c_void>, buffer: &mut [f32]) {
        let mut flags = AudioUnitRenderActionFlags::empty();
        // SAFETY: a plain C struct for which all zeroes is a valid value.
        let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
        let mut list = AudioBufferList {
            mNumberBuffers: 1,
            mBuffers: [AudioBuffer {
                mNumberChannels: CHANNELS,
                mDataByteSize: u32_of(buffer.len() * SAMPLE_BYTES),
                mData: buffer.as_mut_ptr().cast::<c_void>(),
            }],
        };
        // SAFETY: called as the unit calls it: the ring outlives the call, and the list's one
        // buffer holds the bytes it states.
        let status = unsafe {
            render(
                user,
                NonNull::from(&mut flags),
                NonNull::from(&mut stamp),
                0,
                u32_of(DEVICE_FRAMES),
                &raw mut list,
            )
        };
        assert_eq!(status, 0);
    }

    /// What the render callback costs on 512-frame buffers: alone, with the decoder's pushes
    /// between renders on the same thread, and with them on a thread of their own that keeps
    /// the ring a few packets ahead of the renders. `docs/MEASUREMENTS.md`, 2026-09-28.
    /// `cargo xtask bench --filter render_cost` runs it.
    #[test]
    #[ignore = "a measurement; run with `cargo xtask bench`"]
    fn render_cost() {
        const RENDERS: usize = 200_000;
        let packets: Vec<Vec<f32>> = (1..=64).map(tone).collect();
        let mut buffer = vec![0.0_f32; DEVICE_FRAMES * CHANNELS as usize];

        let bench = onicode_desktop_testkit::bench::Bench::new("codec.render_cost");
        let mut playout = Playout::default();
        let mut same = bench.series("same_thread");
        let mut seq = 1_u32;
        for i in 0..RENDERS {
            while seq as usize * FRAME_SAMPLES as usize <= (i + 3) * DEVICE_FRAMES {
                playout.push(&packets[seq as usize % packets.len()]);
                seq += 1;
            }
            let user = NonNull::from(&playout.ring).cast::<c_void>();
            same.time(|| render_once(user, &mut buffer));
        }
        let same = same.report().unwrap();
        eprintln!(
            "same thread: p99.9 {} ns; {:?}",
            same.wall.p999,
            playout.stats()
        );

        let Playout { mut feed, ring } = Playout::default();
        let rendered = AtomicUsize::new(0);
        // The pushes run on their own thread while a render is timed, and the process's
        // instruction count would be theirs too.
        let mut beside = bench.series("decoder_thread_beside").wall_only();
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut seq = 1_u32;
                loop {
                    let done = rendered.load(Ordering::Acquire);
                    if done >= RENDERS {
                        break;
                    }
                    if seq as usize * FRAME_SAMPLES as usize <= (done + 3) * DEVICE_FRAMES {
                        feed.push(&ring, &packets[seq as usize % packets.len()]);
                        seq += 1;
                    } else {
                        std::thread::yield_now();
                    }
                }
            });
            for i in 0..RENDERS {
                let user = NonNull::from(&ring).cast::<c_void>();
                beside.time(|| render_once(user, &mut buffer));
                rendered.store(i + 1, Ordering::Release);
            }
        });
        let beside = beside.report().unwrap();
        eprintln!("decoder thread beside: p99.9 {} ns", beside.wall.p999);
    }

    /// One render far larger than the rest, as a Mac asks once when its output moves to another
    /// device, raises the target over it only for the render-size window: afterwards the target
    /// and the depth heard come back to the fixed one, and nothing more runs dry.
    #[test]
    fn one_large_render_does_not_hold_the_depth_up_for_good() {
        let big_at = 3_000_000;
        let frames = |at: i64| {
            if (big_at..big_at + 3_000).contains(&at) {
                4096
            } else {
                128
            }
        };
        let run = play_with(&trace(12, 0, 1_000, &[]), 12_000_000, (0, 0), &frames);
        let target_at = |at: i64| run.targets.iter().rev().find(|&&(t, _)| t <= at).unwrap().1;
        let (before, during, after) = (
            target_at(big_at),
            target_at(big_at + 500_000),
            target_at(12_000_000),
        );
        let (heard_before, heard_after) = (
            run.mean_heard(2_000_000, big_at),
            run.mean_heard(10_000_000, 12_000_000),
        );
        eprintln!(
            "one 4096-frame render: target {before} µs before, {during} µs during, {after} µs \
             after; {heard_before} ms heard before, {heard_after} ms after; {:?}",
            run.stats
        );
        assert!(
            during >= frames_us(4096),
            "the large render is covered while it is recent"
        );
        assert!(after <= before + 2_000, "{after} µs against {before} µs");
        assert!(
            heard_after <= heard_before + 3,
            "{heard_after} ms against {heard_before} ms"
        );
        assert!(
            run.stats.underruns <= 1,
            "the large render alone: {:?}",
            run.stats
        );
        assert!(
            run.max_step < 2.0 * tone_step(),
            "no click: {}",
            run.max_step
        );
    }

    /// iOS renders 1 024 frames at a time unless the session asks for fewer, twice what a Mac
    /// device takes. The fixed target still covers a render while packets arrive every 10 ms.
    #[test]
    fn a_device_rendering_1024_frames_is_not_starved() {
        for frames in [512, 1024] {
            let run = play_with(&trace(20, 0, 1_000, &[]), 20_000_000, (0, 0), &|_| frames);
            eprintln!(
                "{frames}-frame renders: {} ms mean after 3 s; {:?}",
                run.mean_heard(3_000_000, 20_000_000),
                run.stats
            );
            assert_eq!(run.stats.underruns, 0, "{frames}: {:?}", run.stats);
            assert!(
                run.stats.target >= duration_us(frames_us(frames)),
                "the depth covers the render it takes: {:?}",
                run.stats
            );
            assert!(
                run.max_step < 2.0 * tone_step(),
                "no click: {}",
                run.max_step
            );
        }
    }

    /// A packet is [`FRAME_SAMPLES`] of audio: [`PACKET_US`], what the start waits a packet for.
    #[test]
    fn a_packet_lasts_its_frames() {
        assert_eq!(frames_us(FRAME_SAMPLES as usize), PACKET_US);
    }

    /// A capture that hands the worker its audio 1 024 frames (21.3 ms) at a time sends the
    /// packets each chunk completes together: two most of the time, sometimes three. The fixed
    /// target covers that clumping, so nothing starves.
    #[test]
    fn capture_in_1024_frame_chunks_is_covered() {
        let chunk_us = frames_us(1024);
        let arrivals: Vec<(i64, u32)> = trace(20, 0, 1_000, &[])
            .into_iter()
            .map(|(at, seq)| {
                let sent = i64::from(seq) * PACKET_US;
                let delivered = (sent + chunk_us - 1) / chunk_us * chunk_us;
                (at + delivered - sent, seq)
            })
            .collect();
        let run = play(&arrivals, 20_000_000);
        eprintln!(
            "1024-frame capture chunks: {} ms mean after 3 s; {:?}",
            run.mean_heard(3_000_000, 20_000_000),
            run.stats
        );
        assert_eq!(run.stats.underruns, 0, "{:?}", run.stats);
        assert!(
            run.max_step < 2.0 * tone_step(),
            "no click: {}",
            run.max_step
        );
    }
}
