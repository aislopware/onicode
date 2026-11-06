//! One run of a job on the destination, from the manifest to the last rename.
//!
//! 1. Fetch the manifest and keep the progress of every item unchanged since the last run.
//! 2. Decide each requested path: free names land as they are; taken ones follow the job's policy,
//!    or wait for the owner's answer.
//! 3. In `.onicode-partial-<id>` inside the destination folder, make the folders and links.
//! 4. Fetch the files (see [`crate::plan`]), each block checked against its digest as it lands; a
//!    finished file takes its metadata, mtime and mode.
//! 5. When everything under a requested path is in, its folders take their metadata, deepest first,
//!    and it is renamed into place: a whole tree appears at once.
//!
//! A dropped stream is retried from the blocks already on disk; a damaged block is fetched
//! again; an item changed at the source ends the run, and the next run starts from a fresh
//! manifest.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use onicode_proto::files::{ConflictChoice, ConflictPolicy, TransferConflict, TransferState};
use tokio::task::JoinSet;

use crate::conflict::keep_both;
use crate::job::{Cell, Decision, block_bytes, lock, missing, staged};
use crate::manifest::{Entry, Kind};
use crate::plan::{Scheduler, Unit, Want};
use crate::sink::{Partial, Sink};
use crate::source::{Source, fetch_manifest};
use crate::store::Store;
use crate::wire::{self, CHUNK, MAX_PACKED, MAX_ZFRAME, Reader, Request};
use crate::{Error, Tuning, blocking};

/// Bytes gathered before a write.
const WRITE: usize = 1 << 20;
/// The first wait before a dropped stream is tried again; it doubles up to [`RETRY_MAX`].
pub const RETRY_MIN: Duration = Duration::from_millis(250);
/// The longest wait between tries.
pub const RETRY_MAX: Duration = Duration::from_secs(5);
/// A block that arrives damaged this many times fails the job: the disk or memory is bad.
const DAMAGED_MAX: u32 = 3;

/// What a run shares among its units.
struct Ctx {
    cell: Arc<Cell>,
    source: Arc<dyn Source>,
    sink: Arc<dyn Sink>,
    staging: PathBuf,
    dest: PathBuf,
    tuning: Tuning,
    manifest: Arc<Vec<Entry>>,
    /// Files not yet finished under each requested path.
    pending: Mutex<HashMap<String, usize>>,
}

/// Runs `cell`'s job once, reading `source` and writing `sink`.
///
/// # Errors
///
/// [`Error::Link`] when the source cannot be reached (try again later); [`Error::Unknown`] and
/// [`Error::Changed`] when the run must start over; anything else fails the job.
pub async fn run(
    cell: Arc<Cell>,
    source: Arc<dyn Source>,
    sink: Arc<dyn Sink>,
    store: Store,
) -> Result<(), Error> {
    lock(&cell.files).clear();
    let entries = fetch_manifest(source.as_ref()).await?;
    cell.job().adopt(entries);
    cell.recount();
    let job = cell.snapshot();
    {
        let (store, id, manifest) = (store, job.id.clone(), Arc::clone(&job.manifest));
        blocking(move || Ok(store.save_manifest(&id, &manifest)?)).await?;
    }
    decide(&cell, &sink).await?;
    let job = cell.snapshot();
    let ctx = Arc::new(Ctx {
        staging: job.staging(),
        dest: PathBuf::from(&job.to.dir),
        tuning: job.tuning,
        manifest: Arc::clone(&job.manifest),
        cell,
        source,
        sink,
        pending: Mutex::new(HashMap::new()),
    });
    prepare(&ctx).await?;
    fetch_all(&ctx).await?;
    let names: Vec<String> = ctx
        .cell
        .snapshot()
        .roots
        .into_iter()
        .map(|r| r.name)
        .collect();
    {
        let ctx = Arc::clone(&ctx);
        blocking(move || {
            for name in names {
                ctx.place_root(&name)?;
            }
            Ok(ctx.sink.remove(&ctx.staging)?)
        })
        .await?;
    }
    Ok(())
}

