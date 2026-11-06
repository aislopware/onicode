//! Host side: split an encoded frame into datagrams and add parity.

use std::collections::VecDeque;

use bytes::{BufMut as _, Bytes, BytesMut};
use reed_solomon_simd::ReedSolomonEncoder;
use zerocopy::little_endian::{U16, U32, U64};
use zerocopy::{FromBytes as _, IntoBytes as _};

use crate::wire::{
    FRAME_PREFIX_BYTES, FramePrefix, HEADER_BYTES, Kind, MAX_PAYLOAD, MediaHeader, StreamId, flags,
};
use crate::{MediaError, Redundancy};

/// Most data fragments in one frame. With at most [`MAX_PARITY_FRAGMENTS`] parity fragments every
/// combination stays inside what the GF(2^16) engine supports (checked by a test).
pub const MAX_DATA_FRAGMENTS: usize = 32_768;
/// Most parity fragments in one frame (the header field is a byte).
pub const MAX_PARITY_FRAGMENTS: usize = 255;
/// Least parity on a frame once the link has shown loss: two fragments.
///
/// Above [`Redundancy::MIN`] a clump of two losses in a small frame is then repaired without a
/// round trip. Moonlight asks Sunshine for the same floor (`minRequiredFecPackets` 2, "Require at
/// least 2 FEC packets for small frames", `SdpGenerator.c`). On a clean link the floor stays one
/// fragment, which covers the lone loss such a link has and costs a busy screen nothing more.
/// video decisions, "Small frames carry two parity fragments on a lossy link".
pub const MIN_PARITY_FRAGMENTS: usize = 2;
/// Parity ratio when nothing is known about the link, in thousandths (Sunshine's 20 %).
pub(crate) const DEFAULT_PARITY_PERMILLE: u16 = 200;
/// Frames kept for retransmission. At 60 fps this is about half a second, more than any playout
/// window a NACK is still worth answering in.
pub(crate) const HISTORY_FRAMES: usize = 32;

/// One encoded frame handed to the packetizer.
#[derive(Clone, Copy, Debug)]
pub struct EncodedFrame<'a> {
    /// The bitstream: one access unit, opaque here.
    pub data: &'a [u8],
    /// IDR frame.
    pub keyframe: bool,
    /// Long-term-reference token the receiver must acknowledge, if the encoder marked one.
    pub ltr_token: Option<u64>,
    /// Encoded as a recovery frame from an acknowledged LTR.
    pub ltr_refresh: bool,
    /// Nothing later refers to this frame (the encoder's temporal layer 1). Its datagrams, and
    /// the next frame's, say so, and a receiver that cannot repair it skips it instead of asking
    /// for a refresh. Ignored on a keyframe, a refresh or a frame with an `ltr_token`: later
    /// frames are predicted from those.
    pub discardable: bool,
    /// Capture time, host monotonic microseconds (low 32 bits).
    pub capture_ts_us: u32,
    /// The stripes coded from this capture ([`FramePrefix::stripes`]); zero for a stream coded
    /// as one picture.
    pub stripes: u8,
}

/// How a frame is cut up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// Data fragments.
    pub data_count: u16,
    /// Payload bytes per fragment (data and parity alike); even, ≤ [`MAX_PAYLOAD`].
    pub shard_bytes: usize,
    /// Parity fragments.
    pub parity_count: u8,
}

/// Smallest payload we will ever cut to; below this the per-datagram overhead dominates.
pub(crate) const MIN_PAYLOAD: usize = 256;

