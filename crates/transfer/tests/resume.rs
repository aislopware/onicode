//! A copy killed at any byte resumes from what is on disk to the same bytes, hashes, modes and
//! mtimes as the source, however often it is killed, and even when the source changes between
//! runs. Each kill is the run's source dying after a drawn number of bytes across all its
//! streams; each resume starts from the job as it was saved, as after a restart.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bytes::Bytes;
use onicode_proto::files::{ConflictPolicy, TransferFrom, TransferTo};
use onicode_transfer::job::{Cell, Job};
use onicode_transfer::store::Store;
use onicode_transfer::wire::{Request, Wire};
use onicode_transfer::{Disk, Error, LocalSource, Served, Source, pull};
use proptest::prelude::*;

/// A source that dies once `budget` bytes have crossed, as an engine killed mid-copy.
struct Dying {
    inner: LocalSource,
    budget: Arc<AtomicU64>,
}

struct DyingWire {
    inner: Box<dyn Wire>,
    budget: Arc<AtomicU64>,
}

#[async_trait]
impl Wire for DyingWire {
    async fn chunk(&mut self) -> Result<Option<Bytes>, Error> {
        // Dead once the budget is spent, even where the stream would have ended next: a cut
        // chunk must never be followed by a clean end.
        if self.budget.load(Ordering::Relaxed) == 0 {
            return Err(Error::Stopped);
        }
        let Some(chunk) = self.inner.chunk().await? else {
            return Ok(None);
        };
        let len = chunk.len() as u64;
        let left = self
            .budget
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                Some(left.saturating_sub(len))
            })
            .unwrap();
        if left == 0 {
            return Err(Error::Stopped);
        }
        if left < len {
            return Ok(Some(chunk.slice(..usize::try_from(left).unwrap())));
        }
        Ok(Some(chunk))
    }
}

#[async_trait]
impl Source for Dying {
    async fn open(&self, request: Request) -> Result<Box<dyn Wire>, Error> {
        if self.budget.load(Ordering::Relaxed) == 0 {
            return Err(Error::Stopped);
        }
        Ok(Box::new(DyingWire {
            inner: self.inner.open(request).await?,
            budget: Arc::clone(&self.budget),
        }))
    }
}

const ID: &str = "job-1";

fn source(served: &Arc<Served>, tree: &Path, budget: Option<u64>) -> Arc<dyn Source> {
    let tuning = common::small_tuning();
    served
        .register(ID, vec![tree.to_path_buf()], tuning.block)
        .unwrap();
    let local = LocalSource::new(Arc::clone(served), ID.into(), "here".into());
    match budget {
        Some(budget) => Arc::new(Dying {
            inner: local,
            budget: Arc::new(AtomicU64::new(budget)),
        }),
        None => Arc::new(local),
    }
}

/// The job as a restarted engine would find it.
fn kept(store: &Store) -> Arc<Cell> {
    let mut jobs = store.load();
    assert_eq!(jobs.len(), 1);
    Arc::new(Cell::new(jobs.remove(0)))
}

async fn copy_with_kills(seed: u64, kills: &[f64], change: bool) {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    let dest = dir.path().join("dest");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    let tree = common::build_tree(&src, seed, 40, 600 << 10);
    let store = Store::at(dir.path().join("jobs"));
    let job = Job::new(
        ID.into(),
        TransferFrom {
            device_id: "here".into(),
            paths: vec![tree.to_string_lossy().into_owned()],
        },
        TransferTo {
            device_id: "here".into(),
            dir: dest.to_string_lossy().into_owned(),
        },
        ConflictPolicy::Ask,
        common::small_tuning(),
    );
    store.save(&job).unwrap();
    let served = Arc::new(Served::new());
    let total = 1_300_000_f64;
    for (round, &kill) in kills.iter().enumerate() {
        let cell = kept(&store);
        let budget = (kill * total) as u64;
        let result = pull::run(
            Arc::clone(&cell),
            source(&served, &tree, Some(budget)),
            Arc::new(Disk),
            store.clone(),
        )
        .await;
        if let Err(error) = &result {
            assert!(matches!(error, Error::Stopped), "round {round}: {error}");
        }
        store.save(&cell.snapshot()).unwrap();
        if change && round == 0 && result.is_err() {
            // The source moves on between runs (a copy already in place keeps what it got): a
            // small file and the big one change.
            std::fs::write(tree.join("small/f0003"), b"changed since").unwrap();
            let big = tree.join("zz-big.bin");
            let mut bytes = std::fs::read(&big).unwrap();
            bytes.truncate(500 << 10);
            bytes.extend_from_slice(&common::noise(seed ^ 0xc4, 77_777));
            std::fs::write(&big, bytes).unwrap();
        }
    }
    let cell = kept(&store);
    pull::run(
        Arc::clone(&cell),
        source(&served, &tree, None),
        Arc::new(Disk),
        store.clone(),
    )
    .await
    .unwrap_or_else(|e| panic!("seed {seed}: the last run failed: {e}"));
    common::assert_same_tree(&tree, &dest.join("tree"));
    let left: Vec<_> = std::fs::read_dir(&dest)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["tree"], "nothing partial is left beside it");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn a_copy_killed_anywhere_resumes_to_the_same_bytes(
        seed in any::<u64>(),
        kills in proptest::collection::vec(0.0_f64..1.1, 1..4),
        change in any::<bool>(),
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(copy_with_kills(seed, &kills, change));
    }
}
