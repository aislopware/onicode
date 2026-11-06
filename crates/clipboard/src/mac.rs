//! `NSPasteboard` as a [`Board`]: the general pasteboard, or a named one for a test.

use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread as _, DefinedClass as _, define_class, msg_send};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardContentsOptions, NSPasteboardItem, NSPasteboardItemDataProvider,
    NSPasteboardType, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSObject, NSObjectProtocol, NSString, NSURL};

use crate::board::{Board, Capped, Item, Provide};
use crate::types::FILE_URL_TYPE;

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
    #[name = "OnicodeClipboardPromise"]
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
/// between two threads that use it at once. The watcher polls from a blocking thread while a
/// peer's offer writes from another, so they take turns here. A provider never takes it:
/// AppKit may ask one while a read on another thread holds it.
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
        if kind == FILE_URL_TYPE {
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
        if kind == FILE_URL_TYPE {
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
                    // `setData:forType:` copies the data; a type the item refuses is left out.
                    let _set =
                        item.setData_forType(&NSData::with_bytes(bytes), &NSString::from_str(kind));
                }
                if let Some(provide) = provide.as_ref().filter(|_| !reps.promised.is_empty()) {
                    let promiser = Promiser::new(n, Arc::clone(provide));
                    let types: Vec<Retained<NSString>> = reps
                        .promised
                        .iter()
                        .map(|t| NSString::from_str(t))
                        .collect();
                    let types = NSArray::from_retained_slice(&types);
                    let _set =
                        item.setDataProvider_forTypes(ProtocolObject::from_ref(&*promiser), &types);
                    promisers.push(promiser);
                }
                ProtocolObject::from_retained(item)
            })
            .collect();
        let objects = NSArray::from_retained_slice(&objects);
        let board = self.board();
        // Another process that reads between the clear and the write sees an empty board under
        // the new count, so the items are built first to keep that gap short. Current host
        // only: Universal Clipboard would otherwise fetch every promise at once to hand it to
        // the person's other devices.
        let _count =
            board.prepareForNewContentsWithOptions(NSPasteboardContentsOptions::CurrentHostOnly);
        let wrote = items.is_empty() || board.writeObjects(&objects);
        let count = wrote.then(|| board.changeCount());
        kept.0 = promisers;
        drop(kept);
        count
    }
}

/// Opt-in (`ONICODE_PASTEBOARD_LIVE=1`): each test writes only a uniquely named pasteboard and
/// releases it, never the general one, and nothing here can prompt.
#[cfg(test)]
mod tests {
    use onicode_desktop_testkit::live::{PASTEBOARD_LIVE, opted_in};

    use super::*;
    use crate::types::{ClipFormat, ORIGIN_TYPE, board_type};

    /// Readers and writers on several threads at once, as the watcher and a peer's offer are,
    /// each see whole contents and nothing crashes.
    #[test]
    fn threads_take_turns_on_the_pasteboard() {
        if !opted_in(PASTEBOARD_LIVE) {
            return;
        }
        let board = MacBoard::unique();
        let text = board_type(ClipFormat::Text);
        std::thread::scope(|scope| {
            for n in 0..4_u8 {
                let board = &board;
                scope.spawn(move || {
                    for _ in 0..100 {
                        let mine = vec![n; 64];
                        board.write(&[Item::data(vec![(text.to_owned(), mine)])], None);
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

    /// A named pasteboard takes several items, says what each holds, moves its count on a write,
    /// and is released after; the general pasteboard is never touched.
    #[test]
    fn a_named_board_round_trips_items() {
        if !opted_in(PASTEBOARD_LIVE) {
            return;
        }
        let board = MacBoard::unique();
        let before = board.change_count();
        let text = board_type(ClipFormat::Text);
        let url = board_type(ClipFormat::FileUrls);
        let items = [
            Item::data(vec![
                (url.to_owned(), b"file:///tmp/a".to_vec()),
                (ORIGIN_TYPE.to_owned(), b"o".to_vec()),
            ]),
            Item::data(vec![(url.to_owned(), b"file:///tmp/b".to_vec())]),
        ];
        let after = board.write(&items, None).unwrap();
        assert_ne!(after, before);
        assert_eq!(board.change_count(), after);
        let types = board.items();
        assert_eq!(types.len(), 2, "{types:?}");
        assert!(types[0].iter().any(|t| t == ORIGIN_TYPE), "{types:?}");
        assert_eq!(board.data(1, url).unwrap(), b"file:///tmp/b");
        assert_eq!(board.data(0, ORIGIN_TYPE).unwrap(), b"o");
        assert_eq!(board.data(2, url), None, "no third item");

        board
            .write(
                &[Item::data(vec![(
                    text.to_owned(),
                    "héllo".as_bytes().to_vec(),
                )])],
                None,
            )
            .unwrap();
        assert_eq!(board.data(0, text).unwrap(), "héllo".as_bytes());
        assert_eq!(board.items().len(), 1);
        let cleared = board.write(&[], None).unwrap();
        assert!(
            cleared > after && board.items().is_empty(),
            "no items clears it"
        );
        board.release();
    }

    /// A promised type is on the board by name, and its bytes come from the provider when read,
    /// asked with the item they belong to; a clear of the promise's count drops it unasked.
    #[test]
    fn a_promise_is_kept_when_read() {
        if !opted_in(PASTEBOARD_LIVE) {
            return;
        }
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
                data: vec![(text.to_owned(), b"one".to_vec())],
                promised: vec![png.to_owned()],
            },
            Item {
                data: Vec::new(),
                promised: vec!["com.adobe.pdf".to_owned()],
            },
        ];
        board.write(&items, Some(Arc::clone(&provide))).unwrap();
        assert!(asked.lock().is_empty(), "nothing asked before a read");
        assert!(
            board.items()[0].iter().any(|t| t == png),
            "{:?}",
            board.items()
        );
        assert_eq!(board.data(0, png).unwrap(), [0; 4]);
        assert_eq!(board.data(1, "com.adobe.pdf").unwrap(), [1; 4]);
        assert_eq!(board.data(0, text).unwrap(), b"one");
        assert_eq!(
            *asked.lock(),
            [(0, png.to_owned()), (1, "com.adobe.pdf".to_owned())]
        );

        assert!(board.clear_if(board.change_count()).is_some());
        assert!(board.items().is_empty());
        board.release();
    }

    /// A capped read of another process's big copy gives its size and keeps none of its bytes.
    #[test]
    fn a_capped_read_of_a_big_copy_copies_nothing() {
        const WRITER: &str = "ONICODE_TEST_BIG_COPY_ON";
        const SIZE: usize = 200 << 20;
        let png = board_type(ClipFormat::Png);
        if let Ok(name) = std::env::var(WRITER) {
            // The other process: another app's copy, which stays on the board once it exits.
            MacBoard::named(&name)
                .write(&[Item::data(vec![(png.to_owned(), vec![7; SIZE])])], None)
                .unwrap();
            return;
        }
        if !opted_in(PASTEBOARD_LIVE) {
            return;
        }
        let board = MacBoard::unique();
        let name = board.name().unwrap().to_owned();
        let wrote = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "mac::tests::a_capped_read_of_a_big_copy_copies_nothing",
            ])
            .env(WRITER, &name)
            .status()
            .unwrap();
        assert!(wrote.success());
        let capped = board.data_within(0, png, 1 << 20);
        assert_eq!(capped, Some(Capped::TooBig(SIZE as u64)));
        assert_eq!(board.data(0, png).unwrap().len(), SIZE);
        board.release();
    }
}
