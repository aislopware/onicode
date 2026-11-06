//! The host's pasteboard, for clipboard sync with clients.
//!
//! [`Board`] is the clipboard seam, compiled on every target: the little clipboard sync needs
//! from a pasteboard. That is its `changeCount` (macOS has no change notification, so it is
//! polled), the types of each item it holds, one representation's bytes when asked, and a way to
//! replace the contents with bytes and promises. [`MacBoard`] is `NSPasteboard`, either the
//! general one or a named one; tests use a named one ([`MacBoard::unique`]) and release it, so no
//! test ever touches the user's clipboard. [`Held`] is a clipboard the host keeps itself, on
//! Linux.

use std::sync::Arc;

#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2::runtime::ProtocolObject;
#[cfg(target_os = "macos")]
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardType,
    NSPasteboardTypeFileURL, NSPasteboardTypeHTML, NSPasteboardTypePNG, NSPasteboardTypeRTF,
    NSPasteboardTypeString, NSPasteboardTypeTIFF, NSPasteboardWriting,
};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSArray, NSData, NSString, NSURL};
pub use onicode_desktop_proto::transfer::{
    CONCEALED_TYPE, ClipFormat, ClipType, ORIGIN_TYPE, TRANSIENT_TYPE, carried,
};

// A type on the wire is a platform-neutral format ([`ClipFormat`]) or an Apple UTI; a board
// speaks its own platform's type names, and this is where the two meet. On macOS the names come
// from AppKit's statics and a UTI is itself; elsewhere a board's types are MIME types and a UTI
// has no place.
#[cfg(target_os = "macos")]
/// The type `format` has on this platform's pasteboard: the UTI AppKit names it by.
#[must_use]
pub fn board_type(format: ClipFormat) -> String {
    // SAFETY: the `NSPasteboardType*` statics are AppKit constants, valid for the process
    // lifetime.
    let kind: &NSPasteboardType = unsafe {
        match format {
            ClipFormat::FileUrls => NSPasteboardTypeFileURL,
            ClipFormat::Png => NSPasteboardTypePNG,
            ClipFormat::Tiff => NSPasteboardTypeTIFF,
            ClipFormat::Rtf => NSPasteboardTypeRTF,
            ClipFormat::Html => NSPasteboardTypeHTML,
            ClipFormat::Text => NSPasteboardTypeString,
        }
    };
    kind.to_string()
}

#[cfg(not(target_os = "macos"))]
/// The type `format` has on this platform's pasteboard: its MIME type.
#[must_use]
pub fn board_type(format: ClipFormat) -> String {
    format.mime().to_owned()
}

/// The format a pasteboard type is, if it is one.
#[must_use]
pub fn format_of(board_type: &str) -> Option<ClipFormat> {
    ClipFormat::ALL
        .into_iter()
        .find(|&format| self::board_type(format) == board_type)
}

/// The wire type of a type on this board: a format, else an Apple UTI.
#[must_use]
pub fn clip_type(board_type: &str) -> ClipType {
    format_of(board_type).map_or_else(|| ClipType::Apple(board_type.to_owned()), ClipType::Format)
}

/// The type `kind` has on this board; `None` for an Apple type on a board that is not Apple's.
#[must_use]
pub fn type_on_board(kind: &ClipType) -> Option<String> {
    match kind {
        ClipType::Format(format) => Some(board_type(*format)),
        ClipType::Apple(uti) if cfg!(target_os = "macos") => Some(uti.clone()),
        ClipType::Apple(_) => None,
    }
}

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
/// Called on whatever thread AppKit asks from, and must answer before it returns.
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
/// AppKit asks a promise on the main run loop (a block `__CFRunLoopDoBlocks` runs, from the
/// host's `park_main`), and the provider must answer before it returns. That run loop also
/// serves the host's virtual displays and input sources (the main queue), so a promise
/// waiting on a client runs it between looks rather than stalling them for the wait.
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

/// A clipboard the host holds itself, for a machine with none it can sync.
///
/// That is a Linux host, terminal-only and most often headless. The programs there reach it
/// through shims for `xclip`, `xsel`, `wl-copy` and `wl-paste`, so it holds what they copy and
/// what clipboard sync mirrors from the client in front. A promised type is asked of
/// [`Provide`] when first read, outside the lock, and kept.
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
        // The provider may wait seconds on a client: never under the lock.
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

