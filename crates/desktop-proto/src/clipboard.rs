//! The clipboard while a desktop window has focus (`docs/onicode/decisions/remote-desktop.md`,
//! "The clipboard follows the window"; the design is `one-machine.md`'s "Clipboard").
//!
//! Eager: a copy is announced when it is made ([`Clip::Offer`]) and the other end takes all of it
//! at once, so a paste never waits on the link. Items up to [`INLINE_MAX`] ride inside the offer.
//! The rest is fetched as soon as the offer lands, on a stream of its own, never on the control
//! stream, so a big picture holds up no input or cursor: the fetcher opens a file stream named by
//! [`Fetch::name`] and reads the bytes back raw (a type), or as [`FileEntry`] frames (a copied
//! file). A refused fetch is a reset stream ([`REFUSED`]). How much travels is capped by
//! `onicode-clipboard`.
//!
//! Ported from onicode's peer clipboard (`git show f01736b3^:crates/proto/src/clipboard.rs`),
//! scoped to the desktop session: the pasteboard and the sync logic are `onicode-clipboard`'s.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Items up to this many bytes ride inside the offer.
pub const INLINE_MAX: u64 = 16 << 10;

/// The reset code of a fetch the origin refuses: the copy moved on, or it never offered that.
pub const REFUSED: u32 = 0x636c_6970;

/// Bytes on the wire as one base64 string: a JSON array of numbers costs four times as much.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Inline(pub Vec<u8>);

impl fmt::Debug for Inline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Inline({} bytes)", self.0.len())
    }
}

impl Serialize for Inline {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Inline {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// One pasteboard type of an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardType {
    /// Its uniform type identifier (`public.utf8-plain-text`).
    pub uti: String,
    /// Its bytes.
    pub size: u64,
    /// The bytes themselves, when `size <= INLINE_MAX`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<Inline>,
    /// For a `public.file-url`, the size of the regular file it names; `None` for a folder or
    /// when unknown. Only regular files of known size travel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<u64>,
}

/// One pasteboard item: the same content in each of its types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardItem {
    /// Its types, the richest first.
    pub types: Vec<ClipboardType>,
}

/// A copy, announced to the other end of the session. The newest `(at, origin, seq)` wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardOffer {
    /// The device it was copied on.
    pub origin: String,
    /// That device's count of offers, which names this one in a fetch.
    pub seq: u64,
    /// When it was copied, epoch milliseconds, never behind the newest offer its origin saw.
    pub at: i64,
    /// What was copied.
    pub items: Vec<ClipboardItem>,
}

/// Whether a Mac may read its general pasteboard without a prompt (macOS 15.4+'s "Paste from
/// Other Apps"). Copies leave a Mac only where it may.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum PasteAccess {
    /// No pasteboard privacy here: the foreground app's own reads, or a board in memory.
    #[default]
    Ungated,
    /// Never asked yet: a read would prompt, and the app is not yet listed in System Settings.
    NotAsked,
    /// Listed and set to ask: a read would prompt.
    Ask,
    /// Listed and allowed: reads never prompt.
    Allowed,
    /// Listed and denied: reads fail.
    Denied,
}

impl PasteAccess {
    /// Whether copies may be read, and so sent, when they are made. Otherwise nothing is read,
    /// so nobody is prompted, and copies on that Mac stay there.
    #[must_use]
    pub const fn reads_freely(self) -> bool {
        matches!(self, Self::Ungated | Self::Allowed)
    }
}

/// The clipboard's messages on the session's control stream, either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Clip {
    /// The window showing the other end gained or lost focus: while it has it, the clipboard
    /// follows the session both ways. Sent by the viewer.
    Focus {
        /// It has focus now.
        focused: bool,
    },
    /// A copy on the sender's pasteboard.
    Offer(ClipboardOffer),
    /// Whether the sender may read its pasteboard without a prompt, when following starts and
    /// whenever it changes. Sent by the host, so the viewer can say why copies there stay there.
    Access(PasteAccess),
}

