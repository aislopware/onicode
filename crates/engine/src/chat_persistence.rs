//! Coalesced chat snapshots: a burst of changes saves once, and queue or lock waits never occupy a
//! Tokio worker. On the chat's host a saved snapshot is the host holding its own writes, so the
//! save retires the outbox rows it covers.
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use onicode_doc::SessionDoc;
use onicode_sync::DocsStore;
use tokio::sync::mpsc;

const SAVE_INTERVAL: Duration = Duration::from_secs(1);

/// Whether this engine hosts the chat, asked at each save.
pub(crate) type IsHost = Box<dyn Fn() -> bool + Send + Sync>;

pub(crate) struct ChatPersistence {
    doc: Weak<SessionDoc>,
    store: Arc<DocsStore>,
    chat_id: String,
    generation: AtomicU64,
    saved: AtomicU64,
    snapshot_bytes: AtomicUsize,
    urgent: AtomicBool,
    /// Set once an adopted snapshot replaced this doc on disk: this copy must never overwrite it.
    retired: AtomicBool,
    is_host: IsHost,
    /// Set by the outbox feed when a local write missed the outbox; a snapshot exported after
    /// that holds the write, so a landed save clears it.
    publication_failed: Arc<AtomicBool>,
    write: Mutex<()>,
    wake: Option<mpsc::Sender<()>>,
    #[cfg(test)]
    writes: AtomicUsize,
    #[cfg(test)]
    before_export: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ChatPersistence {
    pub(crate) fn new(
        doc: &Arc<SessionDoc>,
        store: Arc<DocsStore>,
        chat_id: String,
        is_host: IsHost,
        publication_failed: Arc<AtomicBool>,
    ) -> Arc<Self> {
        let runtime = tokio::runtime::Handle::try_current().ok();
        let (tx, rx) = mpsc::channel(1);
        let this = Arc::new(Self {
            doc: Arc::downgrade(doc),
            store,
            chat_id,
            generation: AtomicU64::new(0),
            saved: AtomicU64::new(0),
            snapshot_bytes: AtomicUsize::new(0),
            urgent: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            is_host,
            publication_failed,
            write: Mutex::new(()),
            wake: runtime.as_ref().map(|_| tx),
            #[cfg(test)]
            writes: AtomicUsize::new(0),
            #[cfg(test)]
            before_export: Mutex::new(None),
        });
        if let Some(runtime) = runtime {
            runtime.spawn(Self::run(Arc::downgrade(&this), rx));
        }
        this
    }

    pub(crate) fn is_clean(&self) -> bool {
        self.saved.load(Ordering::Acquire) == self.generation.load(Ordering::Acquire)
    }

    pub(crate) fn snapshot_bytes(&self) -> usize {
        self.snapshot_bytes.load(Ordering::Relaxed)
    }

    /// Stops every further save: the doc on disk was replaced and this copy is stale.
    pub(crate) fn retire(&self) {
        let _write = self.write.lock().unwrap_or_else(|e| e.into_inner());
        self.retired.store(true, Ordering::Release);
    }

    pub(crate) fn dirty(&self, immediate: bool) {
        self.generation.fetch_add(1, Ordering::Release);
        if immediate {
            self.urgent.store(true, Ordering::Release);
        }
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        } else {
            // Offline synchronous tooling/tests have no executor to schedule.
            self.flush_sync();
        }
    }

