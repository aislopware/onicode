//! The pasteboard seam: what clipboard sync needs from a pasteboard.
//!
//! That is its `changeCount` (macOS has no change notification, so it is polled), the types of
//! each item it holds, one representation's bytes when asked, and a way to replace the contents
//! with bytes. `MacBoard` is `NSPasteboard`; [`Held`] is a clipboard kept in memory: every
//! test's, and a desktopd that runs drawn displays.

/// One item to write: its representations.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Item {
    /// `(type, bytes)`, in the order the copying app ranked them.
    pub data: Vec<(String, Vec<u8>)>,
}

impl Item {
    /// An item of `data`.
    #[must_use]
    pub const fn data(data: Vec<(String, Vec<u8>)>) -> Self {
        Self { data }
    }
}

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
    /// Replace the contents with `items`; no items clears it. The `changeCount` the write left,
    /// `None` when it failed.
    fn write(&self, items: &[Item]) -> Option<isize>;
    /// Clear the contents if the count is still `count`: what was written then is still there.
    /// The count the clear left; `None` when the contents moved on, or the clear failed.
    fn clear_if(&self, count: isize) -> Option<isize> {
        if self.change_count() == count {
            self.write(&[])
        } else {
            None
        }
    }
}

/// A clipboard kept in memory: every test's, and a host with no pasteboard of the person's to
/// follow (desktopd's drawn displays).
#[derive(Default)]
pub struct Held {
    contents: parking_lot::Mutex<Contents>,
}

/// What a [`Held`] board holds.
#[derive(Default)]
struct Contents {
    count: isize,
    items: Vec<Item>,
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
        self.contents
            .lock()
            .items
            .iter()
            .map(|item| item.data.iter().map(|(t, _)| t.clone()).collect())
            .collect()
    }

    fn data(&self, item: usize, kind: &str) -> Option<Vec<u8>> {
        self.contents
            .lock()
            .items
            .get(item)?
            .data
            .iter()
            .find(|(t, _)| t == kind)
            .map(|(_, bytes)| bytes.clone())
    }

    fn write(&self, items: &[Item]) -> Option<isize> {
        let mut contents = self.contents.lock();
        contents.count = contents.count.wrapping_add(1);
        contents.items = items.to_vec();
        Some(contents.count)
    }
}

#[cfg(test)]
mod tests {
    use super::{Board as _, Held, Item};

    const TEXT: &str = "public.utf8-plain-text";
    const PNG: &str = "public.png";

    /// A copy holds its types in order and reads back; each write moves the count, and an
    /// empty write clears it.
    #[test]
    fn a_held_board_keeps_what_is_written() {
        let board = Held::default();
        let start = board.change_count();
        let item = Item::data(vec![(TEXT.to_owned(), b"hi".to_vec())]);
        let wrote = board.write(&[item]).unwrap();
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
}
