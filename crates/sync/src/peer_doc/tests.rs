use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use loro::{ExportMode, ImportStatus, LoroDoc, VersionVector};
use proptest::prelude::*;
use tokio::sync::watch;

use super::{End, Frame, PeerDocError, Replica, open, serve};
use crate::frames::pipe::pipe;

/// A replica that is just a Loro doc with one text container.
struct Mem {
    doc: Mutex<LoroDoc>,
    changes: watch::Sender<u64>,
    acks: Mutex<Vec<(String, VersionVector)>>,
    adopted: AtomicUsize,
}

impl Mem {
    fn new(peer: u64) -> Arc<Self> {
        let doc = LoroDoc::new();
        doc.set_peer_id(peer).unwrap();
        Self::with(doc)
    }

    fn with(doc: LoroDoc) -> Arc<Self> {
        Arc::new(Self {
            doc: Mutex::new(doc),
            changes: watch::Sender::new(0),
            acks: Mutex::new(Vec::new()),
            adopted: AtomicUsize::new(0),
        })
    }

    fn current(&self) -> LoroDoc {
        self.doc.lock().unwrap().clone()
    }

    fn write(&self, s: &str) {
        let doc = self.current();
        let text = doc.get_text("t");
        text.insert(text.len_unicode(), s).unwrap();
        doc.commit();
        self.changes.send_modify(|n| *n += 1);
    }

    fn text(&self) -> String {
        self.current().get_text("t").to_string()
    }

    fn vv(&self) -> VersionVector {
        self.current().oplog_vv()
    }

    fn acked_by(&self, peer: &str) -> Option<VersionVector> {
        self.acks
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(p, _)| p == peer)
            .map(|(_, vv)| vv.clone())
    }
}