/// Decides every requested path not yet decided.
async fn decide(cell: &Arc<Cell>, sink: &Arc<dyn Sink>) -> Result<(), Error> {
    let job = cell.snapshot();
    let dest = PathBuf::from(&job.to.dir);
    let names: Vec<String> = job.roots.iter().map(|r| r.name.clone()).collect();
    for name in names {
        let (decided, policy, ours) = {
            let job = cell.snapshot();
            let ours: Vec<String> = job
                .roots
                .iter()
                .filter_map(|r| match &r.decision {
                    Some(Decision::Place { name, .. }) => Some(name.clone()),
                    _ => None,
                })
                .collect();
            (job.decision(&name).is_some(), job.policy, ours)
        };
        if decided {
            continue;
        }
        let there = {
            let (sink, path) = (Arc::clone(sink), dest.join(&name));
            blocking(move || Ok(sink.existing(&path))).await?
        };
        let taken_by_us = ours.contains(&name);
        let decision = if there.is_none() && !taken_by_us {
            Decision::Place {
                name: name.clone(),
                replace: false,
            }
        } else {
            let choice = match policy {
                ConflictPolicy::Ask => ask(cell, &dest, &name, there).await,
                ConflictPolicy::Replace => ConflictChoice::Replace,
                ConflictPolicy::KeepBoth => ConflictChoice::KeepBoth,
                ConflictPolicy::Skip => ConflictChoice::Skip,
            };
            match choice {
                ConflictChoice::Replace if !taken_by_us => Decision::Place {
                    name: name.clone(),
                    replace: true,
                },
                ConflictChoice::Replace | ConflictChoice::KeepBoth => {
                    let taken = |candidate: &str| {
                        ours.iter().any(|n| n == candidate)
                            || sink.existing(&dest.join(candidate)).is_some()
                    };
                    Decision::Place {
                        name: keep_both(&name, &taken),
                        replace: false,
                    }
                }
                ConflictChoice::Skip => Decision::Skip,
            }
        };
        let mut job = cell.job();
        if let Some(root) = job.roots.iter_mut().find(|r| r.name == name) {
            root.decision = Some(decision);
        }
    }
    Ok(())
}

/// Puts the job in conflict over `name` and waits for the owner.
async fn ask(
    cell: &Arc<Cell>,
    dest: &Path,
    name: &str,
    there: Option<crate::sink::Existing>,
) -> ConflictChoice {
    {
        let mut job = cell.job();
        let (incoming_size, incoming_modified_ms) = {
            let mut size = 0_u64;
            let mut modified = 0_i64;
            for entry in job.manifest.iter().filter(|e| e.root() == name) {
                size = size.saturating_add(entry.size);
                if entry.rel == name {
                    modified = entry.mtime_ms;
                }
            }
            (size, modified)
        };
        let there = there.unwrap_or(crate::sink::Existing {
            size: 0,
            modified_ms: 0,
            dir: false,
        });
        job.state = TransferState::Conflict;
        job.conflict = Some(TransferConflict {
            path: dest.join(name).to_string_lossy().into_owned(),
            existing_size: there.size,
            existing_modified_ms: there.modified_ms,
            incoming_size,
            incoming_modified_ms,
        });
    }
    let (choice, all) = cell.answer().await;
    let mut job = cell.job();
    job.state = TransferState::Running;
    job.conflict = None;
    if all {
        job.policy = choice.into();
    }
    choice
}

/// Makes the staging folder, the folders and the links; counts the files left under each
/// requested path; places what is already whole.
async fn prepare(ctx: &Arc<Ctx>) -> Result<(), Error> {
    let pending = {
        let ctx = Arc::clone(ctx);
        blocking(move || {
            ctx.sink.make_dir(&ctx.staging)?;
            let job = ctx.cell.snapshot();
            let mut pending: HashMap<String, usize> = HashMap::new();
            for entry in ctx.manifest.iter().filter(|e| job.active(e)) {
                let index = u64::from(entry.index);
                if job.done.contains(index) {
                    continue;
                }
                let path = staged(&ctx.staging, entry);
                match entry.kind {
                    Kind::Dir => {
                        ctx.sink.make_dir(&path)?;
                        ctx.cell.job().done.insert(index);
                    }
                    Kind::Symlink => {
                        ctx.sink
                            .symlink(entry.target.as_deref().unwrap_or_default(), &path)?;
                        ctx.sink.finish(&path, entry)?;
                        ctx.cell.job().done.insert(index);
                    }
                    Kind::File => {
                        *pending.entry(entry.root().to_owned()).or_insert(0) += 1;
                    }
                }
            }
            Ok(pending)
        })
        .await?
    };
    let whole: Vec<String> = ctx
        .cell
        .snapshot()
        .roots
        .iter()
        .filter(|r| !r.placed && !pending.contains_key(&r.name))
        .map(|r| r.name.clone())
        .collect();
    *lock(&ctx.pending) = pending;
    let ctx = Arc::clone(ctx);
    blocking(move || {
        for name in whole {
            ctx.place_root(&name)?;
        }
        Ok(())
    })
    .await
}

