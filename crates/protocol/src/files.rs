//! The Files group: one directory's listing as the engine's file watch sees it ([`WatchDir`]),
//! and the calls that read, write and search the engine's disk. The mutating calls ([`Write`],
//! [`Create`], [`Move`], [`DeletePath`], [`SaveAttachments`] and [`SaveNoteImages`]) travel with an
//! [`OpId`](crate::OpId) in their [`Routed`](crate::Routed): a repeat of the op is answered with
//! the first reply and changes nothing again. A call that changes a watched directory relists it
//! before it replies, and its reply names, for each watched directory it touched, the scan of the
//! listing that covers it ([`Relisted`]): a viewer keeps its echo over that directory until it
//! holds a listing at or after that scan, so a listing read before the call that arrives after its
//! reply never flashes the old state.
//!
//! File contents travel as one reply each, under the editor's caps: text up to 2 MB, a preview
//! up to 32 MB, image bytes as base64 ([`Bytes`]) rather than a JSON array, and a poll that names
//! the [`Read::unless`] digest it holds gets [`FileContents::Unchanged`] instead of the file.
pub use onicode_core::files::{DirEntry, ProjectFile};
pub use onicode_core::notes::NoteImageAsset;
pub use onicode_core::search::project::{SearchMatch, SearchOptions, SearchResult};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{Call, Cursor, Outcome, Watch};

/// Bytes on the wire as one base64 string: a JSON array of numbers costs four times as much to
/// carry and far more to hold as a `serde_json::Value`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        serializer.serialize_str(&onicode_core::b64::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let text = String::deserialize(deserializer)?;
        onicode_core::b64::STANDARD
            .decode(text)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// What a directory held when it was last read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Entries {
    /// Folders first, then names in natural order, as `files::list_dir` sorts them.
    Read(Vec<DirEntry>),
    /// The directory could not be read, with why.
    Failed(String),
}

/// One directory's children as of a scan. `scan` orders the engine's directory reads: listings
/// read together share one, a later read has a higher one (across engine runs too), and a watch
/// never offers a listing older than one it offered before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub scan: u64,
    pub entries: Entries,
}

/// One change to a watched directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirEvent {
    /// The directory's children changed: replace the listing whole.
    Listing(Listing),
}

/// Watch the directory at `path` inside the checkout or folder `root`, whose file watch the
/// engine shares between every directory under it: a listing, then a new listing whenever the
/// directory's children (or the ignore rules over them) change. Changes coalesce: a burst of
/// writes is one relisting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchDir {
    pub root: String,
    pub path: String,
    /// The last cursor this viewer holds, to resume from.
    pub since: Option<Cursor>,
}

impl Watch for WatchDir {
    type Event = DirEvent;
    type Params = Self;
    type State = Listing;

    const NAME: &'static str = "WatchDir";
}

macro_rules! files_call {
    ($name:ident,agent: $agent:literal,reply: $reply:ty) => {
        impl Call for $name {
            type Params = Self;
            type Reply = $reply;

            const AGENT: bool = $agent;
            const NAME: &'static str = stringify!($name);
        }
    };
}

/// One directory's children, read once.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDir {
    pub path: String,
}
files_call!(ListDir, agent: true, reply: Vec<DirEntry>);

/// What is at a path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathKind {
    #[default]
    Missing,
    File,
    Dir,
    /// A socket, device or other special file.
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stat {
    pub path: String,
}
files_call!(Stat, agent: true, reply: PathStat);

/// [`Stat`]'s reply. Symlinks are followed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathStat {
    pub kind: PathKind,
    pub size: u64,
    /// Last modified, in Unix milliseconds.
    pub modified_ms: Option<u64>,
}

/// How a text file's line endings and byte order mark were written, so a save writes them back.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEncoding {
    pub crlf: bool,
    pub cr: bool,
    pub bom: bool,
}

/// A text file the editor can open: `text` with its line endings made `\n`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextFile {
    pub text: String,
    pub encoding: TextEncoding,
    /// The digest of the bytes on disk: a later [`Read::unless`] or [`Write::expected`].
    pub digest: u64,
}

/// An image the editor previews.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageFile {
    pub bytes: Bytes,
    /// `png`, `jpeg`, `gif`, `webp`, `ico`, `avif` or `bmp`.
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub original_size: u64,
    pub digest: u64,
}

/// [`Read`]'s reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileContents {
    /// The file's digest is still [`Read::unless`].
    Unchanged,
    Text(TextFile),
    Image(ImageFile),
    /// A video plays from its path; only what tells a change is sent.
    Video {
        size: u64,
        modified_ms: Option<u64>,
    },
    /// Not shown, with why: too large, binary, not UTF-8 or not a readable image. The editor's
    /// own wording, as `files::document::read_document` writes it.
    Unsupported(String),
}