/// Choose fragment count and size for `len` bitstream bytes at `parity_permille` redundancy.
///
/// `max_payload` caps the payload per datagram (clamped to `MIN_PAYLOAD..=MAX_PAYLOAD` and
/// rounded down to even). Fragments are balanced (all the same size, as small as the count allows)
/// so the padding in the last one never exceeds two bytes per fragment. Any non-zero ratio gets at
/// least one parity fragment, and at least [`MIN_PARITY_FRAGMENTS`] once it is above the clean-link
/// [`Redundancy::MIN`].
pub fn layout(len: usize, parity_permille: u16, max_payload: usize) -> Result<Layout, MediaError> {
    if len == 0 {
        return Err(MediaError::Empty);
    }
    let max_payload = max_payload.clamp(MIN_PAYLOAD, MAX_PAYLOAD) & !1;
    let total = len.saturating_add(FRAME_PREFIX_BYTES);
    let data_count = total.div_ceil(max_payload);
    if data_count > MAX_DATA_FRAGMENTS {
        return Err(MediaError::FrameTooLarge { len });
    }
    let shard_bytes = total
        .div_ceil(data_count)
        .next_multiple_of(2)
        .min(max_payload);
    let floor = if parity_permille > Redundancy::MIN {
        MIN_PARITY_FRAGMENTS
    } else {
        1
    };
    let parity_count = if parity_permille == 0 {
        0
    } else {
        data_count
            .saturating_mul(usize::from(parity_permille))
            .div_ceil(1000)
            .clamp(floor, MAX_PARITY_FRAGMENTS)
    };
    Ok(Layout {
        data_count: u16::try_from(data_count).unwrap_or(u16::MAX),
        shard_bytes,
        parity_count: u8::try_from(parity_count).unwrap_or(u8::MAX),
    })
}

/// A packetized frame: its datagrams in send order (data first, then parity).
#[derive(Clone, Debug)]
pub struct SentFrame {
    /// Frame number.
    pub frame: u32,
    /// Cut.
    pub layout: Layout,
    /// Ready-to-send datagrams.
    pub datagrams: Vec<Bytes>,
}

impl SentFrame {
    /// Bytes on the wire including parity.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.datagrams.iter().map(Bytes::len).sum()
    }
}

/// Turns encoded frames into datagrams; keeps recent frames for NACK replies.
pub struct Packetizer {
    stream: StreamId,
    next_frame: u32,
    parity_permille: u16,
    max_payload: usize,
    encoder: Option<ReedSolomonEncoder>,
    history: VecDeque<SentFrame>,
    datagrams_sent: u64,
    /// The last frame cut was [`flags::DISCARDABLE`]: the next one carries
    /// [`flags::PREV_DISCARDABLE`].
    previous_discardable: bool,
    /// The build of the sessions whose frames are cut now ([`FramePrefix::build`]).
    build: u8,
}

impl std::fmt::Debug for Packetizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Packetizer")
            .field("stream", &self.stream)
            .field("next_frame", &self.next_frame)
            .field("parity_permille", &self.parity_permille)
            .field("history", &self.history.len())
            .finish_non_exhaustive()
    }
}

impl Packetizer {
    /// A packetizer for one stream, numbering frames from zero.
    #[must_use]
    pub fn new(stream: StreamId) -> Self {
        Self {
            stream,
            next_frame: 0,
            parity_permille: DEFAULT_PARITY_PERMILLE,
            max_payload: MAX_PAYLOAD,
            encoder: None,
            history: VecDeque::with_capacity(HISTORY_FRAMES),
            datagrams_sent: 0,
            previous_discardable: false,
            build: 0,
        }
    }

    /// Parity ratio in thousandths of the data fragment count (0 disables FEC).
    pub const fn set_parity_permille(&mut self, permille: u16) {
        self.parity_permille = if permille > 1000 { 1000 } else { permille };
    }

    /// Current parity ratio in thousandths.
    #[must_use]
    pub const fn parity_permille(&self) -> u16 {
        self.parity_permille
    }

    /// Cap datagrams at `bytes` (header included, the link's channel byte not) when the path
    /// cannot carry [`MAX_DATAGRAM`](crate::wire::MAX_DATAGRAM): the link's MTU is fixed at
    /// 1232, and falls back to QUIC's 1200 on a path that black-holes it, where about 1149 bytes
    /// fit. The host reads the link's budget, less the channel byte, before every frame.
    pub const fn set_max_datagram(&mut self, bytes: usize) {
        self.max_payload = bytes.saturating_sub(HEADER_BYTES);
    }

    /// The frames cut from here on come from the sessions of `build` ([`FramePrefix::build`]).
    /// A retransmission is of a frame as it was cut, under its own build.
    pub const fn set_build(&mut self, build: u8) {
        self.build = build;
    }

    /// The number the next frame will get.
    #[must_use]
    pub const fn next_frame(&self) -> u32 {
        self.next_frame
    }

    /// Datagrams produced so far, retransmissions included.
    #[must_use]
    pub const fn datagrams_sent(&self) -> u64 {
        self.datagrams_sent
    }

