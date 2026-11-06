//! Audio datagrams, and how many earlier packets each carries again.
//!
//! An Opus packet is 10 ms and about 120 bytes, and one datagram carries it. A lost one is
//! concealed on the client (by repeating pitch), which is a guess. Sending each
//! packet again in the datagrams after it recovers 97–98 % of the losses at 20–50‰ random loss,
//! and costs about 95 kbit/s a copy (MEASUREMENTS "Opus concealment: ropus against fade-replay,
//! and repetition"). So an audio datagram carries up to [`MAX_AUDIO_COPIES`] earlier packets,
//! and [`AudioCopies`] turns the audio loss the client reports into how many: none on a clean
//! link, one once packets go missing, two when they go missing in pairs or often.
//!
//! The layout: the header's `data_count` is one more than the copies. Each copy, nearest first
//! (`seq - 1`, then `seq - 2`), goes behind its 2-byte big-endian length, and the packet itself
//! takes the rest.

use bytes::{BufMut as _, Bytes, BytesMut};
use zerocopy::IntoBytes as _;
use zerocopy::little_endian::{U16, U32};

use crate::wire::{HEADER_BYTES, Kind, MAX_PAYLOAD, MediaHeader, SoundReport, StreamId};

/// Most earlier packets an audio datagram carries again.
pub const MAX_AUDIO_COPIES: usize = 2;

/// The length in front of a copy.
const COPY_LENGTH: usize = 2;

/// One Opus packet as a datagram, carrying `earlier` again.
///
/// `earlier` is the packets before it, nearest first. Copies past [`MAX_AUDIO_COPIES`], empty
/// ones and those that would not fit are left off, the farthest first; `None` when the packet
/// itself does not fit.
#[must_use]
pub fn audio_datagram(
    stream: StreamId,
    seq: u32,
    send_ms_lo: u8,
    opus: &[u8],
    earlier: &[&[u8]],
) -> Option<Bytes> {
    if opus.len() > MAX_PAYLOAD {
        return None;
    }
    let mut size = opus.len();
    let copies = earlier
        .iter()
        .take(MAX_AUDIO_COPIES)
        .take_while(|copy| {
            let fits = !copy.is_empty()
                && size.saturating_add(COPY_LENGTH).saturating_add(copy.len()) <= MAX_PAYLOAD;
            if fits {
                size = size.saturating_add(COPY_LENGTH).saturating_add(copy.len());
            }
            fits
        })
        .count();
    let header = MediaHeader {
        stream: U32::new(stream.0),
        frame: U32::new(seq),
        index: U16::new(0),
        data_count: U16::new(u16::try_from(copies).unwrap_or(0).saturating_add(1)),
        parity_count: 0,
        kind: Kind::Audio as u8,
        flags: 0,
        send_ms_lo,
    };
    let mut buf = BytesMut::with_capacity(HEADER_BYTES.saturating_add(size));
    buf.put_slice(header.as_bytes());
    for copy in earlier.iter().take(copies) {
        buf.put_u16(u16::try_from(copy.len()).unwrap_or(u16::MAX));
        buf.put_slice(copy);
    }
    buf.put_slice(opus);
    Some(buf.freeze())
}

/// An audio datagram's packets ([`parse_audio`]).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AudioPackets {
    /// The packet the datagram is numbered for.
    pub packet: Bytes,
    /// The packets before it carried again, nearest first: `earlier[0]` is `seq - 1`.
    pub earlier: [Option<Bytes>; MAX_AUDIO_COPIES],
}

