//! The clipboard's types as every end names them, whatever its platform's pasteboard calls them.
//!
//! Ported from slopty's `slopty-proto` `transfer.rs` (MIT, same authors), cut to what a
//! pasteboard needs (`onicode_desktop_input::pasteboard`): the formats, the type a
//! representation travels as, and which pasteboard types travel at all. The clipboard's control
//! messages and its bulk streams come with the orchestration that carries them.

use serde::{Deserialize, Serialize};

/// The private pasteboard type every onicode write carries: a watcher that finds it knows the
/// change came from onicode and does not announce it back.
pub const ORIGIN_TYPE: &str = "com.aislopware.onicode.origin";

/// nspasteboard.org's marker for a secret, such as a password manager's copy: offered as
/// concealed and never kept.
pub const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";

/// nspasteboard.org's marker for contents that are about to go again: offered and kept as a
/// secret is.
pub const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

/// Whether a type on an Apple pasteboard travels in an offer.
///
/// Left behind: dynamic types (`dyn.…`), which name nothing on another machine; the
/// pre-UTI names AppKit still lists beside their UTIs (`NSStringPboardType`, `Apple PNG
/// pasteboard type`, `CorePasteboardFlavorType 0x…`), whose bytes the UTI already carries;
/// file promises, which only the copying process can keep; and the markers, which an offer
/// says in its own fields.
#[must_use]
pub fn carried(board_type: &str) -> bool {
    let legacy = !board_type.contains('.') || board_type.contains(' ');
    let marker = board_type == ORIGIN_TYPE || board_type.starts_with("org.nspasteboard.");
    let promise = board_type.starts_with("com.apple.pasteboard.promised-")
        || board_type == "com.apple.NSFilePromiseItemMetaData";
    !(legacy || marker || promise || board_type.starts_with("dyn."))
}

/// A representation every end knows by name, whatever its platform calls it.
///
/// Each end maps it to its own pasteboard's types at its board (an Apple UTI on a Mac or an
/// iPhone, a MIME type elsewhere).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ClipFormat {
    /// A file, as its `file://` URL: one per item (`text/uri-list`).
    FileUrls,
    /// `image/png`.
    Png,
    /// `image/tiff`, what a Mac screenshot is on its pasteboard.
    Tiff,
    /// `text/rtf`.
    Rtf,
    /// `text/html`.
    Html,
    /// `text/plain`, UTF-8.
    Text,
}

impl ClipFormat {
    /// Every format, richest first.
    pub const ALL: [Self; 6] = [
        Self::FileUrls,
        Self::Png,
        Self::Tiff,
        Self::Rtf,
        Self::Html,
        Self::Text,
    ];

    /// The MIME type.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::FileUrls => "text/uri-list",
            Self::Png => "image/png",
            Self::Tiff => "image/tiff",
            Self::Rtf => "text/rtf",
            Self::Html => "text/html",
            Self::Text => "text/plain;charset=utf-8",
        }
    }

    /// A picture: what a paste into a program that reads pictures off the pasteboard takes.
    #[must_use]
    pub const fn is_picture(self) -> bool {
        matches!(self, Self::Png | Self::Tiff)
    }
}

/// A representation's type: a format every end knows, or an Apple uniform type identifier.
///
/// Between two Apple ends, the common case, every type a pasteboard holds travels as it is
/// (`com.apple.webarchive`, `com.adobe.pdf`, an app's private type), since macOS and iOS
/// pasteboards both take any UTI. An end that is not Apple's leaves an `Apple` type alone.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ClipType {
    /// A format with a name on every platform.
    Format(ClipFormat),
    /// An Apple uniform type identifier that is none of the formats.
    Apple(String),
}

impl ClipType {
    /// The format, when it is one.
    #[must_use]
    pub const fn format(&self) -> Option<ClipFormat> {
        match self {
            Self::Format(format) => Some(*format),
            Self::Apple(_) => None,
        }
    }

    /// Whether it is `format`.
    #[must_use]
    pub fn is(&self, format: ClipFormat) -> bool {
        self.format() == Some(format)
    }

    /// A picture format.
    #[must_use]
    pub fn is_picture(&self) -> bool {
        self.format().is_some_and(ClipFormat::is_picture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Apple type travels but dynamic ones, the pre-UTI names, file promises and the
    /// markers an offer says in its own fields.
    #[test]
    fn only_types_that_mean_something_elsewhere_travel() {
        for kept in [
            "public.png",
            "com.apple.webarchive",
            "com.adobe.pdf",
            "public.file-url",
        ] {
            assert!(carried(kept), "{kept}");
        }
        for left in [
            "dyn.ah62d4rv4gu8y",
            "NSStringPboardType",
            "Apple PNG pasteboard type",
            "CorePasteboardFlavorType 0x75726C20",
            "com.apple.pasteboard.promised-file-url",
            ORIGIN_TYPE,
            CONCEALED_TYPE,
            TRANSIENT_TYPE,
            "org.nspasteboard.source",
        ] {
            assert!(!carried(left), "{left}");
        }
    }

    /// A type is a format or an Apple type, and says which.
    #[test]
    fn a_type_says_its_format() {
        let png = ClipType::Format(ClipFormat::Png);
        assert!(png.is(ClipFormat::Png) && png.is_picture());
        let pdf = ClipType::Apple("com.adobe.pdf".to_owned());
        assert_eq!(pdf.format(), None);
        assert!(!pdf.is_picture());
        assert!(ClipFormat::ALL.iter().all(|f| !f.mime().is_empty()));
    }
}