    /// Cut `frame` into datagrams and add parity. `send_ms_lo` is the low byte of the host's
    /// millisecond clock, stamped on every datagram.
    ///
    /// `ship` is handed the data datagrams as soon as they are cut, before the parity is computed
    /// over them, and then the parity: Reed–Solomon over a keyframe takes longer than the first
    /// fragments need to be on their way (MEASUREMENTS, "data before parity").
    ///
    /// Every datagram, parity included, is written once into one buffer laid out as the wire
    /// wants it (header, shard, header, shard, …) and handed out as slices of it: the bitstream
    /// is copied once, and a frame costs three allocations however many datagrams it makes (the
    /// buffer, its shared handle and the list of slices; `tests/allocs.rs` holds it there).
    pub fn packetize(
        &mut self,
        frame: &EncodedFrame<'_>,
        send_ms_lo: u8,
        mut ship: impl FnMut(&[Bytes]),
    ) -> Result<&SentFrame, MediaError> {
        let layout = layout(frame.data.len(), self.parity_permille, self.max_payload)?;
        let number = self.next_frame;
        self.next_frame = self.next_frame.wrapping_add(1);

        let data_count = usize::from(layout.data_count);
        let parity_count = usize::from(layout.parity_count);
        let shard_bytes = layout.shard_bytes;
        let stride = HEADER_BYTES.saturating_add(shard_bytes);
        let count = data_count.saturating_add(parity_count);
        let mut wire = BytesMut::with_capacity(count.saturating_mul(stride));
        let prefix = FramePrefix {
            len: U32::new(u32::try_from(frame.data.len()).unwrap_or(u32::MAX)),
            capture_ts_us: U32::new(frame.capture_ts_us),
            ltr_token: U64::new(frame.ltr_token.unwrap_or(0)),
            stripes: frame.stripes,
            build: self.build,
            reserved: [0; 2],
        };
        let mut header = MediaHeader {
            stream: U32::new(self.stream.0),
            frame: U32::new(number),
            index: U16::new(0),
            data_count: U16::new(layout.data_count),
            parity_count: layout.parity_count,
            kind: Kind::VideoData as u8,
            flags: frame_flags(frame, self.previous_discardable),
            send_ms_lo,
        };
        self.previous_discardable = header.flags & flags::DISCARDABLE != 0;
        // The frame body is the prefix, the bitstream, then zeros to the last shard's end; the
        // prefix is shorter than the smallest shard, so it only ever opens the first one.
        let mut rest = frame.data;
        for index in 0..data_count {
            header.index = U16::new(u16::try_from(index).unwrap_or(u16::MAX));
            wire.put_slice(header.as_bytes());
            let mut room = shard_bytes;
            if index == 0 {
                wire.put_slice(prefix.as_bytes());
                room = room.saturating_sub(FRAME_PREFIX_BYTES);
            }
            let (now, later) = rest.split_at(room.min(rest.len()));
            wire.put_slice(now);
            wire.put_bytes(0, room.saturating_sub(now.len()));
            rest = later;
        }

        let data = wire.split().freeze();
        // Sized for the parity too, so adding it does not grow the list.
        let mut datagrams = Vec::with_capacity(count);
        datagrams.extend(slices(&data, data_count, stride));
        ship(&datagrams);

        if parity_count > 0 {
            let encoder = match self.encoder.as_mut() {
                Some(encoder) => {
                    encoder.reset(data_count, parity_count, shard_bytes)?;
                    encoder
                }
                None => self.encoder.insert(ReedSolomonEncoder::new(
                    data_count,
                    parity_count,
                    shard_bytes,
                )?),
            };
            for datagram in data.chunks_exact(stride) {
                encoder.add_original_shard(datagram.get(HEADER_BYTES..).unwrap_or_default())?;
            }
            let parity = encoder.encode()?;
            header.kind = Kind::VideoParity as u8;
            for (offset, shard) in parity.recovery_iter().enumerate() {
                let index = data_count.saturating_add(offset);
                header.index = U16::new(u16::try_from(index).unwrap_or(u16::MAX));
                wire.put_slice(header.as_bytes());
                wire.put_slice(shard);
            }
            datagrams.extend(slices(&wire.freeze(), parity_count, stride));
            ship(datagrams.get(data_count..).unwrap_or_default());
        }

        self.datagrams_sent = self.datagrams_sent.saturating_add(datagrams.len() as u64);
        if self.history.len() >= HISTORY_FRAMES {
            self.history.pop_front();
        }
        self.history.push_back(SentFrame {
            frame: number,
            layout,
            datagrams,
        });
        self.history.back().ok_or(MediaError::Empty)
    }

