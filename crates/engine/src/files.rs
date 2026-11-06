//! Files (`docs/onicode/decisions/engine.md`, migration step 5): directory watches over the
//! engine's file watch ([`FilesHub`], the protocol's `WatchDir`), and the calls that read, write
//! and search the engine's disk, served as the Files group by [`FilesService`].
mod hub;
mod service;
#[cfg(test)]
mod tests;

pub use hub::FilesHub;
pub use service::FilesService;

/// The FNV-1a hash of `bytes`: what a viewer holds of a file to poll it ([`Read::unless`]) and to
/// save over it ([`Write::expected`]). Stable across engine runs, so an editor open across an
/// engine update still saves.
///
/// [`Read::unless`]: onicode_protocol::files::Read::unless
/// [`Write::expected`]: onicode_protocol::files::Write::expected
#[must_use]
pub fn digest(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}
