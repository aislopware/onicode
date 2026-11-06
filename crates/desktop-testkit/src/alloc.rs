//! A counting global allocator for allocation budgets.
//!
//! A test binary installs it once:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOC: onicode_desktop_testkit::alloc::Counting = onicode_desktop_testkit::alloc::Counting;
//! ```
//!
//! and [`measure`] then reports what a closure allocated on the calling thread. The counts are
//! kept per thread, so the harness's own threads and a neighbouring test do not move them, and
//! they do not depend on the machine's load: a budget on them can gate every commit where a
//! wall-time budget could not.
//!
//! Only the calling thread is counted. Work the closure hands to another thread is that
//! thread's; a budget over a path that crosses threads measures each side on its own thread.
//!
//! [`live`] is what the calling thread holds now: the bytes it allocated less those it freed,
//! for what a structure keeps rather than what building it cost. A block freed on another
//! thread than the one that made it moves two threads' counts apart, so it is read over work
//! that stays on one thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static BLOCKS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
    static LIVE: Cell<i64> = const { Cell::new(0) };
}

/// Move this thread's live bytes by `by`.
fn hold(by: i64) {
    let _held = LIVE.try_with(|c| c.set(c.get().wrapping_add(by)));
}

/// `bytes` as a signed count, saturated: no allocation comes near it.
fn signed(bytes: usize) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

/// Count one allocation of `bytes` on this thread.
fn count(bytes: usize) {
    // `try_with`: a thread being torn down may still free and allocate after its locals are
    // gone. These keys have no destructor, so that never happens here, but an allocator must
    // not panic whatever the thread's state.
    let _counted = BLOCKS.try_with(|c| c.set(c.get().wrapping_add(1)));
    let _sized = BYTES.try_with(|c| c.set(c.get().wrapping_add(bytes as u64)));
}

/// [`System`], counting every allocation and reallocation of the thread that makes it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counting;

// SAFETY: every method forwards to `System` with the caller's arguments unchanged, so the
// `GlobalAlloc` contract `System` meets is met here; the counting touches only thread-local
// cells, which never allocate (const-initialised, no destructor).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        hold(signed(layout.size()));
        // SAFETY: the caller's `layout` is forwarded as `GlobalAlloc::alloc` requires.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        hold(signed(layout.size()));
        // SAFETY: as `alloc`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        hold(signed(layout.size()).wrapping_neg());
        // SAFETY: `ptr` came from this allocator, which is `System`, with this `layout`.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        hold(signed(new_size).wrapping_sub(signed(layout.size())));
        // SAFETY: `ptr` came from `System` with `layout`, and the caller guarantees `new_size`
        // is valid for it, as `GlobalAlloc::realloc` requires.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// What a closure allocated on its thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Allocs {
    /// Allocations and reallocations.
    pub blocks: u64,
    /// Bytes asked for by them (a reallocation counts its new size).
    pub bytes: u64,
}

impl std::fmt::Display for Allocs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} allocations, {} bytes", self.blocks, self.bytes)
    }
}

fn now() -> Allocs {
    Allocs {
        blocks: BLOCKS.try_with(Cell::get).unwrap_or(0),
        bytes: BYTES.try_with(Cell::get).unwrap_or(0),
    }
}

/// Run `f` and count what it allocated on this thread.
///
/// The counts are zero unless the binary installed [`Counting`]; [`installed`] tells.
pub fn measure<T>(f: impl FnOnce() -> T) -> (T, Allocs) {
    let before = now();
    let out = f();
    let after = now();
    let used = Allocs {
        blocks: after.blocks.wrapping_sub(before.blocks),
        bytes: after.bytes.wrapping_sub(before.bytes),
    };
    (out, used)
}

/// The bytes this thread holds now: allocated on it less freed on it, since it began. The
/// difference of two readings is what the work between them kept.
#[must_use]
pub fn live() -> i64 {
    LIVE.try_with(Cell::get).unwrap_or(0)
}

/// Whether [`Counting`] is this binary's global allocator: a budget read without it would
/// pass on zeros.
#[must_use]
pub fn installed() -> bool {
    let (boxed, used) = measure(|| std::hint::black_box(Box::new(0_u64)));
    drop(boxed);
    used.blocks == 1
}
