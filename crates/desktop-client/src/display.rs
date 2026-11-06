//! A display a Mac makes for this device ([`crate::Open::Made`]): the device's key, kept in its
//! data directory, and the shape asked for.
//!
//! The view offers it sized to its own drawable at the backing scale and refresh of the screen
//! it is on, so the Mac lays out its desktop for this screen instead of scaling its own down.
//!
//! Ported from slopty's client `screen/display.rs` (MIT, same authors).

use std::path::Path;

use onicode_desktop_proto::screen::{DisplayKey, DisplayShape};

/// The file in the app's data directory that holds this device's key.
pub const KEY_FILE: &str = "display-key";

/// This device's display key, from `data_dir`, drawn and written there when it holds none.
///
/// The same key makes a Mac give this device the same display identity every time, so macOS
/// puts its windows back where they were.
///
/// # Errors
///
/// When a new key cannot be written.
pub fn key(data_dir: &Path) -> std::io::Result<DisplayKey> {
    let path = data_dir.join(KEY_FILE);
    if let Some(key) = std::fs::read_to_string(&path)
        .ok()
        .as_deref()
        .and_then(parse)
    {
        return Ok(key);
    }
    let key = DisplayKey(uuid::Uuid::new_v4().into_bytes());
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(
        &path,
        format!("{}\n", uuid::Uuid::from_bytes(key.0).simple()),
    )?;
    Ok(key)
}

fn parse(text: &str) -> Option<DisplayKey> {
    uuid::Uuid::try_parse(text.trim())
        .ok()
        .map(|uuid| DisplayKey(uuid.into_bytes()))
}

/// The shape of a display for a view `pixels` in size on a screen of `scale` and `refresh_hz`
/// (0 when not known).
#[must_use]
pub const fn shape(pixels: (u32, u32), scale: f32, refresh_hz: u16) -> DisplayShape {
    DisplayShape {
        width: pixels.0,
        height: pixels.1,
        scale,
        refresh_hz,
    }
}

#[cfg(test)]
mod tests {
    use super::{KEY_FILE, key, shape};

    /// The key is drawn once per data directory and read back after; another directory (another
    /// device) draws its own, and a file that holds no key is replaced.
    #[test]
    fn the_key_is_drawn_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let first = key(dir.path()).unwrap();
        assert_eq!(key(dir.path()).unwrap(), first, "kept");
        let other = tempfile::tempdir().unwrap();
        assert_ne!(
            key(other.path()).unwrap(),
            first,
            "another device, another key"
        );
        std::fs::write(dir.path().join(KEY_FILE), "not a key").unwrap();
        let redrawn = key(dir.path()).unwrap();
        assert_ne!(redrawn, first);
        assert_eq!(key(dir.path()).unwrap(), redrawn);
    }

    /// A key directory that cannot be written is an error, not a key that changes every launch.
    #[test]
    fn an_unwritable_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a file");
        std::fs::write(&file, "").unwrap();
        key(&file).unwrap_err();
    }

    #[test]
    fn a_shape_is_what_the_view_is() {
        let s = shape((2360, 1640), 2.0, 120);
        assert_eq!((s.width, s.height, s.refresh_hz), (2360, 1640, 120));
        assert!((s.scale - 2.0).abs() < f32::EPSILON);
    }
}
