//! The client's media receiver: fragments, parity, retransmissions and the peer's own datagrams,
//! in any order, lost, repeated or damaged, with time passing between them.
//!
//! The input is a [`Plan`]: a script of what the host sends and what the wire does to it. The
//! frames come from the real [`Packetizer`], so the reassembler sees real fragments and real
//! Reed–Solomon parity and the fuzzer spends its effort on the order and the losses rather than
//! on guessing a valid header. A NACK the reassembler raises is answered from the packetizer's
//! history, as the host answers it.
//!
//! When nothing in the run was damaged or made up, every frame delivered must be byte for byte
//! the one sent, with its flags, and frames must come out in increasing order.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use arbitrary::{Arbitrary, Unstructured};
use bytes::Bytes;
use onicode_desktop_media::wire::StreamId;
use onicode_desktop_media::{
    Action, Config, EncodedFrame, FrameOut, Packetizer, Reassembler, audio_datagram,
    cursor_datagram, heartbeat_datagram,
};

const STREAM: StreamId = StreamId(7);

/// Most datagrams in flight at once; the rest of a burst is lost, as a full queue loses it.
const WIRE_CAP: usize = 4096;

/// What the host sends and what the wire does, in order.
#[derive(Arbitrary, Debug)]
pub struct Plan {
    /// The packetizer's parity ratio, thousandths.
    parity_permille: u16,
    /// The largest datagram the path carries.
    max_datagram: u16,
    ops: Vec<Op>,
}

#[derive(Arbitrary, Debug)]
enum Op {
    /// The host encodes a frame of `len` bytes; its datagrams join the wire.
    Frame {
        len: u16,
        keyframe: bool,
        ltr_token: Option<u64>,
        ltr_refresh: bool,
        seed: u8,
    },
    /// The parity ratio changes for the frames after this.
    Parity(u16),
    /// The datagram at `pick` (modulo what is in flight) arrives, and stays in flight when
    /// `again`.
    Deliver {
        pick: u16,
        again: bool,
    },
    /// Every datagram in flight arrives, in order.
    Flush,
    /// The datagram at `pick` is lost.
    Lose {
        pick: u16,
    },
    /// A byte of the datagram at `pick` is flipped.
    Damage {
        pick: u16,
        at: u16,
        xor: u8,
    },
    /// A datagram the peer made up arrives.
    Raw(Vec<u8>),
    /// An audio packet, a cursor move or a heartbeat arrives.
    Audio {
        seq: u32,
        len: u16,
        /// Earlier packets carried again (modulo 4; the datagram takes two at most).
        copies: u8,
    },
    Cursor {
        seq: u32,
        x: i32,
        y: i32,
        visible: bool,
    },
    Heartbeat {
        seq: u32,
    },
    /// Time passes and the owner ticks, answering what the reassembler asks for.
    Tick {
        ms: u16,
        rtt_ms: u16,
    },
    /// The presenter decoded an LTR frame, or lost its decoder.
    AckLtr(u64),
    Refresh {
        keyframe: bool,
    },
    SourceLive(bool),
    /// A receiver report is taken, and put back when it could not be sent.
    Report {
        late_frames: u32,
        unsent: bool,
    },
    /// The presenter takes what is ready.
    Drain,
}

/// Run the plan the fuzzer's bytes describe.
pub fn run(data: &[u8]) {
    let input = Unstructured::new(data);
    let Ok(plan) = Plan::arbitrary_take_rest(input) else {
        return;
    };
    Run::new(&plan).play(&plan.ops);
}

struct Run {
    packetizer: Packetizer,
    reassembler: Reassembler,
    wire: Vec<Bytes>,
    now: Instant,
    /// Every frame sent: its bitstream and whether it was a keyframe.
    sent: BTreeMap<u32, (Vec<u8>, bool)>,
    /// Nothing damaged or made up has reached the reassembler.
    clean: bool,
    last_delivered: Option<u32>,
}

impl Run {
    fn new(plan: &Plan) -> Self {
        let now = Instant::now();
        let mut packetizer = Packetizer::new(STREAM);
        packetizer.set_parity_permille(plan.parity_permille);
        packetizer.set_max_datagram(usize::from(plan.max_datagram));
        Self {
            packetizer,
            reassembler: Reassembler::new(STREAM, Config::default(), now),
            wire: Vec::new(),
            now,
            sent: BTreeMap::new(),
            clean: true,
            last_delivered: None,
        }
    }

    fn play(mut self, ops: &[Op]) {
        for op in ops {
            self.step(op);
        }
        self.drain();
    }