/// Split an audio datagram's `payload` by its header's `data_count`; `None` for a count or a
/// length the payload does not bear out, or an empty packet.
#[must_use]
pub fn parse_audio(data_count: u16, payload: &Bytes) -> Option<AudioPackets> {
    let copies = usize::from(data_count.checked_sub(1)?);
    if copies > MAX_AUDIO_COPIES {
        return None;
    }
    let mut earlier: [Option<Bytes>; MAX_AUDIO_COPIES] = Default::default();
    let mut at = 0_usize;
    for slot in earlier.iter_mut().take(copies) {
        let length = payload.get(at..at.checked_add(COPY_LENGTH)?)?;
        let len = usize::from(u16::from_be_bytes(length.try_into().ok()?));
        let start = at.checked_add(COPY_LENGTH)?;
        let end = start.checked_add(len)?;
        if len == 0 || end > payload.len() {
            return None;
        }
        *slot = Some(payload.slice(start..end));
        at = end;
    }
    if at >= payload.len() {
        return None;
    }
    Some(AudioPackets {
        packet: payload.slice(at..),
        earlier,
    })
}

/// The audio packets a receiver heard since its last [`SoundReport`], and the ones missing from
/// their sequence.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SoundCount {
    window: SoundReport,
    /// The newest packet's sequence.
    last: Option<u32>,
}

impl SoundCount {
    /// Packet `seq` arrived: counted, with the ones its sequence skipped since the last. A late
    /// or repeated one counts neither way: its place was counted lost.
    pub fn arrived(&mut self, seq: u32) {
        if let Some(last) = self.last {
            let gap = seq.wrapping_sub(last);
            if gap == 0 || gap > u32::MAX / 2 {
                return;
            }
            let skipped = u16::try_from(gap.saturating_sub(1)).unwrap_or(u16::MAX);
            self.window.lost = self.window.lost.saturating_add(skipped);
        }
        self.last = Some(seq);
        self.window.received = self.window.received.saturating_add(1);
    }

    /// The window's report, and a new window.
    pub const fn take(&mut self) -> SoundReport {
        let report = self.window;
        self.window = SoundReport {
            received: 0,
            lost: 0,
        };
        report
    }
}

/// How many earlier packets each audio datagram carries, from the audio loss the client reports.
///
/// It rises on the report that shows the loss and falls a copy at a time after
/// [`Self::HOLD_US`] without it, as parity does ([`crate::Redundancy`]): loss comes in clumps
/// between clean seconds. A lone packet lost now and then stays with concealment; two within
/// about a second turn one copy on; two in one report (a burst) or many in a second, two copies.
///
/// Pure: the caller passes each report and the time on its own clock.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct AudioCopies {
    copies: u8,
    /// Packets lost lately, decaying by half every [`Self::HALF_LIFE_US`].
    recent: f64,
    /// When [`Self::recent`] was last brought up to date.
    at_us: u64,
    /// Before this the copies do not fall.
    hold_until_us: u64,
}

impl AudioCopies {
    /// The half-life of the recent loss count.
    pub const HALF_LIFE_US: u64 = 1_000_000;
    /// How long the copies stay after the loss that asked for them.
    pub const HOLD_US: u64 = 10_000_000;
    /// Recent losses that turn one copy on.
    const ONE: f64 = 1.5;
    /// Recent losses that turn two on: about five in a second, 5 %.
    const TWO: f64 = 4.0;

    /// The copies in force.
    #[must_use]
    pub const fn copies(&self) -> usize {
        self.copies as usize
    }