impl Replica for Mem {
    fn doc(&self) -> Option<LoroDoc> {
        Some(self.current())
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn import(&self, bytes: &[u8], _catch_up: bool) -> Result<ImportStatus, String> {
        let status = self.current().import(bytes).map_err(|e| e.to_string())?;
        self.changes.send_modify(|n| *n += 1);
        Ok(status)
    }

    fn adopt(&self, snapshot: &[u8]) -> Result<(), String> {
        let doc = LoroDoc::from_snapshot(snapshot).map_err(|e| e.to_string())?;
        *self.doc.lock().unwrap() = doc;
        self.adopted.fetch_add(1, Ordering::Relaxed);
        self.changes.send_modify(|n| *n += 1);
        Ok(())
    }

    fn acked(&self, peer: &str, vv: &VersionVector) {
        self.acks
            .lock()
            .unwrap()
            .push((peer.to_owned(), vv.clone()));
    }
}

/// Runs a one-shot session between two replicas over in-memory pipes.
async fn sync_once(a: &Arc<Mem>, b: &Arc<Mem>) -> (End, End) {
    let (a_tx, b_rx) = pipe();
    let (b_tx, a_rx) = pipe();
    let server = {
        let b = Arc::clone(b);
        tokio::spawn(async move { serve(&*b, "a", b_tx, b_rx).await })
    };
    let opened = open(&**a, "b", false, a_tx, a_rx).await.unwrap();
    let served = server.await.unwrap().unwrap();
    (opened, served)
}

#[test]
fn frames_round_trip_and_compress_large_bodies() {
    let doc = LoroDoc::new();
    doc.set_peer_id(7).unwrap();
    doc.get_text("t").insert(0, &"x".repeat(4096)).unwrap();
    doc.commit();
    let vv = doc.oplog_vv();
    let frames = [
        Frame::Open {
            live: true,
            vv: vv.clone(),
        },
        Frame::State { vv: vv.clone() },
        Frame::Ack { vv },
        Frame::Updates {
            more: true,
            snapshot: false,
            bytes: vec![b'a'; 10_000],
        },
        Frame::Updates {
            more: false,
            snapshot: true,
            bytes: Vec::new(),
        },
    ];
    for frame in frames {
        let wire = frame.encode();
        assert_eq!(Frame::decode(&wire).unwrap(), frame);
    }
    let big = Frame::Updates {
        more: false,
        snapshot: false,
        bytes: vec![b'a'; 10_000],
    }
    .encode();
    assert!(big.len() < 1_000, "a repetitive body is compressed");
    assert_ne!(big.first().unwrap() & 0x80, 0);
}

#[test]
fn malformed_frames_are_refused() {
    assert!(Frame::decode(&[]).is_err());
    assert!(Frame::decode(&[9]).is_err());
    assert!(Frame::decode(&[3]).is_err(), "updates without flags");
    // A compressed frame claiming to inflate to 1 GiB.
    let mut bomb = vec![3 | 0x80];
    bomb.extend_from_slice(&(1u32 << 30).to_le_bytes());
    bomb.extend_from_slice(&[0; 8]);
    assert!(matches!(
        Frame::decode(&bomb),
        Err(PeerDocError::Protocol(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn one_shot_session_converges_divergent_replicas_and_acks() {
    let a = Mem::new(1);
    let b = Mem::new(2);
    a.write("from a;");
    b.write("from b;");
    let (opened, served) = sync_once(&a, &b).await;
    assert_eq!((opened, served), (End::Done, End::Done));
    assert_eq!(a.vv(), b.vv());
    assert_eq!(a.text(), b.text());
    assert_eq!(a.acked_by("b"), Some(b.vv()));
    assert_eq!(b.acked_by("a"), Some(a.vv()));
}

#[tokio::test(start_paused = true)]
async fn live_session_pushes_changes_both_ways_and_acks_them() {
    let a = Mem::new(1);
    let b = Mem::new(2);
    a.write("hello");
    let (a_tx, b_rx) = pipe();
    let (b_tx, a_rx) = pipe();
    let server = {
        let b = Arc::clone(&b);
        tokio::spawn(async move { serve(&*b, "a", b_tx, b_rx).await })
    };
    let client = {
        let a = Arc::clone(&a);
        tokio::spawn(async move { open(&*a, "b", true, a_tx, a_rx).await })
    };
    wait_until(|| b.text() == "hello").await;
    b.write(" world");
    wait_until(|| a.text() == "hello world").await;
    a.write("!");
    wait_until(|| b.text() == "hello world!").await;
    // Each side acks what it imported within a tick.
    wait_until(|| a.acked_by("b") == Some(a.vv()) && b.acked_by("a") == Some(b.vv())).await;
    client.abort();
    let served = server.await.unwrap();
    assert!(matches!(served, Ok(End::Closed) | Err(_)));
}

#[tokio::test(start_paused = true)]
async fn a_replica_behind_the_shallow_root_adopts_a_snapshot() {
    let origin = Mem::new(1);
    origin.write("a");
    origin.write("b");
    let root = origin.current().oplog_frontiers();
    let shallow = origin
        .current()
        .export(ExportMode::shallow_snapshot(&root))
        .unwrap();
    let host = Mem::with(LoroDoc::from_snapshot(&shallow).unwrap());
    host.current().set_peer_id(3).unwrap();
    host.write("c");
    assert!(host.current().is_shallow());

    // Empty, so behind the root: gets the snapshot.
    let straggler = Mem::new(4);
    let (opened, _) = sync_once(&straggler, &host).await;
    assert_eq!(opened, End::Adopted);
    assert_eq!(straggler.adopted.load(Ordering::Relaxed), 1);
    assert_eq!(straggler.text(), "abc");

    // Holds everything up to the root: plain updates, no adoption.
    let (opened, served) = sync_once(&origin, &host).await;
    assert_eq!((opened, served), (End::Done, End::Done));
    assert_eq!(origin.adopted.load(Ordering::Relaxed), 0);
    assert_eq!(origin.text(), "abc");
}

#[tokio::test(start_paused = true)]
async fn a_batch_larger_than_one_chunk_arrives_whole() {
    let a = Mem::new(1);
    let b = Mem::new(2);
    // Incompressible enough to need several chunks after compression too.
    let noise: String = (0..600_000u32)
        .map(|i| char::from(b'a' + u8::try_from(i.wrapping_mul(2_654_435_761) >> 27).unwrap() % 26))
        .collect();
    a.write(&noise);
    sync_once(&a, &b).await;
    assert_eq!(b.text(), noise);
}

async fn wait_until(mut done: impl FnMut() -> bool) {
    for _ in 0..1_000 {
        if done() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition never held");
}

#[derive(Debug, Clone)]
enum Op {
    Write(usize, u8),
    Sync(usize, usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0..3usize, any::<u8>()).prop_map(|(r, c)| Op::Write(r, c)),
        (0..3usize, 0..3usize).prop_map(|(a, b)| Op::Sync(a, b)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any interleaving of writes and pairwise sessions, followed by a sync round, leaves every
    /// replica with the same ops and text (I2), and each ack names a vv the acker really held.
    #[test]
    fn pairwise_sessions_converge(ops in proptest::collection::vec(op(), 0..40)) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        rt.block_on(async {
            let replicas = [Mem::new(1), Mem::new(2), Mem::new(3)];
            for op in ops {
                match op {
                    Op::Write(r, c) => replicas[r].write(&char::from(b'a' + c % 26).to_string()),
                    Op::Sync(a, b) if a != b => {
                        sync_once(&replicas[a], &replicas[b]).await;
                        prop_assert_eq!(replicas[a].vv(), replicas[b].vv());
                    }
                    Op::Sync(..) => {}
                }
            }
            sync_once(&replicas[0], &replicas[1]).await;
            sync_once(&replicas[2], &replicas[0]).await;
            sync_once(&replicas[1], &replicas[2]).await;
            let vv = replicas[0].vv();
            for replica in &replicas {
                prop_assert_eq!(&replica.vv(), &vv);
                prop_assert_eq!(replica.text(), replicas[0].text());
                for (_, acked) in replica.acks.lock().unwrap().iter() {
                    prop_assert!(vv.includes_vv(acked));
                }
            }
            Ok(())
        })?;
    }
}