    /// Answer a NACK: the requested data fragments of `frame` with the retransmit flag set.
    /// Empty `fragments` means every data fragment. Frames outside the history yield nothing.
    pub fn retransmit(&mut self, frame: u32, fragments: &[u16]) -> Vec<Bytes> {
        let Some(sent) = self.history.iter().find(|s| s.frame == frame) else {
            return Vec::new();
        };
        let data_count = usize::from(sent.layout.data_count);
        let out: Vec<Bytes> = if fragments.is_empty() {
            sent.datagrams
                .iter()
                .take(data_count)
                .map(retransmission)
                .collect()
        } else {
            fragments
                .iter()
                .map(|&i| usize::from(i))
                .filter(|&i| i < data_count)
                .filter_map(|i| sent.datagrams.get(i))
                .map(retransmission)
                .collect()
        };
        self.datagrams_sent = self.datagrams_sent.saturating_add(out.len() as u64);
        out
    }
}

/// The first `count` datagrams of `stride` bytes each in `wire`, as slices of it.
fn slices(wire: &Bytes, count: usize, stride: usize) -> impl Iterator<Item = Bytes> + '_ {
    (0..count).map(move |i| {
        let start = i.saturating_mul(stride);
        wire.slice(start..start.saturating_add(stride))
    })
}

/// A frame's header flags; `previous_discardable` says the frame cut before it was
/// [`flags::DISCARDABLE`].
const fn frame_flags(frame: &EncodedFrame<'_>, previous_discardable: bool) -> u8 {
    let mut f = 0;
    // A keyframe, a refresh and a long-term reference are what later frames are predicted
    // from, whatever the encoder's attachment said.
    if frame.discardable && !frame.keyframe && !frame.ltr_refresh && frame.ltr_token.is_none() {
        f |= flags::DISCARDABLE;
    }
    if previous_discardable {
        f |= flags::PREV_DISCARDABLE;
    }
    if frame.keyframe {
        f |= flags::KEYFRAME;
    }
    if frame.ltr_token.is_some() {
        f |= flags::LTR;
    }
    if frame.ltr_refresh {
        f |= flags::LTR_REFRESH;
    }
    f
}