    async fn run(weak: Weak<Self>, mut rx: mpsc::Receiver<()>) {
        while rx.recv().await.is_some() {
            let deadline = tokio::time::Instant::now() + SAVE_INTERVAL;
            loop {
                let Some(this) = weak.upgrade() else { return };
                let urgent = this.urgent.swap(false, Ordering::AcqRel);
                drop(this);
                if urgent {
                    break;
                }
                tokio::select! {
                    () = tokio::time::sleep_until(deadline) => break,
                    message = rx.recv() => if message.is_none() { return; },
                }
            }
            let Some(this) = weak.upgrade() else { return };
            // All chat persisters sharing this SQLite connection queue
            // asynchronously, BEFORE occupying a blocking-pool thread.
            let permit = this.store.snapshot_writer.clone().lock_owned().await;
            let job = this.clone();
            if let Err(error) = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                job.flush_sync();
            })
            .await
            {
                tracing::error!(%error, "chat snapshot worker failed");
            }
            if this.doc.strong_count() == 0 {
                return;
            }
            if !this.is_clean() {
                // Includes failed writes and changes during export. Retry on
                // the next interval even if the document has gone quiet.
                if let Some(wake) = &this.wake {
                    let _ = wake.try_send(());
                }
            }
        }
    }

    pub(crate) fn flush_sync(&self) {
        let flush = || {
            let _write = self.write.lock().unwrap_or_else(|e| e.into_inner());
            let generation = self.generation.load(Ordering::Acquire);
            if generation == self.saved.load(Ordering::Acquire) {
                return;
            }
            if self.retired.load(Ordering::Acquire) {
                self.saved.store(generation, Ordering::Release);
                return;
            }
            let Some(doc) = self.doc.upgrade() else {
                return;
            };
            // Sampled before the export, so the snapshot holds at least this much.
            let held = doc.doc().oplog_vv();
            let failed_before = self.publication_failed.swap(false, Ordering::AcqRel);
            #[cfg(test)]
            if let Some(hook) = self
                .before_export
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                hook();
            }
            let result = doc
                .export_snapshot()
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    self.snapshot_bytes.store(bytes.len(), Ordering::Relaxed);
                    self.store
                        .save_snapshot(&self.chat_id, &bytes)
                        .map_err(|e| e.to_string())
                });
            match result {
                Ok(()) => {
                    #[cfg(test)]
                    self.writes.fetch_add(1, Ordering::Relaxed);
                    self.saved.store(generation, Ordering::Release);
                    if (self.is_host)()
                        && let Err(error) = self.store.retire_outbox(&self.chat_id, &held)
                    {
                        tracing::warn!(chat = %self.chat_id, %error, "outbox retire failed");
                    }
                }
                Err(error) => {
                    if failed_before {
                        self.publication_failed.store(true, Ordering::Release);
                    }
                    tracing::warn!(chat = %self.chat_id, %error, "chat snapshot failed; retrying");
                }
            }
        };
        // Synchronous shutdown/eviction and command APIs call this directly.
        // Async persisters already execute it on the blocking pool.
        if tokio::runtime::Handle::try_current()
            .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
        {
            tokio::task::block_in_place(flush);
        } else {
            flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use loro::LoroDoc;

    use super::*;

    fn fixture(
        is_host: bool,
    ) -> (
        tempfile::TempDir,
        Arc<SessionDoc>,
        Arc<DocsStore>,
        Arc<ChatPersistence>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let doc = Arc::new(SessionDoc::init("whale").unwrap());
        let persistence = ChatPersistence::new(
            &doc,
            store.clone(),
            "whale".into(),
            Box::new(move || is_host),
            Arc::new(AtomicBool::new(false)),
        );
        (dir, doc, store, persistence)
    }

    fn write(doc: &SessionDoc, n: i64) {
        doc.doc().get_map("test").insert("applied", n).unwrap();
        doc.doc().commit();
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_coalesces_and_flush_bypasses_the_debounce() {
        let (_dir, doc, store, persistence) = fixture(false);
        for n in 1..=1000 {
            write(&doc, n);
            persistence.dirty(false);
        }
        assert_eq!(
            persistence.writes.load(Ordering::Relaxed),
            0,
            "no per-change writes"
        );
        while persistence.writes.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(persistence.writes.load(Ordering::Relaxed), 1);
        let restored = LoroDoc::new();
        restored
            .import(&store.load_snapshot("whale").unwrap().unwrap())
            .unwrap();
        assert_eq!(
            restored.get_map("test").get_deep_value(),
            doc.doc().get_map("test").get_deep_value()
        );
        write(&doc, 1001);
        persistence.dirty(false);
        persistence.flush_sync(); // shutdown/eviction must not await the timer
        assert!(persistence.is_clean());
        assert_eq!(persistence.writes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn a_failed_write_stays_dirty_until_a_retry_lands() {
        let (dir, doc, _store, persistence) = fixture(false);
        let db = rusqlite::Connection::open(dir.path().join("docs.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_insert BEFORE INSERT ON snapshots BEGIN SELECT RAISE(FAIL, 'injected disk failure'); END;",
        )
        .unwrap();
        write(&doc, 7);
        persistence.dirty(true); // no runtime: flushes inline, and fails
        assert!(!persistence.is_clean(), "the write failed");
        db.execute_batch("DROP TRIGGER fail_insert;").unwrap();
        persistence.flush_sync();
        assert!(persistence.is_clean());
    }

    #[test]
    fn a_landed_save_clears_a_missed_outbox_write_and_a_failed_one_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(DocsStore::open(dir.path()).unwrap());
        let doc = Arc::new(SessionDoc::init("whale").unwrap());
        let failed = Arc::new(AtomicBool::new(true));
        let persistence = ChatPersistence::new(
            &doc,
            store,
            "whale".into(),
            Box::new(|| false),
            failed.clone(),
        );
        let db = rusqlite::Connection::open(dir.path().join("docs.sqlite3")).unwrap();
        db.execute_batch(
            "CREATE TRIGGER fail_insert BEFORE INSERT ON snapshots BEGIN SELECT RAISE(FAIL, 'injected disk failure'); END;",
        )
        .unwrap();
        write(&doc, 1);
        persistence.dirty(true);
        assert!(
            failed.load(Ordering::Acquire),
            "nothing durable holds the write yet"
        );
        db.execute_batch("DROP TRIGGER fail_insert;").unwrap();
        persistence.flush_sync();
        assert!(!failed.load(Ordering::Acquire));
    }

    #[test]
    fn the_hosts_save_retires_the_outbox_rows_it_holds() {
        let (_dir, doc, store, persistence) = fixture(true);
        write(&doc, 1);
        store
            .enqueue_outbox("whale", b"bytes", &doc.doc().oplog_vv())
            .unwrap();
        let mut ahead = doc.doc().oplog_vv();
        ahead.set_end(loro::ID::new(99, 1));
        store.enqueue_outbox("whale", b"later", &ahead).unwrap();
        persistence.dirty(true);
        let left = store.outbox("whale").unwrap();
        assert_eq!(left.len(), 1, "only what the snapshot holds retires");
        assert_eq!(left[0].bytes, b"later");
    }

    #[test]
    fn a_viewers_save_leaves_the_outbox_for_the_hosts_ack() {
        let (_dir, doc, store, persistence) = fixture(false);
        write(&doc, 1);
        store
            .enqueue_outbox("whale", b"bytes", &doc.doc().oplog_vv())
            .unwrap();
        persistence.dirty(true);
        assert_eq!(store.outbox("whale").unwrap().len(), 1);
    }

    #[test]
    fn a_retired_copy_never_overwrites_the_adopted_snapshot() {
        let (_dir, doc, store, persistence) = fixture(false);
        store.save_snapshot("whale", b"adopted").unwrap();
        persistence.retire();
        write(&doc, 1);
        persistence.dirty(true);
        assert!(persistence.is_clean());
        assert_eq!(store.load_snapshot("whale").unwrap().unwrap(), b"adopted");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_held_version_is_read_before_the_export() {
        let (_dir, doc, store, persistence) = fixture(true);
        write(&doc, 1);
        let first = doc.doc().oplog_vv();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *persistence.before_export.lock().unwrap() = Some(Box::new(move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        persistence.generation.fetch_add(1, Ordering::Release);
        let p = persistence.clone();
        let job = tokio::task::spawn_blocking(move || p.flush_sync());
        started_rx.recv().unwrap();
        write(&doc, 2);
        store
            .enqueue_outbox("whale", b"second", &doc.doc().oplog_vv())
            .unwrap();
        release_tx.send(()).unwrap();
        job.await.unwrap();
        assert!(!first.includes_vv(&doc.doc().oplog_vv()));
        assert_eq!(
            store.outbox("whale").unwrap().len(),
            1,
            "a write made during the export is not retired by it"
        );
    }
}
