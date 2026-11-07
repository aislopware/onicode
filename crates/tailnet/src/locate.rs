//! Where this machine's Tailscale `LocalAPI` listens.
//!
//! Tailscale runs three ways on a Mac (<https://tailscale.com/kb/1065/macos-variants>), and each says
//! where its `LocalAPI` is differently (`safesocket/safesocket_darwin.go` in tailscale/tailscale):
//!
//! * **App Store** (a sandboxed network extension, `IPNExtension`, run as the user) listens on a
//!   loopback TCP port and names the port and its token only in the name of a file it keeps open,
//!   `…/.tailscale.ipn.macos/sameuserproof-<port>-<token>`.
//! * **Standalone** (a system extension, run as root) points `/Library/Tailscale/ipnport` at its
//!   port and writes the token into `/Library/Tailscale/sameuserproof-<port>`, readable by the
//!   `admin` group only.
//! * **Open source** `tailscaled` listens on the unix socket `/var/run/tailscaled.socket`, where
//!   reading needs no token.
//!
//! They are tried in that order, as Tailscale's own CLI does.
//!
//! Ported from slopty's `slopty-tailnet` `locate.rs` (MIT).

#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

/// Where a `LocalAPI` listens, and what it asks of a caller.
#[derive(Clone, PartialEq, Eq)]
pub enum Location {
    /// Loopback TCP, with the token sent as the Basic-auth password.
    Tcp {
        /// The loopback port.
        port: u16,
        /// The same-user proof.
        token: String,
    },
    /// A unix socket that trusts whoever can open it.
    Unix(PathBuf),
}

impl std::fmt::Debug for Location {
    // The token is a credential: it stays out of logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp { port, .. } => write!(f, "Tcp {{ port: {port} }}"),
            Self::Unix(path) => f.debug_tuple("Unix").field(path).finish(),
        }
    }
}

/// The App Store variant's network extension.
#[cfg(target_os = "macos")]
const APP_STORE_EXTENSION: &str = "IPNExtension";
/// The standalone variant's files.
#[cfg(target_os = "macos")]
const STANDALONE_DIR: &str = "/Library/Tailscale";
/// The open-source daemon's socket.
#[cfg(target_os = "macos")]
const DAEMON_SOCKET: &str = "/var/run/tailscaled.socket";

/// This machine's `LocalAPI`, or `None` when no Tailscale this process can read is running.
#[cfg(target_os = "macos")]
#[must_use]
pub fn find() -> Option<Location> {
    app_store()
        .or_else(|| standalone(Path::new(STANDALONE_DIR)))
        .or_else(daemon)
}

/// Always `None`: no third-party iOS app can reach the Tailscale app's `LocalAPI`.
#[cfg(target_os = "ios")]
#[must_use]
pub const fn find() -> Option<Location> {
    None
}

#[cfg(target_os = "macos")]
fn app_store() -> Option<Location> {
    crate::proc_files::open_by(APP_STORE_EXTENSION)
        .iter()
        .find_map(|p| proof_name(p))
}

/// `sameuserproof-<port>-<token>`, the App Store extension's open file.
#[cfg(target_os = "macos")]
fn proof_name(path: &Path) -> Option<Location> {
    if !path.parent()?.to_str()?.ends_with(".tailscale.ipn.macos") {
        return None;
    }
    let rest = path.file_name()?.to_str()?.strip_prefix("sameuserproof-")?;
    let (port, token) = rest.split_once('-')?;
    let port = port.parse().ok()?;
    (!token.is_empty()).then(|| Location::Tcp {
        port,
        token: token.to_owned(),
    })
}

/// `ipnport` → the port; `sameuserproof-<port>` → the token. A user outside `admin` cannot
/// read the token, and gets `None`.
#[cfg(target_os = "macos")]
fn standalone(dir: &Path) -> Option<Location> {
    let port = std::fs::read_link(dir.join("ipnport")).ok()?;
    let port: u16 = port.to_str()?.parse().ok()?;
    let token = std::fs::read_to_string(dir.join(format!("sameuserproof-{port}"))).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| Location::Tcp {
        port,
        token: token.to_owned(),
    })
}

#[cfg(target_os = "macos")]
fn daemon() -> Option<Location> {
    let socket = Path::new(DAEMON_SOCKET);
    socket
        .exists()
        .then(|| Location::Unix(socket.to_path_buf()))
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    /// The App Store extension's file names its port and token; any other file is ignored.
    #[test]
    fn the_app_store_proof_names_its_port_and_token() {
        let at = |p: &str| proof_name(Path::new(p));
        let dir = "/Users/me/Library/Group Containers/io.tailscale.ipn.macos/.tailscale.ipn.macos";
        assert_eq!(
            at(&format!("{dir}/sameuserproof-49177-9f86d081")),
            Some(Location::Tcp {
                port: 49177,
                token: "9f86d081".into()
            })
        );
        assert_eq!(at(&format!("{dir}/sameuserproof-49177-")), None, "no token");
        assert_eq!(at(&format!("{dir}/sameuserproof-x-9f86")), None, "no port");
        assert_eq!(at(&format!("{dir}/ipn.log")), None, "another file");
        assert_eq!(
            at("/tmp/sameuserproof-49177-9f86d081"),
            None,
            "outside the container"
        );
    }

    /// The standalone variant: the port from the link, the token from the file, trimmed; a
    /// missing or unreadable token finds nothing.
    #[test]
    fn the_standalone_files_name_the_port_and_the_token() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("49177", dir.path().join("ipnport")).unwrap();
        assert_eq!(standalone(dir.path()), None, "no token file");
        std::fs::write(dir.path().join("sameuserproof-49177"), "9f86d081\n").unwrap();
        assert_eq!(
            standalone(dir.path()),
            Some(Location::Tcp {
                port: 49177,
                token: "9f86d081".into()
            })
        );
    }

    /// The token never reaches a log line.
    #[test]
    fn a_location_does_not_print_its_token() {
        let at = Location::Tcp {
            port: 49177,
            token: "9f86d081".into(),
        };
        assert_eq!(format!("{at:?}"), "Tcp { port: 49177 }");
    }
}
