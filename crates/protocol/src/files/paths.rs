//! Display-path helpers ported from paths.ts and fileTree.ts. Paths are POSIX (macOS and iOS
//! only, `docs/onicode/decisions/platform.md`): `/` separates, and a backslash is part of a name.
use super::name::path_segments;

fn trim_slash(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        trimmed.into()
    }
}

/// The key two spellings of one path compare by: the path without its trailing slashes.
pub fn path_key(path: &str) -> String {
    trim_slash(path)
}

/// Text after the first `chars` characters.
fn skip_chars(text: &str, chars: usize) -> &str {
    text.char_indices()
        .nth(chars)
        .map_or("", |(index, _)| &text[index..])
}

pub fn parent_path(path: &str) -> String {
    let trimmed = trim_slash(path);
    match trimmed.rfind('/').filter(|i| *i > 0) {
        Some(i) => trimmed[..i].to_string(),
        None => "/".into(),
    }
}

/// `path` with its `from` prefix replaced by `to`; unrelated paths are unchanged.
pub fn rebase_path(path: &str, from: &str, to: &str) -> String {
    let normalized = trim_slash(path);
    let source = trim_slash(from);
    let dest = trim_slash(to);
    let key = path_key(&normalized);
    let source_key = path_key(&source);
    if key == source_key {
        return dest;
    }
    if key.starts_with(&format!("{source_key}/")) {
        return format!("{dest}{}", skip_chars(&normalized, source.chars().count()));
    }
    path.to_string()
}

pub fn is_equal_or_inside(path: &str, root: &str) -> bool {
    let key = path_key(path);
    let base_key = path_key(root);
    key == base_key || key.starts_with(&format!("{base_key}/"))
}

pub fn join_path(parent: &str, relative: &str) -> String {
    let mut out = trim_slash(parent);
    let parts = relative
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".");
    for part in parts {
        if part == ".." {
            out = parent_path(&out);
            continue;
        }
        out = if out == "/" {
            format!("/{part}")
        } else {
            format!("{out}/{part}")
        };
    }
    out
}

/// Path relative to `cwd` when it lives under the project, otherwise unchanged.
pub fn display_path(path: &str, cwd: Option<&str>) -> String {
    let normalized = trim_slash(path);
    if let Some(base) = cwd.map(trim_slash).filter(|base| base != "~") {
        let key = path_key(&normalized);
        let base_key = path_key(&base);
        if key == base_key {
            return normalized
                .split('/')
                .rfind(|part| !part.is_empty())
                .map_or_else(|| normalized.clone(), str::to_string);
        }
        if key.starts_with(&format!("{base_key}/")) {
            return skip_chars(&normalized, base.chars().count() + 1).to_string();
        }
    }
    normalized
}

/// Folder to create into, given the explorer selection. `is_dir_at` reports
/// whether a path is known to be a folder.
pub fn create_parent_of(
    cwd: &str,
    at_path: Option<&str>,
    is_dir_at: impl Fn(&str) -> bool,
) -> String {
    match at_path {
        None => cwd.to_string(),
        Some(selected) if selected == cwd => cwd.to_string(),
        Some(selected) if is_dir_at(selected) => selected.to_string(),
        Some(selected) => parent_path(selected),
    }
}

/// Directories whose children change when creating `name` under `parent`.
pub fn dirs_touched_by_create(parent: &str, name: &str) -> Vec<String> {
    let segments = path_segments(name);
    let mut out = vec![parent.to_string()];
    let mut cur = parent.to_string();
    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        cur = join_path(&cur, segment);
        out.push(cur.clone());
    }
    out
}

/// Directories whose children change when moving `from` to `to`.
pub fn dirs_touched_by_move(from: &str, to: &str) -> Vec<String> {
    let from_parent = parent_path(from);
    let to_parent = parent_path(to);
    if from_parent == to_parent {
        vec![from_parent]
    } else {
        vec![from_parent, to_parent]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backslash_is_part_of_a_name() {
        assert_eq!(join_path("/tmp", "a\\b.txt"), "/tmp/a\\b.txt");
        assert_eq!(parent_path("/tmp/a\\b.txt"), "/tmp");
        assert!(!is_equal_or_inside("/tmp/a\\b", "/tmp/a"));
    }

    #[test]
    fn walks_unix_paths() {
        assert_eq!(parent_path("/a/b/"), "/a");
        assert_eq!(parent_path("/a"), "/");
        assert_eq!(parent_path("/"), "/");
        assert_eq!(join_path("/", "a/./b"), "/a/b");
        assert_eq!(join_path("/a/b", "../c"), "/a/c");
    }

    #[test]
    fn rebases_and_displays_unix_paths() {
        assert_eq!(
            rebase_path("/r/old/a.ts", "/r/old", "/r/new"),
            "/r/new/a.ts"
        );
        assert_eq!(rebase_path("/r/old", "/r/old/", "/r/new"), "/r/new");
        assert_eq!(rebase_path("/r/older/a", "/r/old", "/r/new"), "/r/older/a");
        assert_eq!(display_path("/repo/src/a.ts", Some("/repo")), "src/a.ts");
        assert_eq!(display_path("/repo/", Some("/repo")), "repo");
        assert_eq!(display_path("/other/a.ts", Some("/repo")), "/other/a.ts");
        assert_eq!(display_path("/repo/a.ts", Some("~")), "/repo/a.ts");
        assert_eq!(display_path("/repo/a.ts", None), "/repo/a.ts");
    }

    #[test]
    fn create_parent_follows_the_selection() {
        let dirs = |path: &str| path == "/repo/src";
        assert_eq!(create_parent_of("/repo", None, dirs), "/repo");
        assert_eq!(create_parent_of("/repo", Some("/repo"), dirs), "/repo");
        assert_eq!(
            create_parent_of("/repo", Some("/repo/src"), dirs),
            "/repo/src"
        );
        assert_eq!(
            create_parent_of("/repo", Some("/repo/src/a.ts"), dirs),
            "/repo/src"
        );
    }

    #[test]
    fn touched_dirs() {
        assert_eq!(dirs_touched_by_create("/repo", "a.ts"), ["/repo"]);
        assert_eq!(
            dirs_touched_by_create("/repo", "src/lib/a.ts"),
            ["/repo", "/repo/src", "/repo/src/lib"]
        );
        assert_eq!(dirs_touched_by_create("/repo", "src/"), ["/repo"]);
        assert_eq!(dirs_touched_by_move("/r/a/x", "/r/a/y"), ["/r/a"]);
        assert_eq!(dirs_touched_by_move("/r/a/x", "/r/b/x"), ["/r/a", "/r/b"]);
    }
}