fn retransmission(original: &Bytes) -> Bytes {
    let mut buf = BytesMut::from(original.as_ref());
    if let Ok((header, _payload)) = MediaHeader::mut_from_prefix(buf.as_mut()) {
        header.flags |= flags::RETRANSMIT;
    }
    buf.freeze()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::MAX_DATAGRAM;

    #[test]
    fn layout_balances_fragments() {
        // The most a frame of one fragment carries.
        let one = MAX_PAYLOAD - FRAME_PREFIX_BYTES;
        for len in [1, 100, one - 1, one, one + 1, 2333, 30_000, 1_000_000] {
            let l = layout(len, 200, MAX_PAYLOAD).unwrap();
            let total = len + FRAME_PREFIX_BYTES;
            assert!(
                l.shard_bytes <= MAX_PAYLOAD && l.shard_bytes.is_multiple_of(2),
                "{len}: {l:?}"
            );
            assert!(
                usize::from(l.data_count) * l.shard_bytes >= total,
                "{len}: {l:?}"
            );
            // Balanced: one fragment fewer would not fit.
            assert!(
                (usize::from(l.data_count) - 1) * MAX_PAYLOAD < total,
                "{len}: {l:?}"
            );
            assert!(usize::from(l.parity_count) >= MIN_PARITY_FRAGMENTS);
            assert!(ReedSolomonEncoder::supports(
                usize::from(l.data_count),
                usize::from(l.parity_count)
            ));
        }
        assert_eq!(
            layout(one, 200, MAX_PAYLOAD).unwrap(),
            Layout {
                data_count: 1,
                shard_bytes: MAX_PAYLOAD,
                parity_count: 2
            }
        );
        assert_eq!(layout(one + 1, 0, MAX_PAYLOAD).unwrap().parity_count, 0);
        assert_eq!(layout(one + 1, 200, MAX_PAYLOAD).unwrap().data_count, 2);
        assert!(matches!(
            layout(0, 200, MAX_PAYLOAD),
            Err(MediaError::Empty)
        ));
        assert!(matches!(
            layout(MAX_DATA_FRAGMENTS * MAX_PAYLOAD, 200, MAX_PAYLOAD),
            Err(MediaError::FrameTooLarge { .. })
        ));
    }

    /// Two parity fragments on any frame once the ratio says the link loses, one on a clean link,
    /// none when parity is off; a big frame gets its ratio's share either way.
    #[test]
    fn the_parity_floor_is_two_on_a_lossy_link_and_one_on_a_clean_one() {
        let parity =
            |len: usize, permille: u16| layout(len, permille, MAX_PAYLOAD).unwrap().parity_count;
        assert_eq!(parity(100, Redundancy::MIN), 1);
        assert_eq!(parity(5_000, Redundancy::MIN), 1);
        assert_eq!(parity(100, Redundancy::MIN + 1), 2);
        assert_eq!(parity(5_000, 200), 2);
        assert_eq!(parity(100, 0), 0);
        assert_eq!(parity(60_000, 200), 11);
        assert_eq!(parity(60_000, Redundancy::MIN), 3);
    }

    #[test]
    fn layout_honours_a_smaller_path_budget() {
        // QUIC's fallback 1200-byte MTU leaves about 1149 bytes per datagram.
        let l = layout(5000, 0, 1149 - HEADER_BYTES).unwrap();
        assert!(
            l.shard_bytes <= 1132 && l.shard_bytes.is_multiple_of(2),
            "{l:?}"
        );
        assert!(usize::from(l.data_count) * l.shard_bytes >= 5000 + FRAME_PREFIX_BYTES);
        // Odd budgets round down; tiny budgets are floored.
        assert_eq!(
            layout(100, 0, 1001).unwrap().shard_bytes,
            100 + FRAME_PREFIX_BYTES
        );
        assert!(layout(100_000, 0, 10).unwrap().shard_bytes <= MIN_PAYLOAD);
        // The packetizer cuts to the budget it was given.
        let shard = |budget: usize| {
            let mut p = Packetizer::new(StreamId(1));
            p.set_parity_permille(0);
            p.set_max_datagram(budget);
            let data = vec![3_u8; 5000];
            let frame = EncodedFrame {
                data: &data,
                keyframe: false,
                ltr_token: None,
                ltr_refresh: false,
                discardable: false,
                capture_ts_us: 0,
                stripes: 0,
            };
            p.packetize(&frame, 0, |_| {}).unwrap().layout.shard_bytes
        };
        assert!(shard(1149) <= 1132);
        assert_eq!(
            shard(5000),
            5000_usize
                .saturating_add(FRAME_PREFIX_BYTES)
                .div_ceil(5)
                .next_multiple_of(2)
        );
    }

    #[test]
    fn every_supported_extreme_is_accepted_by_the_engine() {
        for data in [1, 2, 3, 255, 256, 257, MAX_DATA_FRAGMENTS] {
            for parity in [1, 2, 255] {
                assert!(
                    ReedSolomonEncoder::supports(data, parity),
                    "{data}/{parity}"
                );
            }
        }
    }

    #[test]
    fn packetize_emits_data_then_parity_under_the_mtu() {
        let mut p = Packetizer::new(StreamId(3));
        let data: Vec<u8> = (0..5000_u32).map(|i| (i % 251) as u8).collect();
        let frame = EncodedFrame {
            data: &data,
            keyframe: true,
            ltr_token: Some(42),
            ltr_refresh: false,
            discardable: false,
            capture_ts_us: 99,
            stripes: 0,
        };
        let sent = p.packetize(&frame, 7, |_| {}).unwrap().clone();
        assert_eq!(sent.frame, 0);
        assert_eq!(sent.layout.data_count, 5);
        assert_eq!(sent.layout.parity_count, 2);
        assert_eq!(sent.datagrams.len(), 7);
        for (i, dg) in sent.datagrams.iter().enumerate() {
            assert!(dg.len() <= MAX_DATAGRAM);
            let (h, payload) = MediaHeader::parse(dg).unwrap();
            assert_eq!(payload.len(), sent.layout.shard_bytes);
            assert_eq!(h.stream.get(), 3);
            assert_eq!(usize::from(h.index.get()), i);
            assert_eq!(h.flags, flags::KEYFRAME | flags::LTR);
            assert_eq!(h.send_ms_lo, 7);
            assert_eq!(h.is_parity(), i >= 5);
        }
        let (_h, first) = MediaHeader::parse(&sent.datagrams[0]).unwrap();
        let (prefix, rest) = FramePrefix::parse(first).unwrap();
        assert_eq!(prefix.len.get(), 5000);
        assert_eq!(prefix.ltr_token.get(), 42);
        assert_eq!(&rest[..8], &data[..8]);
        assert_eq!(p.next_frame(), 1);
        assert_eq!(p.datagrams_sent(), 7);
    }

    #[test]
    fn retransmit_answers_from_history_with_the_flag_set() {
        let mut p = Packetizer::new(StreamId(1));
        let data = vec![1_u8; 3000];
        let frame = EncodedFrame {
            data: &data,
            keyframe: false,
            ltr_token: None,
            ltr_refresh: false,
            discardable: false,
            capture_ts_us: 0,
            stripes: 0,
        };
        for _ in 0..HISTORY_FRAMES + 2 {
            p.packetize(&frame, 0, |_| {}).unwrap();
        }
        assert!(p.retransmit(0, &[0]).is_empty(), "evicted from history");
        let all = p.retransmit(u32::try_from(HISTORY_FRAMES).unwrap(), &[]);
        assert_eq!(all.len(), 3, "all data fragments, no parity");
        let some = p.retransmit(u32::try_from(HISTORY_FRAMES + 1).unwrap(), &[2, 0, 3, 9]);
        assert_eq!(some.len(), 2, "out-of-range and parity indexes are dropped");
        let (h, _) = MediaHeader::parse(&some[0]).unwrap();
        assert_eq!(h.index.get(), 2);
        assert_eq!(h.flags & flags::RETRANSMIT, flags::RETRANSMIT);
    }

    /// The fragment cap is inclusive; the ratio clamps at one and is read back as set; the
    /// payload cap is even and bounded on both sides; the counters and the wire size are exact.
    #[test]
    fn the_caps_and_counters_read_back_exactly() {
        let full = MAX_DATA_FRAGMENTS * MAX_PAYLOAD - FRAME_PREFIX_BYTES;
        assert_eq!(
            usize::from(layout(full, 0, MAX_PAYLOAD).unwrap().data_count),
            MAX_DATA_FRAGMENTS
        );
        assert!(matches!(
            layout(full + 1, 0, MAX_PAYLOAD),
            Err(MediaError::FrameTooLarge { .. })
        ));

        let mut p = Packetizer::new(StreamId(1));
        assert_eq!(p.next_frame(), 0);
        assert!(format!("{p:?}").contains("Packetizer"));
        p.set_parity_permille(1500);
        assert_eq!(p.parity_permille(), 1000, "clamped to one");
        p.set_parity_permille(1000);
        assert_eq!(p.parity_permille(), 1000);
        p.set_parity_permille(300);
        assert_eq!(p.parity_permille(), 300);

        let data = vec![7_u8; 3000];
        let frame = EncodedFrame {
            data: &data,
            keyframe: true,
            ltr_token: None,
            ltr_refresh: false,
            discardable: false,
            capture_ts_us: 0,
            stripes: 0,
        };
        let sent = p.packetize(&frame, 0, |_| {}).unwrap();
        let wire = sent.bytes();
        assert_eq!(wire, sent.datagrams.iter().map(Bytes::len).sum::<usize>());
        assert_eq!(
            wire,
            sent.datagrams.len() * (sent.layout.shard_bytes + HEADER_BYTES)
        );
        assert!(sent.layout.parity_count > 0);
        assert_eq!(p.next_frame(), 1);

        p.set_parity_permille(0);
        let sent = p.packetize(&frame, 0, |_| {}).unwrap();
        assert_eq!(sent.layout.parity_count, 0);
        assert_eq!(
            sent.datagrams.len(),
            usize::from(sent.layout.data_count),
            "no parity work"
        );
        assert_eq!(p.next_frame(), 2);

        // The budget reaches the cut through `layout`'s clamp: floored, and rounded down to even.
        let total = data.len() + FRAME_PREFIX_BYTES;
        p.set_max_datagram(MIN_PAYLOAD + HEADER_BYTES - 1);
        let floored = p.packetize(&frame, 0, |_| {}).unwrap().layout.data_count;
        assert_eq!(
            usize::from(floored),
            total.div_ceil(MIN_PAYLOAD),
            "never under the floor"
        );
        p.set_max_datagram(MIN_PAYLOAD + HEADER_BYTES + 3);
        let even = p.packetize(&frame, 0, |_| {}).unwrap().layout.data_count;
        assert_eq!(
            usize::from(even),
            total.div_ceil(MIN_PAYLOAD + 2),
            "rounded down to even"
        );
    }

    /// The datagrams are slices of one buffer, and every byte of them is what the old
    /// cut-and-copy wrote: header, prefix, bitstream, zero padding, parity that rebuilds a
    /// lost data shard.
    #[test]
    fn the_datagrams_share_one_buffer_and_rebuild_the_frame() {
        let mut p = Packetizer::new(StreamId(9));
        p.set_build(4);
        let data: Vec<u8> = (0..4000_u32)
            .map(|i| u8::try_from(i % 253).unwrap())
            .collect();
        let frame = EncodedFrame {
            data: &data,
            keyframe: false,
            ltr_token: Some(7),
            ltr_refresh: true,
            discardable: false,
            capture_ts_us: 5,
            stripes: 0b10,
        };
        let sent = p.packetize(&frame, 3, |_| {}).unwrap().clone();
        let stride = HEADER_BYTES + sent.layout.shard_bytes;
        for pair in sent.datagrams.windows(2) {
            let gap = pair[1].as_ptr() as usize - pair[0].as_ptr() as usize;
            assert_eq!(gap, stride, "adjacent slices of one allocation");
        }
        let mut body = Vec::new();
        for (i, d) in sent
            .datagrams
            .iter()
            .take(usize::from(sent.layout.data_count))
            .enumerate()
        {
            let (h, payload) = MediaHeader::parse(d).unwrap();
            assert_eq!(usize::from(h.index.get()), i);
            assert_eq!(h.flags, flags::LTR | flags::LTR_REFRESH);
            body.extend_from_slice(payload);
        }
        let (prefix, rest) = FramePrefix::parse(&body).unwrap();
        assert_eq!(
            (
                prefix.len.get(),
                prefix.capture_ts_us.get(),
                prefix.ltr_token.get()
            ),
            (4000, 5, 7)
        );
        assert_eq!(
            (prefix.stripes, prefix.build),
            (0b10, 4),
            "the stripes and the build"
        );
        assert_eq!(&rest[..4000], data.as_slice());
        assert!(rest[4000..].iter().all(|&b| b == 0), "zero padded");

        // Drop data shard 1 and rebuild it from the parity.
        let data_count = usize::from(sent.layout.data_count);
        let parity_count = usize::from(sent.layout.parity_count);
        let shard = |i: usize| MediaHeader::parse(&sent.datagrams[i]).unwrap().1.to_vec();
        let mut decoder = reed_solomon_simd::ReedSolomonDecoder::new(
            data_count,
            parity_count,
            sent.layout.shard_bytes,
        )
        .unwrap();
        for i in (0..data_count).filter(|&i| i != 1) {
            decoder.add_original_shard(i, shard(i)).unwrap();
        }
        decoder.add_recovery_shard(0, shard(data_count)).unwrap();
        let restored = decoder.decode().unwrap();
        let rebuilt = restored
            .restored_original_iter()
            .find(|&(i, _)| i == 1)
            .unwrap()
            .1
            .to_vec();
        assert_eq!(rebuilt, shard(1));
    }

    /// A long-term reference is what a refresh is predicted from, so a frame carrying one is
    /// never one a receiver may skip, whatever the encoder said of it.
    #[test]
    fn a_frame_carrying_a_reference_is_never_skippable() {
        let mut p = Packetizer::new(StreamId(1));
        let data = vec![7_u8; 900];
        let frame = |ltr_token| EncodedFrame {
            data: &data,
            keyframe: false,
            ltr_token,
            ltr_refresh: false,
            discardable: true,
            capture_ts_us: 1,
            stripes: 0,
        };
        let flags_of = |sent: &SentFrame| MediaHeader::parse(&sent.datagrams[0]).unwrap().0.flags;
        let with_token = flags_of(p.packetize(&frame(Some(9)), 0, |_| {}).unwrap());
        assert_eq!(with_token & flags::DISCARDABLE, 0, "a reference is kept");
        let after = flags_of(p.packetize(&frame(None), 0, |_| {}).unwrap());
        assert_eq!(
            after & flags::PREV_DISCARDABLE,
            0,
            "nor is it named skippable by the next"
        );
        assert_eq!(after & flags::DISCARDABLE, flags::DISCARDABLE);
    }
}