/// Fetches every file left, [`Tuning::files`] units at once.
async fn fetch_all(ctx: &Arc<Ctx>) -> Result<(), Error> {
    let (wants, cheap) = {
        let job = ctx.cell.snapshot();
        let block = ctx.tuning.block;
        let wants: Vec<Want> = ctx
            .manifest
            .iter()
            .filter(|e| e.kind == Kind::File && job.active(e))
            .filter(|e| !job.done.contains(u64::from(e.index)))
            .map(|e| Want {
                item: e.index,
                size: e.size,
                missing: missing(e, job.partial.get(&e.index), block),
            })
            .collect();
        let cheap: Vec<bool> = ctx.manifest.iter().map(|e| e.kind != Kind::File).collect();
        (wants, cheap)
    };
    let is_cheap = |i: u32| {
        usize::try_from(i)
            .ok()
            .and_then(|i| cheap.get(i))
            .copied()
            .unwrap_or(false)
    };
    let mut scheduler = Scheduler::new(&wants, &is_cheap, ctx.tuning);
    let mut units: JoinSet<(Unit, Result<(), Error>)> = JoinSet::new();
    let mut damaged: HashMap<u32, u32> = HashMap::new();
    loop {
        while let Some(unit) = scheduler.start_next() {
            let ctx = Arc::clone(ctx);
            units.spawn(async move { (unit, fetch_unit(&ctx, unit).await) });
        }
        let Some(joined) = units.join_next().await else {
            return Ok(());
        };
        let (unit, result) = joined.map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        scheduler.finished(&unit);
        match result {
            Ok(()) => {
                let mut job = ctx.cell.job();
                if job.state == TransferState::Running {
                    job.error = None;
                }
            }
            Err(Error::Digest(item)) => {
                let times = damaged.entry(item).or_insert(0);
                *times = times.saturating_add(1);
                if *times >= DAMAGED_MAX {
                    return Err(Error::Refused(format!(
                        "item {item} arrived damaged {times} times; the disk or memory may be failing"
                    )));
                }
                if let Some(rest) = ctx.remaining(unit) {
                    scheduler.requeue(rest);
                }
            }
            Err(error) => return Err(error),
        }
    }
}

/// Fetches `unit`, trying a dropped stream again from where it got to.
async fn fetch_unit(ctx: &Arc<Ctx>, unit: Unit) -> Result<(), Error> {
    let mut unit = unit;
    let mut wait = RETRY_MIN;
    loop {
        let before = ctx.cell.bytes_done.load(Ordering::Relaxed);
        let result = match unit {
            Unit::Range { item, offset, len } => ctx.fetch_range(item, offset, len).await,
            Unit::Pack { first, count } => ctx.fetch_pack(first, count).await,
        };
        match result {
            Err(Error::Link(message)) => {
                tracing::debug!(%message, ?unit, "transfer stream dropped; trying again");
                ctx.cell.job().error = Some(message);
                if ctx.cell.bytes_done.load(Ordering::Relaxed) > before {
                    wait = RETRY_MIN;
                }
                tokio::time::sleep(wait).await;
                wait = wait.saturating_mul(2).min(RETRY_MAX);
                match ctx.remaining(unit) {
                    Some(rest) => unit = rest,
                    None => return Ok(()),
                }
            }
            other => return other,
        }
    }
}

impl Ctx {
    fn entry(&self, item: u32) -> Result<&Entry, Error> {
        usize::try_from(item)
            .ok()
            .and_then(|i| self.manifest.get(i))
            .ok_or_else(|| Error::Protocol(format!("no item {item}")))
    }

