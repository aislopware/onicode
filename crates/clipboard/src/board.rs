//! The pasteboard seam: what clipboard sync needs from a pasteboard.
//!
//! That is its `changeCount` (macOS has no change notification, so it is polled), the types of
//! each item it holds, one representation's bytes when asked, and a way to replace the contents
//! with bytes and promises. `MacBoard` is `NSPasteboard`; [`Held`] is a clipboard kept in
//! memory: a Linux host's, and every test's.

use std::sync::Arc;

/// One item to write: the representations here now, and the types promised, which the board
/// asks [`Provide`] for when something reads them.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Item {
    /// `(type, bytes)`, in the order the copying app ranked them.
    pub data: Vec<(String, Vec<u8>)>,
    /// Types whose bytes come from the provider.
    pub promised: Vec<String>,
}

impl Item {
    /// An item of bytes alone.
    #[must_use]
    pub const fn data(data: Vec<(String, Vec<u8>)>) -> Self {
        Self {
            data,
            promised: Vec::new(),
        }
    }
}

/// Answers a promise: the bytes of type `kind` of item `item`, or `None` when they cannot be had.
/// Called on whatever thread asks (AppKit's main thread for a `MacBoard`), and must answer before
/// it returns.
pub type Provide = Arc<dyn Fn(usize, &str) -> Option<Vec<u8>> + Send + Sync>;

/// A read of one representation, capped at a size.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Capped {
    /// Its bytes, no more than the cap.
    Data(Vec<u8>),
    /// Past the cap: its size, and nothing copied.
    TooBig(u64),
}

/// A pasteboard as clipboard sync sees it.
pub trait Board: Send + Sync {
    /// The count that moves on every change of owner.
    fn change_count(&self) -> isize;
    /// The types of each item, in pasteboard order.
    fn items(&self) -> Vec<Vec<String>>;
    /// The bytes of type `kind` of item `item`. A `public.file-url` comes back as a path URL: a
    /// file named by reference (`file:///.file/id=…`, as Finder copies) means nothing on another
    /// machine.
    fn data(&self, item: usize, kind: &str) -> Option<Vec<u8>>;
    /// The bytes of type `kind` of item `item` when they are no more than `max`, else only their
    /// size. A board that can tell the size before copying copies nothing past the cap.
    fn data_within(&self, item: usize, kind: &str, max: u64) -> Option<Capped> {
        let bytes = self.data(item, kind)?;
        let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        Some(if size > max {
            Capped::TooBig(size)
        } else {
            Capped::Data(bytes)
        })
    }
    /// Replace the contents with `items`, their promises kept by `provide`; no items clears it.
    /// The `changeCount` the write left, `None` when it failed.
    fn write(&self, items: &[Item], provide: Option<Provide>) -> Option<isize>;
    /// Clear the contents if the count is still `count`: what was written then is still there.
    /// The count the clear left; `None` when the contents moved on, or the clear failed.
    fn clear_if(&self, count: isize) -> Option<isize> {
        if self.change_count() == count {
            self.write(&[], None)
        } else {
            None
        }
    }
}

/// Run the main thread's run loop once for what is due, without waiting, when called on the main
/// thread; `false` elsewhere.
///
/// AppKit asks a promise on the main run loop, and the provider must answer before it returns.
/// That run loop also serves the app's windows (or the background engine's banners), so a
/// promise waiting on a peer runs it between looks rather than stalling it for the wait.
#[cfg(target_os = "macos")]
#[must_use]
pub fn serve_main_run_loop() -> bool {
    // SAFETY: libSystem rule: `pthread_main_np` takes nothing and only reads the calling
    // thread's identity.
    if unsafe { libc::pthread_main_np() } != 1 {
        return false;
    }
    // SAFETY: CoreFoundation rule: `kCFRunLoopDefaultMode` is a constant string valid for the
    // life of the process.
    let mode = unsafe { objc2_core_foundation::kCFRunLoopDefaultMode };
    let _ran = objc2_core_foundation::CFRunLoop::run_in_mode(mode, 0.0, true);
    true
}

/// Run the main thread's run loop once, on the main thread: no run loop to serve off macOS.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn serve_main_run_loop() -> bool {
    false
}

/// A clipboard kept in memory, for a machine with none it can sync, and for tests.
///
/// That is a Linux host, terminal-only and most often headless. The programs there reach it
/// through shims for `xclip`, `xsel`, `wl-copy` and `wl-paste`, so it holds what they copy and
/// what clipboard sync mirrors from elsewhere. A promised type is asked of [`Provide`] when
/// first read, outside the lock, and kept.
#[derive(Default)]
pub struct Held {
    contents: parking_lot::Mutex<Contents>,
}

/// What a [`Held`] board holds.
#[derive(Default)]
struct Contents {
    count: isize,
    items: Vec<Item>,
    provide: Option<Provide>,
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let contents = self.contents.lock();
        f.debug_struct("Held")
            .field("count", &contents.count)
            .field("items", &contents.items.len())
            .finish_non_exhaustive()
    }
}

impl Held {
    /// The types still promised and not yet asked for, per item.
    #[must_use]
    pub fn promised(&self) -> Vec<Vec<String>> {
        self.contents
            .lock()
            .items
            .iter()
            .map(|item| item.promised.clone())
            .collect()
    }
}

