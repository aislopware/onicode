//! Trees to copy, and the check that two trees are the same.

#![allow(dead_code)]

use std::fs::{self, File};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use onicode_transfer::Tuning;

/// Bytes drawn from `seed`.
pub fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[3]
        })
        .collect()
}

/// Text that compresses well.
pub fn text(seed: u64, len: usize) -> Vec<u8> {
    let line = format!("line {seed} of a log that repeats itself\n");
    line.bytes().cycle().take(len).collect()
}

/// Numbers small enough that a megabyte is a big file.
pub fn small_tuning() -> Tuning {
    Tuning {
        block: 64 << 10,
        range: 64 << 10,
        small: 8 << 10,
        split: 256 << 10,
        files: 8,
        ranges: 4,
        pack_bytes: 64 << 10,
        pack_items: 64,
    }
}

fn set_mtime(path: &Path, seconds: u64) {
    let times =
        fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(seconds * 1000 + 123));
    File::open(path).unwrap().set_times(times).unwrap();
}

/// A tree under `root/tree`: a big file last, many small ones, a compressible one, an empty
/// folder, a symlink and a bundle, with modes and mtimes of their own. `small_files` small
/// files, the big one `big` bytes.
pub fn build_tree(root: &Path, seed: u64, small_files: usize, big: usize) -> std::path::PathBuf {
    let tree = root.join("tree");
    let contents = tree.join("App.app/Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::create_dir_all(tree.join("small")).unwrap();
    fs::create_dir_all(tree.join("empty")).unwrap();
    fs::write(contents.join("Info.plist"), b"<plist><dict/></plist>").unwrap();
    let app = contents.join("MacOS/App");
    fs::write(&app, noise(seed ^ 1, 3000)).unwrap();
    fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
    for i in 0..small_files {
        let len = (i * 37 + usize::try_from(seed % 7).unwrap()) % 3000;
        let path = tree.join(format!("small/f{i:04}"));
        fs::write(&path, noise(seed.wrapping_add(i as u64), len)).unwrap();
        if i % 5 == 0 {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        set_mtime(&path, 1_600_000_000 + i as u64);
    }
    fs::write(tree.join("notes.txt"), text(seed, 200 << 10)).unwrap();
    fs::write(tree.join("zz-big.bin"), noise(seed ^ 0xb16, big)).unwrap();
    std::os::unix::fs::symlink("notes.txt", tree.join("link")).unwrap();
    set_mtime(&tree.join("notes.txt"), 1_650_000_000);
    set_mtime(&tree.join("zz-big.bin"), 1_660_000_000);
    set_mtime(&app, 1_670_000_000);
    // Folders last, deepest first: writing into a folder moves its mtime.
    for (dir, at) in [
        (contents.join("MacOS"), 1_500_000_001),
        (contents.clone(), 1_500_000_002),
        (tree.join("App.app"), 1_500_000_003),
        (tree.join("small"), 1_500_000_004),
        (tree.join("empty"), 1_500_000_005),
        (tree.clone(), 1_500_000_006),
    ] {
        set_mtime(&dir, at);
    }
    fs::set_permissions(tree.join("empty"), fs::Permissions::from_mode(0o700)).unwrap();
    tree
}

fn millis(meta: &fs::Metadata) -> i64 {
    meta.mtime() * 1000 + meta.mtime_nsec() / 1_000_000
}

/// Panics unless `a` and `b` hold the same names, kinds, bytes, link targets, modes and
/// mtimes (to the millisecond; a link's own mode and mtime are not kept).
pub fn assert_same_tree(a: &Path, b: &Path) {
    let ma = fs::symlink_metadata(a).unwrap();
    let mb = fs::symlink_metadata(b).unwrap_or_else(|e| panic!("{} is missing: {e}", b.display()));
    assert_eq!(ma.file_type(), mb.file_type(), "{}", b.display());
    if ma.file_type().is_symlink() {
        assert_eq!(fs::read_link(a).unwrap(), fs::read_link(b).unwrap());
        return;
    }
    assert_eq!(
        ma.permissions().mode() & 0o7777,
        mb.permissions().mode() & 0o7777,
        "mode of {}",
        b.display()
    );
    assert_eq!(millis(&ma), millis(&mb), "mtime of {}", b.display());
    if ma.is_file() {
        let (x, y) = (fs::read(a).unwrap(), fs::read(b).unwrap());
        assert!(x == y, "bytes of {} differ", b.display());
        assert_eq!(blake3::hash(&x), blake3::hash(&y));
        return;
    }
    let names = |p: &Path| {
        let mut names: Vec<_> = fs::read_dir(p)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let (na, nb) = (names(a), names(b));
    assert_eq!(na, nb, "entries of {}", b.display());
    for name in na {
        assert_same_tree(&a.join(&name), &b.join(&name));
    }
}