/// One fetch from an offer's origin, named on the stream that carries it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fetch {
    /// The bytes of one type of one item, raw: `clip/<seq>/<item>/<type>`.
    Type {
        /// The offer.
        seq: u64,
        /// The item, by its place in the offer: a copy of several pictures has one type per item.
        item: usize,
        /// The type.
        uti: String,
    },
    /// The file one item names, as [`FileEntry`] frames: `clipfiles/<seq>/<item>`.
    Files {
        /// The offer.
        seq: u64,
        /// The item.
        item: usize,
    },
}

impl Fetch {
    /// The stream's name.
    #[must_use]
    pub fn name(&self) -> String {
        match self {
            Self::Type { seq, item, uti } => format!("clip/{seq}/{item}/{uti}"),
            Self::Files { seq, item } => format!("clipfiles/{seq}/{item}"),
        }
    }

    /// Reads [`Self::name`] back. `clip/<seq>/<type>` means item 0.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        if let Some(rest) = name.strip_prefix("clipfiles/") {
            let (seq, item) = rest.split_once('/')?;
            return Some(Self::Files {
                seq: seq.parse().ok()?,
                item: item.parse().ok()?,
            });
        }
        let rest = name.strip_prefix("clip/")?;
        let (seq, rest) = rest.split_once('/')?;
        let seq = seq.parse().ok()?;
        let (item, uti) = match rest.split_once('/') {
            Some((item, uti)) if item.bytes().all(|b| b.is_ascii_digit()) && !item.is_empty() => {
                (item.parse().ok()?, uti)
            }
            _ => (0, rest),
        };
        (!uti.is_empty()).then(|| Self::Type {
            seq,
            item,
            uti: uti.to_owned(),
        })
    }
}

/// One entry of a copied file on a [`Fetch::Files`] stream: a frame of its own, as JSON,
/// and for a file its bytes after it, in frames that add up to `size`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Its path under the copy, `/`-separated, starting with the copied item's own name.
    pub path: String,
    /// A file's size; `None` for a folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// A file anyone may run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exec: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_names_read_back() {
        for fetch in [
            Fetch::Type {
                seq: 7,
                item: 2,
                uti: "public.png".into(),
            },
            Fetch::Type {
                seq: 1,
                item: 0,
                uti: "com.example/odd".into(),
            },
            Fetch::Files { seq: 9, item: 3 },
        ] {
            assert_eq!(Fetch::parse(&fetch.name()), Some(fetch));
        }
        assert_eq!(
            Fetch::parse("clip/5/public.utf8-plain-text"),
            Some(Fetch::Type {
                seq: 5,
                item: 0,
                uti: "public.utf8-plain-text".into()
            }),
            "no item is item 0"
        );
        for bad in [
            "clip/",
            "clip/x/0/public.png",
            "clip/1/0/",
            "clipfiles/1",
            "doc/1",
        ] {
            assert_eq!(Fetch::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn inline_bytes_are_one_base64_string() {
        let kind = ClipboardType {
            uti: "public.utf8-plain-text".into(),
            size: 5,
            inline: Some(Inline(b"hello".to_vec())),
            file_size: None,
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("\"aGVsbG8=\""), "{json}");
        assert_eq!(serde_json::from_str::<ClipboardType>(&json).unwrap(), kind);
        let clip = Clip::Offer(ClipboardOffer {
            origin: "mac".into(),
            seq: 1,
            at: 2,
            items: vec![ClipboardItem { types: vec![kind] }],
        });
        let json = serde_json::to_string(&clip).unwrap();
        assert_eq!(serde_json::from_str::<Clip>(&json).unwrap(), clip);
    }

    #[test]
    fn only_allowed_or_ungated_reads_freely() {
        assert!(PasteAccess::Ungated.reads_freely());
        assert!(PasteAccess::Allowed.reads_freely());
        assert!(!PasteAccess::Ask.reads_freely());
        assert!(!PasteAccess::NotAsked.reads_freely());
        assert!(!PasteAccess::Denied.reads_freely());
    }
}