    /// What of `unit` is still to fetch: a unit lands in order, so what is done is a prefix.
    fn remaining(&self, unit: Unit) -> Option<Unit> {
        let job = self.cell.snapshot();
        match unit {
            Unit::Range { item, offset, len } => {
                if job.done.contains(u64::from(item)) {
                    return None;
                }
                let block = self.tuning.block;
                let end = offset.saturating_add(len);
                let first = offset / block;
                let last = if len == 0 {
                    first + 1
                } else {
                    end.div_ceil(block)
                };
                let run = job
                    .partial
                    .get(&item)
                    .map_or(0, |spans| spans.run_from(first, last));
                if first.saturating_add(run) >= last {
                    return None;
                }
                let from = first.saturating_add(run).saturating_mul(block).min(end);
                Some(Unit::Range {
                    item,
                    offset: from,
                    len: end.saturating_sub(from),
                })
            }
            Unit::Pack { first, count } => {
                let end = first.saturating_add(count);
                let next = (first..end).find(|&i| {
                    self.entry(i)
                        .is_ok_and(|e| e.kind == Kind::File && !job.done.contains(u64::from(i)))
                })?;
                Some(Unit::Pack {
                    first: next,
                    count: end.saturating_sub(next),
                })
            }
        }
    }

    /// The open partial file of `item`.
    async fn file(&self, entry: &Entry) -> Result<Arc<dyn Partial>, Error> {
        if let Some(file) = lock(&self.cell.files).get(&entry.index) {
            return Ok(Arc::clone(file));
        }
        let (sink, path, size) = (
            Arc::clone(&self.sink),
            staged(&self.staging, entry),
            entry.size,
        );
        let file = blocking(move || Ok(sink.create(&path, size)?)).await?;
        Ok(Arc::clone(
            lock(&self.cell.files).entry(entry.index).or_insert(file),
        ))
    }

    async fn fetch_range(self: &Arc<Self>, item: u32, offset: u64, len: u64) -> Result<(), Error> {
        let entry = self.entry(item)?.clone();
        *lock(&self.cell.current) = Some(entry.rel.clone());
        let file = self.file(&entry).await?;
        let mut reader = Reader::new(
            self.source
                .open(Request::Range { item, offset, len })
                .await?,
        );
        let block = self.tuning.block;
        for (start, stop) in wire::blocks(offset, len, block) {
            let mut hasher = blake3::Hasher::new();
            let mut at = start;
            let mut gathered = BytesMut::new();
            let mut gathered_at = start;
            while at < stop {
                let want = usize::try_from(stop.saturating_sub(at)).unwrap_or(usize::MAX);
                let data: Bytes = if entry.zstd {
                    let n = usize::try_from(reader.u32().await?).unwrap_or(usize::MAX);
                    if n > MAX_ZFRAME {
                        return Err(Error::Protocol(format!("a zstd frame of {n} bytes")));
                    }
                    let packed = reader.exact(n).await?;
                    zstd::bulk::decompress(&packed, CHUNK.min(want))
                        .map(Bytes::from)
                        .map_err(|e| Error::Protocol(format!("zstd: {e}")))?
                } else {
                    reader.some(want.min(CHUNK)).await?
                };
                if data.is_empty() || data.len() > want {
                    return Err(Error::Protocol("a frame past its block".into()));
                }
                hasher.update(&data);
                at = at.saturating_add(data.len() as u64);
                gathered.extend_from_slice(&data);
                if gathered.len() >= WRITE {
                    write(&file, gathered.split().freeze(), gathered_at).await?;
                    gathered_at = at;
                }
            }
            if !gathered.is_empty() {
                write(&file, gathered.split().freeze(), gathered_at).await?;
            }
            let digest = reader.digest().await?;
            if digest != *hasher.finalize().as_bytes() {
                return Err(Error::Digest(item));
            }
            self.block_done(&entry, start / block).await?;
        }
        Ok(())
    }

