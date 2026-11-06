//! Lexical navigation of absolute folders on another device, which `std::path`
//! would resolve against this machine instead.

/// Append a folder name.
pub fn child_folder(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Stop at the root, even across repeated Up operations.
pub fn parent_folder(path: &str) -> Option<String> {
    let path = path.trim_end_matches('/');
    match path.rfind('/')? {
        0 => Some("/".into()),
        index => Some(path[..index].into()),
    }
}

/// Expand `~` using the target device's home.
pub fn expand_home(path: &str, home: &str) -> Option<String> {
    if path == "~" {
        return Some(home.into());
    }
    let relative = path.strip_prefix("~/")?;
    Some(child_folder(home, relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_expansion_uses_the_device_home() {
        assert_eq!(
            expand_home("~/worktrees", "/home/ana"),
            Some("/home/ana/worktrees".into())
        );
        assert_eq!(expand_home("~", "/home/ana"), Some("/home/ana".into()));
        assert_eq!(expand_home(r"~\worktrees", "/home/ana"), None);
    }

    #[test]
    fn navigation_preserves_backslashes_and_case() {
        assert_eq!(
            child_folder(r"/home/ana/a\b", "Repo"),
            r"/home/ana/a\b/Repo"
        );
        assert_eq!(
            parent_folder(r"/home/ana/a\b/Repo"),
            Some(r"/home/ana/a\b".into())
        );
        assert_eq!(parent_folder("/home"), Some("/".into()));
        assert_eq!(parent_folder("/"), None);
    }
}