#[cfg(target_os = "macos")]
pub use mac::MacBoard;

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::Arc;

    use objc2::{AllocAnyThread as _, DefinedClass as _, define_class, msg_send};
    use objc2_app_kit::NSPasteboardItemDataProvider;
    use objc2_foundation::{NSObject, NSObjectProtocol};

    use super::{
        Board, Capped, ClipFormat, Item, NSArray, NSData, NSPasteboard,
        NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardType, NSPasteboardWriting,
        NSString, NSURL, ProtocolObject, Provide, Retained, board_type,
    };

    /// What one item's promises answer from.
    struct Promise {
        item: usize,
        provide: Provide,
    }

    define_class!(
        // SAFETY:
        // - `NSObject` has no subclassing requirements.
        // - `Promiser` does not implement `Drop`.
        #[unsafe(super(NSObject))]
        #[name = "OnicodeHostPromise"]
        #[ivars = Promise]
        struct Promiser;

        unsafe impl NSObjectProtocol for Promiser {}

        unsafe impl NSPasteboardItemDataProvider for Promiser {
            #[unsafe(method(pasteboard:item:provideDataForType:))]
            fn provide_data(
                &self,
                _pasteboard: Option<&NSPasteboard>,
                item: &NSPasteboardItem,
                kind: &NSPasteboardType,
            ) {
                let promise = self.ivars();
                let uti = kind.to_string();
                if let Some(bytes) = (promise.provide)(promise.item, &uti) {
                    let set = item.setData_forType(&NSData::with_bytes(&bytes), kind);
                    tracing::debug!(uti, bytes = bytes.len(), set, "promise kept");
                } else {
                    tracing::debug!(uti, "promise not kept");
                }
            }
        }
    );

    impl Promiser {
        fn new(item: usize, provide: Provide) -> Retained<Self> {
            let this = Self::alloc().set_ivars(Promise { item, provide });
            // SAFETY: `NSObject`'s `init` on a freshly allocated instance with ivars set.
            unsafe { msg_send![super(this), init] }
        }
    }

    /// The promisers of the last write, alive for as long as its promises may be asked for.
    struct Kept(Vec<Retained<Promiser>>);

    // SAFETY: a promiser's ivars are `Send + Sync` (an index and an `Arc` of a `Send + Sync`
    // closure) and `NSObject` reference counting is atomic; AppKit messages it from its own
    // threads, and Rust only drops it here.
    unsafe impl Send for Kept {}

    /// Held across every `NSPasteboard` call in this process, and holding the providers of the
    /// last write. AppKit documents no thread rule for the pasteboard, but Apple has told a
    /// developer it is not safe off one thread, and crash reports show its type cache racing
    /// between two threads that use it at once. The host polls from a blocking thread while a
    /// paste writes from another, so they take turns here. A provider never takes it: AppKit
    /// may ask one while a read on another thread holds it.
    static ONE_AT_A_TIME: parking_lot::Mutex<Kept> = parking_lot::Mutex::new(Kept(Vec::new()));

    /// `NSPasteboard`: the general pasteboard, or a named one.
    #[derive(Clone, PartialEq, Eq, Debug, Default)]
    pub struct MacBoard {
        /// `None` is the general pasteboard.
        name: Option<String>,
    }

    impl MacBoard {
        /// The general pasteboard: the one ⌘C and ⌘V use.
        #[must_use]
        pub const fn general() -> Self {
            Self { name: None }
        }

        /// The pasteboard called `name`, created on first use.
        #[must_use]
        pub fn named(name: &str) -> Self {
            Self {
                name: Some(name.to_owned()),
            }
        }

        /// A pasteboard nobody else uses, for a test; [`MacBoard::release`] it after.
        #[must_use]
        pub fn unique() -> Self {
            Self {
                name: Some(NSPasteboard::pasteboardWithUniqueName().name().to_string()),
            }
        }

        /// Its name, `None` for the general pasteboard.
        #[must_use]
        pub fn name(&self) -> Option<&str> {
            self.name.as_deref()
        }

        /// Let the pasteboard server drop a named pasteboard (a no-op for the general one).
        pub fn release(&self) {
            if self.name.is_none() {
                return;
            }
            let _turn = ONE_AT_A_TIME.lock();
            let board = self.board();
            // SAFETY: `-[NSPasteboard releaseGlobally]` takes no arguments and returns void
            // (`NSPasteboard.h`); objc2 does not bind it.
            unsafe {
                let () = objc2::msg_send![&*board, releaseGlobally];
            }
        }

        fn board(&self) -> Retained<NSPasteboard> {
            match &self.name {
                None => NSPasteboard::generalPasteboard(),
                Some(name) => NSPasteboard::pasteboardWithName(&NSString::from_str(name)),
            }
        }
    }

    impl Board for MacBoard {
        fn change_count(&self) -> isize {
            let _turn = ONE_AT_A_TIME.lock();
            self.board().changeCount()
        }

        fn items(&self) -> Vec<Vec<String>> {
            let _turn = ONE_AT_A_TIME.lock();
            let Some(items) = self.board().pasteboardItems() else {
                return Vec::new();
            };
            items
                .iter()
                .map(|item| item.types().iter().map(|t| t.to_string()).collect())
                .collect()
        }

        fn data(&self, item: usize, kind: &str) -> Option<Vec<u8>> {
            let _turn = ONE_AT_A_TIME.lock();
            let items = self.board().pasteboardItems()?;
            if item >= items.count() {
                return None;
            }
            let entry = items.objectAtIndex(item);
            let uti = NSString::from_str(kind);
            if kind == board_type(ClipFormat::FileUrls) {
                let text = entry.stringForType(&uti)?;
                let url = NSURL::URLWithString(&text)?;
                return Some(
                    url.filePathURL()?
                        .absoluteString()?
                        .to_string()
                        .into_bytes(),
                );
            }
            // `dataForType:` copies the bytes out of the pasteboard server, asking the owner's
            // provider first when the type is a promise.
            Some(entry.dataForType(&uti)?.to_vec())
        }

        fn data_within(&self, item: usize, kind: &str, max: u64) -> Option<Capped> {
            if kind == board_type(ClipFormat::FileUrls) {
                let bytes = self.data(item, kind)?;
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                return Some(if size > max {
                    Capped::TooBig(size)
                } else {
                    Capped::Data(bytes)
                });
            }
            let _turn = ONE_AT_A_TIME.lock();
            let items = self.board().pasteboardItems()?;
            if item >= items.count() {
                return None;
            }
            // `dataForType:` brings the bytes over from the pasteboard server either way (about
            // 20–40 ms for 200 MB, `a_capped_read_of_a_big_copy_copies_nothing`); past the cap
            // they are neither copied again nor kept.
            let data = items
                .objectAtIndex(item)
                .dataForType(&NSString::from_str(kind))?;
            let size = u64::try_from(data.length()).unwrap_or(u64::MAX);
            Some(if size > max {
                Capped::TooBig(size)
            } else {
                Capped::Data(data.to_vec())
            })
        }

        fn clear_if(&self, count: isize) -> Option<isize> {
            let mut kept = ONE_AT_A_TIME.lock();
            let board = self.board();
            // Checked and cleared inside one turn: no write of this process lands between.
            if board.changeCount() != count {
                return None;
            }
            let cleared = board.clearContents();
            kept.0.clear();
            drop(kept);
            Some(cleared)
        }

        fn write(&self, items: &[Item], provide: Option<Provide>) -> Option<isize> {
            let mut kept = ONE_AT_A_TIME.lock();
            let mut promisers = Vec::new();
            let objects: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = items
                .iter()
                .enumerate()
                .map(|(n, reps)| {
                    let item = NSPasteboardItem::new();
                    for (kind, bytes) in &reps.data {
                        // `setData:forType:` copies the data; a type the item refuses is left
                        // out.
                        let _set = item
                            .setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(kind));
                    }
                    if let Some(provide) = provide.as_ref().filter(|_| !reps.promised.is_empty()) {
                        let promiser = Promiser::new(n, Arc::clone(provide));
                        let types: Vec<Retained<NSString>> = reps
                            .promised
                            .iter()
                            .map(|t| NSString::from_str(t))
                            .collect();
                        let types = NSArray::from_retained_slice(&types);
                        let _set = item
                            .setDataProvider_forTypes(ProtocolObject::from_ref(&*promiser), &types);
                        promisers.push(promiser);
                    }
                    ProtocolObject::from_retained(item)
                })
                .collect();
            let objects = NSArray::from_retained_slice(&objects);
            let board = self.board();
            // Another process that reads between the clear and the write sees an empty board
            // under the new count, so the items are built first to keep that gap short. Current
            // host only: Universal Clipboard would otherwise fetch every promise at once to hand
            // it to the person's other devices.
            let _count = board
                .prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
            let wrote = items.is_empty() || board.writeObjects(&objects);
            let count = wrote.then(|| board.changeCount());
            kept.0 = promisers;
            drop(kept);
            count
        }
    }
}