/// Read a file for the editor, within its caps (2 MB of text, 16 KB per line, 32 MB of preview).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Read {
    pub path: String,
    /// The digest the viewer holds: an unchanged file answers [`FileContents::Unchanged`].
    pub unless: Option<u64>,
}
files_call!(Read, agent: true, reply: FileContents);

/// Save the editor's text atomically, keeping permissions and symlinks, only while the file on
/// disk is still the one read as `expected`: a file changed underneath is refused with the
/// editor's "The file changed on disk" error.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Write {
    pub path: String,
    pub text: String,
    pub encoding: TextEncoding,
    pub expected: u64,
}
files_call!(Write, agent: false, reply: Written);

/// [`Write`]'s reply: the digest of the bytes now on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Written {
    pub digest: u64,
}

/// Create a file or folder named `name` (which may hold `/`, making the folders between) in
/// `parent`. An existing path is never replaced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Create {
    pub parent: String,
    pub name: String,
    pub dir: bool,
}
files_call!(Create, agent: false, reply: NewPath);

/// [`Create`]'s and [`Move`]'s reply: where the file is now, and the listings that cover the
/// change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPath {
    pub path: String,
    pub listed: Vec<Relisted>,
}

/// A watched directory a mutating call changed, under one watched root: its watchers hold a
/// listing that shows the change once they hold one at `scan` or later.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Relisted {
    pub root: String,
    pub path: String,
    pub scan: u64,
}

/// Where [`Move`] takes a file or folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Destination {
    /// The same folder, under this name.
    Rename(String),
    /// Into this folder, keeping the name.
    Into(String),
    /// A copy into this folder, renamed as Finder does when the name is taken.
    CopyInto(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Move {
    pub from: String,
    pub to: Destination,
}
files_call!(Move, agent: false, reply: NewPath);

/// Delete a file, or a folder and everything in it. Named apart from the Workspace group's
/// `Delete`, which deletes a session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePath {
    pub path: String,
}
files_call!(DeletePath, agent: false, reply: Deleted);

/// [`DeletePath`]'s reply: the listings that cover the deletion.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deleted {
    pub listed: Vec<Relisted>,
}

/// Every file of the project at `root` for Quick Open and mentions: `git ls-files` where it
/// can, else a bounded walk; up to 20,000. A later search under the same non-empty `search` id,
/// or a [`StopSearch`] naming it, stops this one, which then answers no files.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPaths {
    pub root: String,
    pub search: String,
}
files_call!(SearchPaths, agent: true, reply: PathList);

/// [`SearchPaths`]'s reply.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PathList {
    pub files: Vec<ProjectFile>,
}

/// Search the contents of the project at `options.cwd`, up to 500 matches. A later search under
/// the same non-empty `options.search_id`, or a [`StopSearch`] naming it, stops this one, which
/// then answers no matches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchContent {
    pub options: SearchOptions,
}
files_call!(SearchContent, agent: true, reply: SearchResult);

/// Stop the search running under `search` ([`SearchPaths`], [`SearchContent`] or the Workspace
/// group's `SearchSessions`): a view that closes, or whose query moves on, lets its search go as
/// monocode's cancel flag did. A stop that arrives before its search does stops it as it starts,
/// so a viewer mints a fresh id for each search it may stop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopSearch {
    pub search: String,
}
files_call!(StopSearch, agent: true, reply: Outcome);

/// The disks an engine's folders can be opened from: its root and every mounted volume.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListDrives {}
files_call!(ListDrives, agent: false, reply: Vec<Drive>);

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Drive {
    pub name: String,
    pub path: String,
}

/// Folders under the engine's home whose name holds `query`, nearest first, up to `limit`:
/// "Open folder on a machine".
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHomeFolders {
    pub query: String,
    pub limit: u32,
}
files_call!(SearchHomeFolders, agent: false, reply: Vec<String>);

/// A pasted image to keep as an attachment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAttachment {
    pub bytes: Bytes,
    /// Without the dot: `png`, `jpg`…
    pub extension: String,
}

/// Keep pasted images as attachments of the session stored at `session` (in its own folder, so
/// deleting the session removes them; `kept` are the composer's attachments already, and
/// together they stay within 20 files and 20 MB), or, with no session yet, in the quick
/// composer's staging folder, which a new session adopts its files from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveAttachments {
    pub session: Option<i64>,
    pub kept: Vec<String>,
    pub images: Vec<NewAttachment>,
}
files_call!(SaveAttachments, agent: false, reply: Vec<String>);

/// Copy image files into a note's assets. Answers the images it kept, or the first failure when
/// it kept none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveNoteImages {
    pub note: String,
    pub paths: Vec<String>,
}
files_call!(SaveNoteImages, agent: false, reply: Vec<NoteImageAsset>);
