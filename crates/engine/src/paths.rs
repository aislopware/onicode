//! Browsing every path on this device, not only a chat's folder
//! (`docs/onicode/decisions/one-machine.md`). Admission is the tailnet's whois, as for every
//! peer call; the workspace file RPCs beside these stay scoped to a chat.
//!
//! - `ListPath {path, showHidden}` → `PathListing`: folders first, then the rest, by name.
//! - `PathRoots {}` → `{home, volumes, recent}`.
//! - `PathMutate {op: mkdir|rename|trash, path, to?}` → `{}`.

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::{fs, io};

use onicode_proto::files::{
    ListPathRequest, PathEntry, PathKind, PathListing, PathMutateRequest, PathOp, PathRoots,
    PathVolume,
};
use onicode_rpc::{RpcError, RpcReply, methods, parse_params};

use crate::repos::Repos;

/// Folder extensions Finder shows as one item.
const PACKAGES: &[&str] = &[
    "app",
    "appex",
    "bundle",
    "component",
    "dsym",
    "docarchive",
    "framework",
    "kext",
    "mdimporter",
    "mlmodelc",
    "photoslibrary",
    "playground",
    "plugin",
    "prefpane",
    "qlgenerator",
    "rtfd",
    "saver",
    "scptd",
    "sparsebundle",
    "xcarchive",
    "xcodeproj",
    "xcworkspace",
    "xpc",
];

/// The methods [`dispatch`] answers.
pub(crate) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::LIST_PATH | methods::PATH_ROOTS | methods::PATH_MUTATE
    )
}

/// Answers one of the methods [`handles`] names.
pub(crate) async fn dispatch(
    repos: &Repos,
    method: &str,
    params: serde_json::Value,
) -> Result<RpcReply, RpcError> {
    match method {
        methods::LIST_PATH => {
            let request: ListPathRequest = parse_params(params)?;
            let path = expand(&request.path)?;
            let listing = run_blocking(move || list(&path, request.show_hidden)).await?;
            RpcReply::value(&listing)
        }
        methods::PATH_ROOTS => {
            let mut recent: Vec<String> = Vec::new();
            for repo in repos.list().await {
                if !recent.contains(&repo.path) {
                    recent.push(repo.path);
                }
            }
            let volumes = repos
                .list_drives()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|drive| PathVolume {
                    name: drive.name,
                    path: drive.path,
                })
                .collect();
            RpcReply::value(&PathRoots {
                home: home().to_string_lossy().into_owned(),
                volumes,
                recent,
            })
        }
        methods::PATH_MUTATE => {
            let request: PathMutateRequest = parse_params(params)?;
            let path = expand(&request.path)?;
            let to = request.to.clone();
            run_blocking(move || mutate(request.op, &path, to.as_deref())).await?;
            RpcReply::value(&serde_json::json!({}))
        }
        other => Err(RpcError::UnknownMethod(other.to_owned())),
    }
}

async fn run_blocking<T, F>(work: F) -> Result<T, RpcError>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| RpcError::Failed(e.to_string()))?
        .map_err(|e| RpcError::Failed(e.to_string()))
}

fn home() -> PathBuf {
    crate::repos::session_home_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// `path` with a leading `~` expanded; an empty path is home.
fn expand(path: &str) -> Result<PathBuf, RpcError> {
    let expanded = if path.is_empty() || path == "~" {
        home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home().join(rest)
    } else {
        PathBuf::from(path)
    };
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Err(RpcError::BadParams(format!("{path} is not absolute")))
    }
}

fn is_package(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| PACKAGES.contains(&ext.to_string_lossy().to_ascii_lowercase().as_str()))
}

fn list(path: &Path, show_hidden: bool) -> io::Result<PathListing> {
    let mut entries = Vec::new();
    for child in fs::read_dir(path)?.flatten() {
        let name = child.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        // Gone since the listing, or unreadable: left out.
        let Ok(meta) = fs::symlink_metadata(child.path()) else {
            continue;
        };
        let file_type = meta.file_type();
        let (kind, target) = if file_type.is_symlink() {
            let target = fs::read_link(child.path())
                .ok()
                .map(|t| t.to_string_lossy().into_owned());
            (PathKind::Symlink, target)
        } else if file_type.is_dir() {
            let kind = if is_package(&name) {
                PathKind::Package
            } else {
                PathKind::Dir
            };
            (kind, None)
        } else {
            (PathKind::File, None)
        };
        entries.push(PathEntry {
            size: if file_type.is_file() { meta.len() } else { 0 },
            modified_ms: onicode_transfer::manifest::mtime_ms(&meta),
            mode: meta.mode() & 0o7777,
            name,
            kind,
            target,
        });
    }
    entries.sort_by(|a, b| {
        (a.kind != PathKind::Dir)
            .cmp(&(b.kind != PathKind::Dir))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(PathListing {
        path: path.to_string_lossy().into_owned(),
        entries,
        parent: path.parent().map(|p| p.to_string_lossy().into_owned()),
    })
}

fn mutate(op: PathOp, path: &Path, to: Option<&str>) -> io::Result<()> {
    match op {
        PathOp::Mkdir => fs::create_dir(path),
        PathOp::Rename => {
            let to = to
                .filter(|t| !t.is_empty())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "rename needs `to`"))?;
            let target = if to.contains('/') {
                expand(to)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?
            } else {
                path.parent()
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "nothing to rename")
                    })?
                    .join(to)
            };
            if fs::symlink_metadata(&target).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is taken", target.display()),
                ));
            }
            fs::rename(path, target)
        }
        PathOp::Trash => onicode_transfer::fsops::trash(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_puts_folders_first_and_names_packages_and_links() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("b-folder")).unwrap();
        fs::create_dir(dir.path().join("A.app")).unwrap();
        fs::write(dir.path().join("a.txt"), b"hi").unwrap();
        fs::write(dir.path().join(".hidden"), b"").unwrap();
        std::os::unix::fs::symlink("a.txt", dir.path().join("link")).unwrap();
        let listing = list(dir.path(), false).unwrap();
        let names: Vec<_> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["b-folder", "A.app", "a.txt", "link"]);
        assert_eq!(listing.entries[1].kind, PathKind::Package);
        assert_eq!(listing.entries[2].size, 2);
        assert_eq!(listing.entries[3].target.as_deref(), Some("a.txt"));
        assert_eq!(list(dir.path(), true).unwrap().entries.len(), 5);
    }

    #[test]
    fn mkdir_and_rename_refuse_to_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        mutate(PathOp::Mkdir, &a, None).unwrap();
        assert!(mutate(PathOp::Mkdir, &a, None).is_err());
        fs::create_dir(dir.path().join("b")).unwrap();
        assert!(mutate(PathOp::Rename, &a, Some("b")).is_err(), "b is taken");
        mutate(PathOp::Rename, &a, Some("c")).unwrap();
        assert!(dir.path().join("c").is_dir());
        assert!(expand("relative").is_err());
        assert_eq!(expand("~").unwrap(), home());
    }
}
