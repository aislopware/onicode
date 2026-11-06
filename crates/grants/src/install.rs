//! Where the app runs from. A grant belongs to the copy that asked, so the checklist refuses to
//! run from a disk image or a translocated copy: the grants would land on a copy that goes away.
//! A disk image mounts read-only under `/Volumes`; a writable drive there (an external disk the
//! owner builds on) is a lasting place.

use std::path::{Component, Path};

/// Where the app bundle sits, as the checklist judges it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Install {
    /// Somewhere that lasts: the checklist runs.
    Settled,
    /// On a read-only volume under `/Volumes`: the disk image it came in.
    Volume,
    /// Gatekeeper's randomised read-only copy (`/private/var/folders/…/AppTranslocation/…`).
    Translocated,
}

impl Install {
    /// Judges the app bundle at `app`; `writable` is whether its folder can be written, as a
    /// disk image's cannot.
    #[must_use]
    pub fn of(app: &Path, writable: bool) -> Self {
        let mut parts = app.components().filter_map(|part| match part {
            Component::Normal(name) => Some(name),
            _ => None,
        });
        let first = parts.next();
        if first.is_some_and(|name| name == "Volumes") && !writable {
            return Self::Volume;
        }
        let rest: Vec<_> = parts.collect();
        let in_folders = match first {
            Some(name) if name == "private" => {
                rest.first().is_some_and(|name| *name == "var")
                    && rest.get(1).is_some_and(|name| *name == "folders")
            }
            Some(name) if name == "var" => rest.first().is_some_and(|name| *name == "folders"),
            _ => false,
        };
        if in_folders && rest.iter().any(|name| *name == "AppTranslocation") {
            return Self::Translocated;
        }
        Self::Settled
    }

    /// Whether the checklist may run.
    #[must_use]
    pub const fn settled(self) -> bool {
        matches!(self, Self::Settled)
    }

    /// What the checklist says instead of its rows.
    #[must_use]
    pub const fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Settled => None,
            Self::Volume | Self::Translocated => Some("Move Onicode to Applications first"),
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn sample_paths() {
        let cases = [
            ("/Applications/Onicode.app", true, Install::Settled),
            ("/Users/me/Applications/Onicode.app", true, Install::Settled),
            ("/Volumes/Onicode/Onicode.app", false, Install::Volume),
            // A writable drive under /Volumes is a lasting place, as the owner's build disk is.
            (
                "/Volumes/Lacie/build/Onicode Dev.app",
                true,
                Install::Settled,
            ),
            (
                "/Volumes/Lacie/build/Onicode Dev.app",
                false,
                Install::Volume,
            ),
            (
                "/private/var/folders/xy/abc123/T/AppTranslocation/0F1E-22/d/Onicode.app",
                false,
                Install::Translocated,
            ),
            (
                "/var/folders/xy/abc123/T/AppTranslocation/0F1E-22/d/Onicode.app",
                false,
                Install::Translocated,
            ),
            // Only Gatekeeper's folder counts, not a folder of that name anywhere.
            (
                "/Users/me/AppTranslocation/Onicode.app",
                true,
                Install::Settled,
            ),
            (
                "/private/var/folders/xy/abc123/T/Onicode.app",
                true,
                Install::Settled,
            ),
            // A read-only folder elsewhere is not a disk image.
            ("/Applications/Onicode.app", false, Install::Settled),
        ];
        for (path, writable, want) in cases {
            let got = Install::of(Path::new(path), writable);
            assert_eq!(got, want, "{path} writable={writable}");
            assert_eq!(got.settled(), want == Install::Settled);
        }
        assert_eq!(
            Install::Translocated.refusal(),
            Some("Move Onicode to Applications first")
        );
    }
}