    async fn fetch_pack(self: &Arc<Self>, first: u32, count: u32) -> Result<(), Error> {
        let mut reader = Reader::new(self.source.open(Request::Pack { first, count }).await?);
        for item in first..first.saturating_add(count) {
            let n = reader.u64().await?;
            if n > MAX_PACKED {
                return Err(Error::Protocol(format!("a packed item of {n} bytes")));
            }
            let body = reader
                .exact(usize::try_from(n).unwrap_or(usize::MAX))
                .await?;
            let digest = reader.digest().await?;
            if digest != *blake3::hash(&body).as_bytes() {
                return Err(Error::Digest(item));
            }
            let entry = self.entry(item)?.clone();
            let wanted = {
                let job = self.cell.snapshot();
                entry.kind == Kind::File
                    && job.active(&entry)
                    && !job.done.contains(u64::from(item))
            };
            if !wanted {
                continue;
            }
            if n != entry.size {
                return Err(Error::Changed);
            }
            *lock(&self.cell.current) = Some(entry.rel.clone());
            let (sink, path) = (Arc::clone(&self.sink), staged(&self.staging, &entry));
            blocking(move || {
                sink.create(&path, n)?.write_at(&body, 0)?;
                Ok(())
            })
            .await?;
            self.block_done(&entry, 0).await?;
        }
        Ok(())
    }

    /// Block `index` of `entry` is on disk and checked; finishes the file with its last block.
    async fn block_done(self: &Arc<Self>, entry: &Entry, index: u64) -> Result<(), Error> {
        let block = self.tuning.block;
        let whole = {
            let mut job = self.cell.job();
            let spans = job.partial.entry(entry.index).or_default();
            if spans.contains(index) {
                return Ok(());
            }
            spans.insert(index);
            self.cell
                .bytes_done
                .fetch_add(block_bytes(entry.size, block, index), Ordering::Relaxed);
            spans.count() >= wire::block_count(entry.size, block)
        };
        if !whole {
            return Ok(());
        }
        lock(&self.cell.files).remove(&entry.index);
        {
            let (sink, path, done) = (
                Arc::clone(&self.sink),
                staged(&self.staging, entry),
                entry.clone(),
            );
            blocking(move || Ok(sink.finish(&path, &done)?)).await?;
        }
        {
            let mut job = self.cell.job();
            job.partial.remove(&entry.index);
            job.done.insert(u64::from(entry.index));
        }
        let last = {
            let mut pending = lock(&self.pending);
            pending.get_mut(entry.root()).is_some_and(|n| {
                *n = n.saturating_sub(1);
                *n == 0
            })
        };
        if last {
            let (ctx, root) = (Arc::clone(self), entry.root().to_owned());
            blocking(move || ctx.place_root(&root)).await?;
        }
        Ok(())
    }

    /// Finishes the folders under the requested path `name`, deepest first, and renames it
    /// into place. Blocks.
    fn place_root(&self, name: &str) -> Result<(), Error> {
        let decision = {
            let job = self.cell.snapshot();
            let Some(root) = job.roots.iter().find(|r| r.name == name) else {
                return Ok(());
            };
            if root.placed {
                return Ok(());
            }
            root.decision.clone()
        };
        let Some(Decision::Place {
            name: landing,
            replace,
        }) = decision
        else {
            return Ok(());
        };
        let mut dirs: Vec<&Entry> = self
            .manifest
            .iter()
            .filter(|e| e.kind == Kind::Dir && e.root() == name)
            .collect();
        dirs.sort_by_key(|e| Reverse(e.rel.matches('/').count()));
        // The requested folder itself is finished where it lands: a move may touch its times,
        // and a read-only mode would refuse the move.
        let (inner, top): (Vec<&Entry>, Vec<&Entry>) =
            dirs.into_iter().partition(|e| e.rel != name);
        for dir in inner {
            self.sink.finish(&staged(&self.staging, dir), dir)?;
        }
        let from = self.staging.join(name);
        let to = self.dest.join(&landing);
        if self.sink.existing(&from).is_some() {
            self.sink.place(&from, &to, replace)?;
            for dir in top {
                self.sink.finish(&to, dir)?;
            }
        }
        let mut job = self.cell.job();
        if let Some(root) = job.roots.iter_mut().find(|r| r.name == name) {
            root.placed = true;
        }
        Ok(())
    }
}

async fn write(file: &Arc<dyn Partial>, data: Bytes, at: u64) -> Result<(), Error> {
    let file = Arc::clone(file);
    blocking(move || Ok(file.write_at(&data, at)?)).await
}