    fn step(&mut self, op: &Op) {
        match *op {
            Op::Frame {
                len,
                keyframe,
                ltr_token,
                ltr_refresh,
                seed,
            } => {
                self.frame(len, keyframe, ltr_token, ltr_refresh, seed);
            }
            Op::Parity(permille) => self.packetizer.set_parity_permille(permille),
            Op::Deliver { pick, again } => {
                if let Some(at) = self.pick(pick) {
                    let datagram = if again {
                        self.wire.get(at).cloned()
                    } else {
                        Some(self.wire.swap_remove(at))
                    };
                    if let Some(datagram) = datagram {
                        self.arrive(&datagram);
                    }
                }
            }
            Op::Flush => {
                for datagram in std::mem::take(&mut self.wire) {
                    self.arrive(&datagram);
                }
            }
            Op::Lose { pick } => {
                if let Some(at) = self.pick(pick) {
                    self.wire.swap_remove(at);
                }
            }
            Op::Damage { pick, at, xor } => self.damage(pick, at, xor),
            Op::Raw(ref bytes) => {
                self.clean = false;
                self.arrive(&Bytes::copy_from_slice(bytes));
            }
            Op::Audio { seq, len, copies } => {
                let opus = vec![0x5a; usize::from(len)];
                let earlier = vec![&opus[..]; usize::from(copies % 4)];
                if let Some(datagram) = audio_datagram(STREAM, seq, 0, &opus, &earlier) {
                    self.arrive(&datagram);
                }
            }
            Op::Cursor { seq, x, y, visible } => {
                self.arrive(&cursor_datagram(STREAM, seq, 0, x, y, visible));
            }
            Op::Heartbeat { seq } => self.arrive(&heartbeat_datagram(STREAM, seq, 0)),
            Op::Tick { ms, rtt_ms } => self.tick(ms, rtt_ms),
            Op::AckLtr(token) => self.reassembler.ack_ltr(token),
            Op::Refresh { keyframe } => self.reassembler.force_refresh(self.now, keyframe),
            Op::SourceLive(live) => self.reassembler.set_source_live(live),
            Op::Report {
                late_frames,
                unsent,
            } => {
                let report = self.reassembler.take_report(self.now, late_frames);
                if unsent {
                    self.reassembler.take_back(&report);
                }
            }
            Op::Drain => self.drain(),
        }
    }

    fn frame(&mut self, len: u16, keyframe: bool, ltr: Option<u64>, refresh: bool, seed: u8) {
        let data: Vec<u8> = (0..len)
            .map(|i| seed.wrapping_add(i.to_le_bytes()[0]))
            .collect();
        let frame = EncodedFrame {
            data: &data,
            keyframe,
            ltr_token: ltr,
            ltr_refresh: refresh,
            capture_ts_us: 0,
            discardable: false,
            stripes: 0,
        };
        let number = self.packetizer.next_frame();
        let mut shipped = Vec::new();
        if self
            .packetizer
            .packetize(&frame, 0, |d| shipped.extend_from_slice(d))
            .is_ok()
        {
            self.sent.insert(number, (data, keyframe));
            self.send(shipped);
        }
    }

    fn send(&mut self, datagrams: Vec<Bytes>) {
        let room = WIRE_CAP.saturating_sub(self.wire.len());
        self.wire.extend(datagrams.into_iter().take(room));
    }

    fn pick(&self, pick: u16) -> Option<usize> {
        usize::from(pick).checked_rem(self.wire.len())
    }

    fn damage(&mut self, pick: u16, at: u16, xor: u8) {
        let Some(slot) = self.pick(pick).and_then(|i| self.wire.get_mut(i)) else {
            return;
        };
        let mut bytes = slot.to_vec();
        let Some(at) = usize::from(at).checked_rem(bytes.len()) else {
            return;
        };
        if let Some(byte) = bytes.get_mut(at) {
            *byte ^= xor;
        }
        if xor != 0 {
            self.clean = false;
        }
        *slot = Bytes::from(bytes);
    }

    fn arrive(&mut self, datagram: &Bytes) {
        let _ingest = self.reassembler.ingest(datagram, self.now);
    }

    fn tick(&mut self, ms: u16, rtt_ms: u16) {
        self.now = self
            .now
            .checked_add(Duration::from_millis(u64::from(ms)))
            .unwrap_or(self.now);
        let rtt = Duration::from_millis(u64::from(rtt_ms));
        let _stalled = self.reassembler.stalled(self.now);
        for action in self.reassembler.tick(self.now, rtt) {
            if let Action::Nack { frame, fragments } = action {
                let resent = self.packetizer.retransmit(frame, &fragments);
                self.send(resent);
            }
        }
    }

    fn drain(&mut self) {
        while let Some(out) = self.reassembler.next_frame() {
            self.delivered(&out);
        }
    }

    fn delivered(&mut self, out: &FrameOut) {
        if !self.clean {
            return;
        }
        let number = out.info.frame;
        if let Some(last) = self.last_delivered {
            assert!(number > last, "frame {number} delivered after frame {last}");
        }
        self.last_delivered = Some(number);
        let Some((data, keyframe)) = self.sent.get(&number) else {
            panic!("frame {number} delivered but never sent");
        };
        assert_eq!(
            out.info.keyframe, *keyframe,
            "frame {number}'s keyframe flag"
        );
        assert!(
            out.data == data.as_slice(),
            "frame {number} delivered with other bytes"
        );
    }
}