    /// Take a report the client sent at `now_us`; the copies in force after it.
    pub fn on_report(&mut self, report: SoundReport, now_us: u64) -> usize {
        let elapsed = now_us.saturating_sub(self.at_us);
        self.at_us = now_us;
        #[expect(
            clippy::cast_precision_loss,
            reason = "microseconds of a session, well in range"
        )]
        let halves = elapsed as f64 / Self::HALF_LIFE_US as f64;
        let lost = report.lost;
        self.recent = self.recent.mul_add(0.5_f64.powf(halves), f64::from(lost));
        let wanted: u8 = if lost >= 2 || self.recent >= Self::TWO {
            2
        } else {
            u8::from(self.recent >= Self::ONE)
        };
        if wanted >= self.copies && wanted > 0 {
            self.copies = wanted;
            self.hold_until_us = now_us.saturating_add(Self::HOLD_US);
        } else if wanted < self.copies && now_us >= self.hold_until_us {
            self.copies = self.copies.saturating_sub(1);
            self.hold_until_us = now_us.saturating_add(Self::HOLD_US);
        }
        self.copies()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets(datagram: &Bytes) -> Option<(u32, AudioPackets)> {
        let (header, _) = MediaHeader::parse(datagram)?;
        let payload = datagram.slice(HEADER_BYTES..);
        Some((
            header.frame.get(),
            parse_audio(header.data_count.get(), &payload)?,
        ))
    }

    /// A packet and its copies come back as they went; one with none is one packet.
    #[test]
    fn the_copies_come_back_nearest_first() {
        let dg = audio_datagram(StreamId(1), 9, 0, &[9; 120], &[&[8; 110], &[7; 3]]).unwrap();
        let (seq, got) = packets(&dg).unwrap();
        assert_eq!(seq, 9);
        assert_eq!(got.packet, Bytes::from(vec![9; 120]));
        assert_eq!(
            got.earlier,
            [
                Some(Bytes::from(vec![8; 110])),
                Some(Bytes::from(vec![7; 3]))
            ]
        );
        let lone = audio_datagram(StreamId(1), 9, 0, &[9; 120], &[]).unwrap();
        assert_eq!(lone.len(), HEADER_BYTES + 120, "no copies, no lengths");
        assert_eq!(packets(&lone).unwrap().1.earlier, [None, None]);
    }

    /// The layout, byte for byte: a wire format, so a change here is a wire change. The header
    /// counts the packet and one copy; the copy goes behind its big-endian length, the packet
    /// takes the rest.
    #[test]
    fn the_layout_is_fixed() {
        let dg = audio_datagram(StreamId(7), 0x0102_0304, 0xab, &[0xaa, 0xbb], &[&[0xcc]]).unwrap();
        let header = [
            7,
            0,
            0,
            0,
            4,
            3,
            2,
            1,
            0,
            0,
            2,
            0,
            0,
            Kind::Audio as u8,
            0,
            0xab,
        ];
        assert_eq!(dg.get(..HEADER_BYTES), Some(&header[..]));
        assert_eq!(dg.get(HEADER_BYTES..), Some(&[0, 1, 0xcc, 0xaa, 0xbb][..]));
    }

    /// Copies that do not fit, past the cap, or empty are left off, the farthest first; a packet
    /// that does not fit alone is refused.
    #[test]
    fn copies_that_do_not_fit_are_left_off() {
        let full = MAX_PAYLOAD - 120 - COPY_LENGTH;
        let dg = audio_datagram(StreamId(1), 9, 0, &[9; 120], &[&[8; 1], &[7; 1]]).unwrap();
        assert!(packets(&dg).unwrap().1.earlier.iter().all(Option::is_some));
        let dg = audio_datagram(StreamId(1), 9, 0, &[9; 120], &[&vec![8; full], &[7; 1]]).unwrap();
        assert_eq!(
            packets(&dg).unwrap().1.earlier[1],
            None,
            "the farther one did not fit"
        );
        let dg = audio_datagram(StreamId(1), 9, 0, &[9; 120], &[&[], &[7; 1]]).unwrap();
        assert_eq!(
            packets(&dg).unwrap().1.earlier,
            [None, None],
            "nothing past a gap"
        );
        let three: [&[u8]; 3] = [&[1], &[2], &[3]];
        let dg = audio_datagram(StreamId(1), 9, 0, &[9; 120], &three).unwrap();
        assert_eq!(
            MediaHeader::parse(&dg).unwrap().0.data_count.get(),
            3,
            "two at most"
        );
        assert!(audio_datagram(StreamId(1), 5, 0, &[9; MAX_PAYLOAD], &[&[1]]).is_some());
        assert!(audio_datagram(StreamId(1), 5, 0, &[9; MAX_PAYLOAD + 1], &[]).is_none());
    }

    /// A count or a length the payload does not bear out is refused, never read past.
    #[test]
    fn a_malformed_audio_payload_is_refused() {
        let payload = |bytes: &[u8]| Bytes::copy_from_slice(bytes);
        assert_eq!(parse_audio(0, &payload(&[1, 2])), None, "no packet counted");
        assert_eq!(
            parse_audio(4, &payload(&[0, 1, 5, 0, 1, 6, 0, 1, 7, 9])),
            None,
            "three copies"
        );
        assert_eq!(
            parse_audio(2, &payload(&[0, 9, 5])),
            None,
            "a copy past the end"
        );
        assert_eq!(parse_audio(2, &payload(&[0])), None, "a length cut short");
        assert_eq!(parse_audio(2, &payload(&[0, 0, 9])), None, "an empty copy");
        assert_eq!(
            parse_audio(2, &payload(&[0, 1, 5])),
            None,
            "no packet behind the copy"
        );
        assert_eq!(parse_audio(1, &payload(&[])), None, "an empty packet");
        let ok = parse_audio(2, &payload(&[0, 1, 5, 9])).unwrap();
        assert_eq!(
            (ok.packet, ok.earlier[0].clone()),
            (payload(&[9]), Some(payload(&[5])))
        );
    }

    fn lost(lost: u16) -> SoundReport {
        SoundReport { received: 5, lost }
    }

    /// Gaps in the sequence count as lost, once; a late or repeated packet counts neither way;
    /// a report starts the next window empty.
    #[test]
    fn the_count_reads_the_sequence() {
        let mut count = SoundCount::default();
        for seq in [7, 8, 11, 10, 11, 12] {
            count.arrived(seq);
        }
        assert_eq!(
            count.take(),
            SoundReport {
                received: 4,
                lost: 2
            }
        );
        assert_eq!(count.take(), SoundReport::default());
        let mut count = SoundCount::default();
        count.arrived(u32::MAX);
        count.arrived(1);
        assert_eq!(
            count.take(),
            SoundReport {
                received: 2,
                lost: 1
            },
            "across the wrap"
        );
    }

    const REPORT_US: u64 = 50_000;

    /// A lone loss stays with concealment; two within a second turn a copy on; a pair in one
    /// report turns two on at once; the copies fall one at a time after the hold.
    #[test]
    fn the_copies_follow_the_reported_loss() {
        let mut copies = AudioCopies::default();
        let mut now = 1_000_000;
        let mut report = |copies: &mut AudioCopies, n| {
            now += REPORT_US;
            copies.on_report(lost(n), now)
        };
        assert_eq!(report(&mut copies, 0), 0, "a clean link carries none");
        assert_eq!(report(&mut copies, 1), 0, "a lone loss is concealed");
        for _ in 0..40 {
            assert_eq!(report(&mut copies, 0), 0);
        }
        assert_eq!(report(&mut copies, 1), 0);
        assert_eq!(report(&mut copies, 0), 0);
        assert_eq!(report(&mut copies, 1), 1, "two in a tenth of a second");
        assert_eq!(report(&mut copies, 2), 2, "a burst");
        let held = AudioCopies::HOLD_US / REPORT_US;
        for _ in 0..held - 1 {
            assert_eq!(report(&mut copies, 0), 2, "held");
        }
        assert_eq!(report(&mut copies, 0), 1, "one copy fewer after the hold");
        for _ in 0..held - 1 {
            assert_eq!(report(&mut copies, 0), 1);
        }
        assert_eq!(report(&mut copies, 0), 0, "and none after another");
    }

    /// Steady loss holds the copies up: each report that still asks for them renews the hold.
    #[test]
    fn steady_loss_keeps_the_copies() {
        let mut copies = AudioCopies::default();
        let mut now = 0;
        for k in 0..2_000_u32 {
            now += REPORT_US;
            copies.on_report(lost(u16::from(k % 4 == 0)), now);
            if k > 10 {
                assert!(copies.copies() >= 1, "report {k}: 5 % loss lost its copies");
            }
        }
    }
}