#[cfg(test)]
mod held_tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{Board as _, Held, Item, Provide};

    const TEXT: &str = "text/plain;charset=utf-8";
    const PNG: &str = "image/png";

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
        assert_eq!(board.data(0, PNG).map(|b| b.len()), Some(4));
        assert_eq!(board.data(0, PNG).map(|b| b.len()), Some(4));
        assert_eq!(
            asked.load(Ordering::Relaxed),
            1,
            "kept after the first read"
        );

        // The provider runs outside the lock: here it writes the board itself, as a copy
        // landing while a client's bytes are on their way would.
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

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    /// Readers and writers on several threads at once, as the host's poller and a paste are,
    /// each see whole contents and nothing crashes.
    #[test]
    fn threads_take_turns_on_the_pasteboard() {
        let board = MacBoard::unique();
        let text = board_type(ClipFormat::Text);
        std::thread::scope(|scope| {
            for n in 0..4_u8 {
                let (board, text) = (&board, &text);
                scope.spawn(move || {
                    for _ in 0..100 {
                        let mine = vec![n; 64];
                        board.write(&[Item::data(vec![(text.clone(), mine)])], None);
                        let _count = board.change_count();
                        let _types = board.items();
                        if let Some(bytes) = board.data(0, text) {
                            assert!(bytes.len() == 64 && bytes.iter().all(|b| *b == bytes[0]));
                        }
                    }
                });
            }
        });
        board.release();
    }

    /// Each format is a UTI on a Mac's pasteboard, and each of those UTIs is that format again;
    /// any other type travels as the UTI it is.
    #[test]
    fn each_format_is_an_apple_uti_and_back() {
        let utis = [
            (ClipFormat::FileUrls, "public.file-url"),
            (ClipFormat::Png, "public.png"),
            (ClipFormat::Tiff, "public.tiff"),
            (ClipFormat::Rtf, "public.rtf"),
            (ClipFormat::Html, "public.html"),
            (ClipFormat::Text, "public.utf8-plain-text"),
        ];
        for (format, uti) in utis {
            assert_eq!(board_type(format), uti);
            assert_eq!(format_of(uti), Some(format));
            assert_eq!(clip_type(uti), ClipType::Format(format));
            assert_eq!(
                type_on_board(&ClipType::Format(format)).as_deref(),
                Some(uti)
            );
        }
        assert_eq!(format_of("com.adobe.pdf"), None);
        let pdf = clip_type("com.adobe.pdf");
        assert_eq!(pdf, ClipType::Apple("com.adobe.pdf".to_owned()));
        assert_eq!(type_on_board(&pdf).as_deref(), Some("com.adobe.pdf"));
        assert_eq!(format_of(ORIGIN_TYPE), None);
    }

    /// A named pasteboard takes several items, says what each holds, moves its count on a write,
    /// and is released after; the general pasteboard is never touched.
    #[test]
    fn a_named_board_round_trips_items() {
        let board = MacBoard::unique();
        let before = board.change_count();
        let text = board_type(ClipFormat::Text);
        let url = board_type(ClipFormat::FileUrls);
        let items = [
            Item::data(vec![
                (url.clone(), b"file:///tmp/a".to_vec()),
                (ORIGIN_TYPE.to_owned(), b"o".to_vec()),
            ]),
            Item::data(vec![(url.clone(), b"file:///tmp/b".to_vec())]),
        ];
        let after = board.write(&items, None).unwrap();
        assert_ne!(after, before);
        assert_eq!(board.change_count(), after);
        let types = board.items();
        assert_eq!(types.len(), 2, "{types:?}");
        assert!(types[0].iter().any(|t| t == ORIGIN_TYPE), "{types:?}");
        assert_eq!(board.data(1, &url).unwrap(), b"file:///tmp/b");
        assert_eq!(board.data(0, ORIGIN_TYPE).unwrap(), b"o");
        assert_eq!(board.data(2, &url), None, "no third item");

        board
            .write(
                &[Item::data(vec![(
                    text.clone(),
                    "héllo".as_bytes().to_vec(),
                )])],
                None,
            )
            .unwrap();
        assert_eq!(board.data(0, &text).unwrap(), "héllo".as_bytes());
        assert_eq!(board.items().len(), 1);
        let cleared = board.write(&[], None).unwrap();
        assert!(
            cleared > after && board.items().is_empty(),
            "no items clears it"
        );
        board.release();
    }

    /// A promised type is on the board by name, and its bytes come from the provider when read,
    /// asked with the item they belong to.
    #[test]
    fn a_promise_is_kept_when_read() {
        let board = MacBoard::unique();
        let (png, text) = (board_type(ClipFormat::Png), board_type(ClipFormat::Text));
        let asked = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let log = Arc::clone(&asked);
        let provide: Provide = Arc::new(move |item, kind| {
            log.lock().push((item, kind.to_owned()));
            Some(vec![u8::try_from(item).unwrap_or(0); 4])
        });
        let items = [
            Item {
                data: vec![(text.clone(), b"one".to_vec())],
                promised: vec![png.clone()],
            },
            Item {
                data: Vec::new(),
                promised: vec!["com.adobe.pdf".to_owned()],
            },
        ];
        board.write(&items, Some(provide)).unwrap();
        assert!(asked.lock().is_empty(), "nothing asked before a read");
        assert!(board.items()[0].contains(&png), "{:?}", board.items());
        assert_eq!(board.data(0, &png).unwrap(), [0; 4]);
        assert_eq!(board.data(1, "com.adobe.pdf").unwrap(), [1; 4]);
        assert_eq!(board.data(0, &text).unwrap(), b"one");
        assert_eq!(*asked.lock(), [(0, png), (1, "com.adobe.pdf".to_owned())]);
        board.release();
    }

    /// A capped read of another process's big copy gives its size and keeps none of its bytes;
    /// the whole read of the same copy is timed beside it.
    #[test]
    fn a_capped_read_of_a_big_copy_copies_nothing() {
        const WRITER: &str = "ONICODE_TEST_BIG_COPY_ON";
        const SIZE: usize = 200 << 20;
        let png = board_type(ClipFormat::Png);
        if let Ok(name) = std::env::var(WRITER) {
            // The other process: another app's copy, which stays on the board once it exits.
            MacBoard::named(&name)
                .write(&[Item::data(vec![(png, vec![7; SIZE])])], None)
                .unwrap();
            return;
        }
        let board = MacBoard::unique();
        let name = board.name().unwrap().to_owned();
        let wrote = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pasteboard::tests::a_capped_read_of_a_big_copy_copies_nothing",
            ])
            .env(WRITER, &name)
            .status()
            .unwrap();
        assert!(wrote.success());
        let began = std::time::Instant::now();
        let capped = board.data_within(0, &png, 1 << 20);
        let capped_took = began.elapsed();
        assert_eq!(capped, Some(Capped::TooBig(SIZE as u64)));
        let began = std::time::Instant::now();
        let whole = board.data(0, &png).unwrap();
        let whole_took = began.elapsed();
        assert_eq!(whole.len(), SIZE);
        println!("200 MB copy: capped read {capped_took:?}, whole read {whole_took:?}");
        board.release();
    }

    /// A look at a board that has not changed is one change-count call, cheap enough to make
    /// 20 times a second. Its wall time bounds what the poller's thread spends on it.
    #[test]
    fn a_look_at_an_unchanged_board_is_one_cheap_call() {
        let board = MacBoard::unique();
        let text = board_type(ClipFormat::Text);
        board
            .write(&[Item::data(vec![(text, b"still".to_vec())])], None)
            .unwrap();
        let _warm = board.change_count();
        let looks = 2000_u32;
        let began = std::time::Instant::now();
        for _ in 0..looks {
            let _count = board.change_count();
        }
        let each = began.elapsed() / looks;
        let share = each.as_secs_f64() * 20.0 * 100.0;
        println!("one look: {each:?}; at 50 ms that is {share:.4} % of a core");
        board.release();
        assert!(each < std::time::Duration::from_micros(500), "{each:?}");
    }
}