impl Board for Held {
    fn change_count(&self) -> isize {
        self.contents.lock().count
    }

    fn items(&self) -> Vec<Vec<String>> {
        let contents = self.contents.lock();
        contents
            .items
            .iter()
            .map(|item| {
                let mut types: Vec<String> = item.data.iter().map(|(t, _)| t.clone()).collect();
                for kind in &item.promised {
                    if !types.contains(kind) {
                        types.push(kind.clone());
                    }
                }
                types
            })
            .collect()
    }

    fn data(&self, item: usize, kind: &str) -> Option<Vec<u8>> {
        let (count, provide) = {
            let contents = self.contents.lock();
            let held = contents.items.get(item)?;
            if let Some((_, bytes)) = held.data.iter().find(|(t, _)| t == kind) {
                return Some(bytes.clone());
            }
            if !held.promised.iter().any(|t| t == kind) {
                return None;
            }
            (contents.count, contents.provide.clone()?)
        };
        // The provider may wait seconds on a peer: never under the lock.
        let bytes = provide(item, kind)?;
        let mut contents = self.contents.lock();
        if contents.count == count
            && let Some(held) = contents.items.get_mut(item)
        {
            held.promised.retain(|t| t != kind);
            held.data.push((kind.to_owned(), bytes.clone()));
        }
        drop(contents);
        Some(bytes)
    }

    fn write(&self, items: &[Item], provide: Option<Provide>) -> Option<isize> {
        let mut contents = self.contents.lock();
        contents.count = contents.count.wrapping_add(1);
        contents.items = items.to_vec();
        contents.provide = provide;
        Some(contents.count)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{Board as _, Held, Item, Provide};

    const TEXT: &str = "public.utf8-plain-text";
    const PNG: &str = "public.png";

    /// A copy holds its types in order and reads back; each write moves the count, and an
    /// empty write clears it.
    #[test]
    fn a_held_board_keeps_what_is_written() {
        let board = Held::default();
        let start = board.change_count();
        let item = Item::data(vec![(TEXT.to_owned(), b"hi".to_vec())]);
        let wrote = board.write(&[item], None).unwrap();
        assert_ne!(wrote, start);
        assert_eq!(board.change_count(), wrote);
        assert_eq!(board.items(), vec![vec![TEXT.to_owned()]]);
        assert_eq!(board.data(0, TEXT).as_deref(), Some(&b"hi"[..]));
        assert_eq!(board.data(0, PNG), None, "a type it does not hold");
        assert_eq!(board.data(1, TEXT), None, "an item it does not hold");
        assert!(
            board.clear_if(wrote).is_some_and(|n| n != wrote),
            "a clear is a change"
        );
        assert_eq!(board.items(), Vec::<Vec<String>>::new());
    }

    /// A promise is asked once, when first read, and kept; one answered after the board moved
    /// on is handed to its reader and not kept for the new contents.
    #[test]
    fn a_promise_is_asked_once_and_kept_while_the_contents_stand() {
        let board = Arc::new(Held::default());
        let asked = Arc::new(AtomicUsize::new(0));
        let provide: Provide = {
            let asked = Arc::clone(&asked);
            Arc::new(move |item, kind| {
                asked.fetch_add(1, Ordering::Relaxed);
                (item == 0 && kind == PNG).then(|| vec![0x89, b'P', b'N', b'G'])
            })
        };
        let item = Item {
            data: vec![(TEXT.to_owned(), b"caption".to_vec())],
            promised: vec![PNG.to_owned()],
        };
        board.write(&[item], Some(Arc::clone(&provide))).unwrap();
        assert_eq!(board.items(), vec![vec![TEXT.to_owned(), PNG.to_owned()]]);
        assert_eq!(board.promised(), vec![vec![PNG.to_owned()]]);
        assert_eq!(board.data(0, PNG).map(|b| b.len()), Some(4));
        assert_eq!(board.data(0, PNG).map(|b| b.len()), Some(4));
        assert_eq!(
            asked.load(Ordering::Relaxed),
            1,
            "kept after the first read"
        );
        assert_eq!(board.promised(), vec![Vec::<String>::new()]);

        // The provider runs outside the lock: here it writes the board itself, as a copy
        // landing while a peer's bytes are on their way would.
        let racing: Provide = {
            let board = Arc::downgrade(&board);
            Arc::new(move |_item, _kind| {
                let newer = Item::data(vec![(TEXT.to_owned(), b"newer".to_vec())]);
                board.upgrade()?.write(&[newer], None)?;
                Some(b"late".to_vec())
            })
        };
        let item = Item {
            data: Vec::new(),
            promised: vec![PNG.to_owned()],
        };
        board.write(&[item], Some(racing)).unwrap();
        assert_eq!(
            board.data(0, PNG).as_deref(),
            Some(&b"late"[..]),
            "its reader gets it"
        );
        assert_eq!(
            board.items(),
            vec![vec![TEXT.to_owned()]],
            "the newer copy stands alone"
        );
        assert_eq!(board.data(0, TEXT).as_deref(), Some(&b"newer"[..]));
    }
}
