//! The Files group: one directory's listing as the engine's file watch sees it ([`WatchDir`]),
//! and the calls that read, write and search the engine's disk. The mutating calls ([`Write`],
//! [`Create`], [`Move`], [`ImportFiles`], [`DeletePath`], [`SaveAttachments`] and
//! [`SaveNoteImages`]) travel with an
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
pub mod document;
mod name;
mod paths;

use std::time::{SystemTime, UNIX_EPOCH};

pub use name::{
    NameIssue, NameIssueKind, Severity, leaf_name, validate_file_name, well_formed_file_name,
};
pub use paths::{
    create_parent_of, dirs_touched_by_create, dirs_touched_by_move, display_path,
    is_equal_or_inside, join_path, parent_path, path_key, rebase_path,
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use crate::notes::NoteImageAsset;
pub use crate::search::project::{SearchMatch, SearchOptions, SearchResult};
use crate::{Call, Cursor, Outcome, Watch};

/// One entry of a directory listing.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub ignored: bool,
}

/// Case-insensitive order where digit runs compare by numeric value.
pub fn compare_natural_names(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let a_start = i;
            let b_start = j;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let a_digits = &a[a_start..i];
            let b_digits = &b[b_start..j];
            let a_value = a_digits
                .iter()
                .position(|digit| *digit != b'0')
                .map_or(&a_digits[a_digits.len()..], |start| &a_digits[start..]);
            let b_value = b_digits
                .iter()
                .position(|digit| *digit != b'0')
                .map_or(&b_digits[b_digits.len()..], |start| &b_digits[start..]);
            let order = a_value
                .len()
                .cmp(&b_value.len())
                .then_with(|| a_value.cmp(b_value));
            if order != Ordering::Equal {
                return order;
            }
        } else {
            let order = a[i].to_ascii_lowercase().cmp(&b[j].to_ascii_lowercase());
            if order != Ordering::Equal {
                return order;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j)).then_with(|| a.cmp(b))
}

fn split_stem_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// The name a copy of `name` takes beside the names `taken` says are in use, as Finder names
/// one: `name`, then `stem copy.ext`, then `stem copy 2.ext` and on. What the engine's copy picks,
/// so a viewer can show the copy before it is made.
pub fn unique_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_stem_ext(name);
    let mut n = 0u32;
    loop {
        let candidate = match n {
            0 => name.to_string(),
            1 => format!("{stem} copy{ext}"),
            _ => format!("{stem} copy {n}{ext}"),
        };
        if !taken(&candidate) {
            return candidate;
        }
        n += 1;
        if n > 1000 {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            return format!("{stem} copy {stamp}{ext}");
        }
    }
}

/// A file of a project, as Quick Open lists it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFile {
    pub name: String,
    pub path: String,
    pub relative: String,
}

/// Bytes on the wire as one base64 string: a JSON array of numbers costs four times as much to
/// carry and far more to hold as a `serde_json::Value`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        serializer.serialize_str(&crate::b64::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let text = String::deserialize(deserializer)?;
        crate::b64::STANDARD
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

/// One file or folder of an [`ImportFiles`] item, by its path under the item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportEntry {
    /// `/`-separated under the item; empty for the item itself.
    pub path: String,
    /// A file's bytes; `None` for a folder.
    pub bytes: Option<Bytes>,
}

/// Copy a file or folder from the viewer's own disk into the folder `into`, named `name` or,
/// when that is taken, renamed as Finder does (as [`Destination::CopyInto`]): what Finder drops
/// into a project on another engine, whose disk the viewer cannot copy from. Each folder comes
/// before what it holds; a copy that fails partway leaves nothing behind. Answers where the item
/// landed and the listings that cover it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportFiles {
    pub into: String,
    pub name: String,
    pub entries: Vec<ImportEntry>,
}
files_call!(ImportFiles, agent: false, reply: NewPath);

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

/// Move the quick composer's staged files among `paths` into the attachments folder of the
/// session stored at `session`, which the launch just created. Answers `paths` with each moved
/// file at its new place; a path that is not one of `staged`, or lies outside the staging folder,
/// stays where it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptAttachments {
    pub session: i64,
    pub paths: Vec<String>,
    pub staged: Vec<String>,
}
files_call!(AdoptAttachments, agent: false, reply: Vec<String>);

/// Copy image files into a note's assets. Answers the images it kept, or the first failure when
/// it kept none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveNoteImages {
    pub note: String,
    pub paths: Vec<String>,
}
files_call!(SaveNoteImages, agent: false, reply: Vec<NoteImageAsset>);

#[cfg(test)]
mod name_tests {
    use super::*;

    #[test]
    fn split_stem_ext_keeps_dotfiles_whole() {
        assert_eq!(split_stem_ext("foo.ts"), ("foo", ".ts"));
        assert_eq!(split_stem_ext("foo.d.ts"), ("foo.d", ".ts"));
        assert_eq!(split_stem_ext(".gitignore"), (".gitignore", ""));
        assert_eq!(split_stem_ext("Makefile"), ("Makefile", ""));
    }
}
