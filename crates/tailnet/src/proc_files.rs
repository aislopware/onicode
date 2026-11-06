//! The files this user's other processes hold open, read through `libproc` as `lsof` does.
//!
//! The App Store Tailscale's network extension says where its local API listens only by the
//! name of a file it keeps open (`crate::locate`).
//!
//! Ported from slopty's `slopty-platform` `proc_files.rs` (MIT).

use std::ffi::{CStr, c_int, c_void};
use std::path::PathBuf;

/// `<sys/proc_info.h>`: `proc_listpids` type selecting the processes of one user id.
const PROC_UID_ONLY: u32 = 4;
/// `<sys/proc_info.h>`: `proc_pidfdinfo` flavor filling a `vnode_fdinfowithpath`.
const PROC_PIDFDVNODEPATHINFO: c_int = 2;

/// `<sys/proc_info.h>`: `struct proc_fileinfo`, which `libc` does not declare.
#[repr(C)]
#[derive(Clone, Copy)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: libc::off_t,
    fi_type: i32,
    fi_guardflags: u32,
}

/// `<sys/proc_info.h>`: `struct vnode_fdinfowithpath`.
#[repr(C)]
#[derive(Clone, Copy)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

/// The paths of the regular files the processes named `name` hold open, for this user's
/// processes only. A process that exits between the listing and the reading is skipped.
#[must_use]
pub(crate) fn open_by(name: &str) -> Vec<PathBuf> {
    pids_of_this_user()
        .into_iter()
        .filter(|&pid| process_name(pid).as_deref() == Some(name))
        .flat_map(open_paths)
        .collect()
}

/// A libproc listing of `T`s: asked once with no buffer for its size in bytes, then filled
/// with `spare` records of room for what appeared in between.
fn listing<T: Copy>(zero: T, spare: usize, fill: impl Fn(*mut c_void, c_int) -> c_int) -> Vec<T> {
    let Ok(bytes) = usize::try_from(fill(std::ptr::null_mut(), 0)) else {
        return Vec::new();
    };
    let len = bytes
        .checked_div(size_of::<T>())
        .unwrap_or(0)
        .saturating_add(spare);
    let mut items = vec![zero; len];
    let room = items
        .len()
        .checked_mul(size_of::<T>())
        .and_then(|room| c_int::try_from(room).ok())
        .unwrap_or(c_int::MAX);
    let filled = usize::try_from(fill(items.as_mut_ptr().cast::<c_void>(), room)).unwrap_or(0);
    items.truncate(filled.checked_div(size_of::<T>()).unwrap_or(0));
    items
}

fn pids_of_this_user() -> Vec<c_int> {
    // SAFETY: `getuid` has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut pids = listing(0, 64, |buffer, room| {
        // SAFETY: libproc.h: with a null buffer returns the bytes the listing needs; otherwise
        // fills at most `room` bytes of `buffer` and returns the bytes filled.
        unsafe { libc::proc_listpids(PROC_UID_ONLY, uid, buffer, room) }
    });
    pids.retain(|&pid| pid > 0);
    pids
}

fn process_name(pid: c_int) -> Option<String> {
    let mut name = [0_u8; 2 * libc::MAXCOMLEN + 1];
    let room = u32::try_from(name.len()).ok()?;
    // SAFETY: libproc.h: writes a NUL-terminated name of at most `room` bytes.
    let len = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast::<c_void>(), room) };
    let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
    String::from_utf8(name.get(..len)?.to_vec()).ok()
}

fn open_paths(pid: c_int) -> Vec<PathBuf> {
    let none = libc::proc_fdinfo {
        proc_fd: 0,
        proc_fdtype: 0,
    };
    let fds = listing(none, 16, |buffer, room| {
        // SAFETY: libproc.h: as `proc_listpids`, for this process's `proc_fdinfo` records.
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, buffer, room) }
    });
    fds.iter()
        .filter(|fd| {
            i32::try_from(fd.proc_fdtype).is_ok_and(|kind| kind == libc::PROX_FDTYPE_VNODE)
        })
        .filter_map(|fd| vnode_path(pid, fd.proc_fd))
        .collect()
}

fn vnode_path(pid: c_int, fd: i32) -> Option<PathBuf> {
    let mut info = std::mem::MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
    let size = c_int::try_from(size_of::<VnodeFdInfoWithPath>()).ok()?;
    // SAFETY: libproc.h: with PROC_PIDFDVNODEPATHINFO the kernel fills a whole
    // `vnode_fdinfowithpath` and returns its size, or less on failure.
    let filled = unsafe {
        libc::proc_pidfdinfo(
            pid,
            fd,
            PROC_PIDFDVNODEPATHINFO,
            info.as_mut_ptr().cast::<c_void>(),
            size,
        )
    };
    if filled != size {
        return None;
    }
    // SAFETY: the kernel filled every byte (checked above), and zeroed memory is a valid value
    // of these plain C structs anyway.
    let info = unsafe { info.assume_init() };
    let path = CStr::from_bytes_until_nul(path_bytes(&info.pvip.vip_path)).ok()?;
    Some(PathBuf::from(path.to_str().ok()?))
}

/// `vip_path` as the bytes of its C string (libc splits its `MAXPATHLEN` bytes into 32 rows).
const fn path_bytes(path: &[[libc::c_char; 32]; 32]) -> &[u8] {
    // SAFETY: `[[c_char; 32]; 32]` is 1024 contiguous single-byte values with no padding, and
    // `c_char` has the size and alignment of `u8`.
    unsafe { std::slice::from_raw_parts(path.as_ptr().cast::<u8>(), 32 * 32) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test holds a file open; reading its own process's descriptors finds it by name.
    #[test]
    fn a_process_names_the_files_it_holds_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sameuserproof-49177-abc");
        let _held = std::fs::File::create(&path).unwrap();
        let me = c_int::try_from(std::process::id()).unwrap();
        let name = process_name(me).expect("our own name");
        let found = open_by(&name);
        let canonical = path.canonicalize().unwrap();
        assert!(found.contains(&canonical), "{canonical:?} among {found:?}");
    }

    /// A name no process has finds nothing, rather than failing.
    #[test]
    fn a_process_that_is_not_running_holds_nothing() {
        assert_eq!(open_by("onicode-no-such-process"), Vec::<PathBuf>::new());
    }
}
