//! A process as the kernel counts it: retired instructions, footprint, descriptors, threads.
//!
//! Everything here reads `libproc` for a pid of the same user, which needs no root and no
//! entitlement.
//!
//! - Instructions and cycles (`ri_instructions`, `ri_cycles` of `rusage_info_v4`) are the
//!   process's, all threads together. On a loaded machine they move by well under a percent where
//!   wall time moves by tens, so they are what a measurement's budget holds.
//! - The footprint (`ri_phys_footprint`) is the dirty memory Activity Monitor and jetsam count, not
//!   the resident set; `ri_lifetime_max_phys_footprint` is its peak.
//! - Energy (`ri_energy_nj`, `rusage_info_v6`) is what the process's threads spent on the CPU, not
//!   the GPU's or the media engines' (those take `powermetrics`, which needs root). Wakeups are the
//!   package-idle and interrupt wakeups (`ri_pkg_idle_wkups`, `ri_interrupt_wkups`) Activity
//!   Monitor's "Idle Wake Ups" counts: what an app at rest costs a battery.

/// One reading of a process's counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// Instructions retired since the process started.
    pub instructions: u64,
    /// Cycles since the process started.
    pub cycles: u64,
    /// Physical footprint, in bytes.
    pub footprint: u64,
    /// The largest footprint the process has had, in bytes.
    pub peak_footprint: u64,
    /// Energy its threads spent on the CPU since it started, in nanojoules.
    pub energy_nj: u64,
    /// Times it woke the CPU from idle or was woken by an interrupt since it started.
    pub wakeups: u64,
}

/// The counters of `pid`; `None` when it is gone or not ours to read.
#[must_use]
pub fn usage(pid: i32) -> Option<Usage> {
    imp::usage(pid)
}

/// The counters of this process.
#[must_use]
pub fn own() -> Option<Usage> {
    usage(i32::try_from(std::process::id()).ok()?)
}

/// Instructions this process has retired, all threads together.
#[must_use]
pub fn instructions() -> Option<u64> {
    own().map(|u| u.instructions)
}

/// The descriptors `pid` has open.
#[must_use]
pub fn open_fds(pid: i32) -> Option<u32> {
    imp::open_fds(pid)
}

/// The threads `pid` runs.
#[must_use]
pub fn threads(pid: i32) -> Option<u32> {
    imp::threads(pid)
}

mod imp {
    use std::mem::MaybeUninit;

    use libc::{
        PROC_PIDLISTFDS, PROC_PIDTASKINFO, c_int, c_void, proc_fdinfo, proc_pid_rusage,
        proc_pidinfo, proc_taskinfo,
    };

    use super::Usage;

    /// `struct rusage_info_v6` (`<sys/resource.h>`), which the `libc` crate does not declare:
    /// a UUID, then 56 `uint64_t` counters in the header's order.
    #[repr(C)]
    struct RusageV6 {
        uuid: [u8; 16],
        counters: [u64; 56],
    }

    /// `RUSAGE_INFO_V6` (`<sys/resource.h>`).
    const RUSAGE_INFO_V6: c_int = 6;
    /// The counters' places in [`RusageV6`], as `<sys/resource.h>` orders the fields.
    const PKG_IDLE_WKUPS: usize = 2;
    const INTERRUPT_WKUPS: usize = 3;
    const PHYS_FOOTPRINT: usize = 7;
    const LIFETIME_MAX_PHYS_FOOTPRINT: usize = 28;
    const INSTRUCTIONS: usize = 29;
    const CYCLES: usize = 30;
    const ENERGY_NJ: usize = 40;

    pub(super) fn usage(pid: i32) -> Option<Usage> {
        let mut info = MaybeUninit::<RusageV6>::zeroed();
        // SAFETY: `proc_pid_rusage` (libproc.h) writes one `rusage_info_v6` for the flavor
        // `RUSAGE_INFO_V6` into the buffer it is given, whose layout `RusageV6` mirrors. The
        // SDK declares the parameter `rusage_info_t *`, a pointer to the struct passed as that.
        let done = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V6, info.as_mut_ptr().cast()) };
        if done != 0 {
            return None;
        }
        // SAFETY: zeroed is a valid `RusageV6` (plain integers), and the call succeeded.
        let info = unsafe { info.assume_init() };
        let at = |i: usize| info.counters.get(i).copied().unwrap_or_default();
        Some(Usage {
            instructions: at(INSTRUCTIONS),
            cycles: at(CYCLES),
            footprint: at(PHYS_FOOTPRINT),
            peak_footprint: at(LIFETIME_MAX_PHYS_FOOTPRINT),
            energy_nj: at(ENERGY_NJ),
            wakeups: at(PKG_IDLE_WKUPS).saturating_add(at(INTERRUPT_WKUPS)),
        })
    }

    /// `proc_pidinfo(PROC_PIDLISTFDS)` answers a null buffer with the size of the descriptor
    /// table, not the count open; so it is asked with a buffer, grown until the list fits, and
    /// the count is what it wrote.
    pub(super) fn open_fds(pid: i32) -> Option<u32> {
        let entry = size_of::<proc_fdinfo>();
        let mut room = 256_usize;
        loop {
            let mut buffer: Vec<MaybeUninit<proc_fdinfo>> = Vec::with_capacity(room);
            let bytes = c_int::try_from(room.checked_mul(entry)?).ok()?;
            // SAFETY: the buffer holds `room` entries of `proc_fdinfo`, `bytes` long, which is
            // what `proc_pidinfo` (libproc.h) may write for `PROC_PIDLISTFDS`; it writes whole
            // entries and returns the bytes written.
            let wrote = unsafe {
                proc_pidinfo(
                    pid,
                    PROC_PIDLISTFDS,
                    0,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    bytes,
                )
            };
            let wrote = usize::try_from(wrote).ok().filter(|&w| w > 0)?;
            let count = wrote.checked_div(entry)?;
            if count < room {
                return u32::try_from(count).ok();
            }
            room = room.checked_mul(4)?;
        }
    }

    pub(super) fn threads(pid: i32) -> Option<u32> {
        let mut info = MaybeUninit::<proc_taskinfo>::zeroed();
        let bytes = c_int::try_from(size_of::<proc_taskinfo>()).ok()?;
        // SAFETY: the buffer is one `proc_taskinfo`, `bytes` long, which is what
        // `proc_pidinfo` (libproc.h) writes for `PROC_PIDTASKINFO`.
        let wrote = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTASKINFO,
                0,
                info.as_mut_ptr().cast::<c_void>(),
                bytes,
            )
        };
        if wrote != bytes {
            return None;
        }
        // SAFETY: zeroed is a valid `proc_taskinfo` (plain integers), and the call wrote it all.
        let info = unsafe { info.assume_init() };
        u32::try_from(info.pti_threadnum).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_reads_back() {
        let me = i32::try_from(std::process::id()).unwrap();
        let before = own().unwrap();
        let mut x = 0_u64;
        for i in 0..1_000_000_u64 {
            x = std::hint::black_box(x.wrapping_add(i));
        }
        let after = own().unwrap();
        assert!(
            after.footprint > 0 && after.peak_footprint >= after.footprint,
            "{after:?}"
        );
        if after.instructions == 0 {
            // A virtual Mac (the office CI runners) counts no retired instructions.
            crate::live::skip("this Mac counts no instructions (a virtual machine)");
            return;
        }
        assert!(
            after.instructions.saturating_sub(before.instructions) > 1_000_000,
            "a million additions retire at least a million instructions: {before:?} {after:?}"
        );
        assert!(after.cycles > before.cycles, "and cycles: {after:?}");
        // The kernel bills energy in steps, not per instruction: a million additions on a busy
        // Mac can end inside one (land run of 2026-10-05). More of them always cross the next.
        let mut spent = after;
        for _ in 0..1_000 {
            if spent.energy_nj > before.energy_nj {
                break;
            }
            for i in 0..1_000_000_u64 {
                x = std::hint::black_box(x.wrapping_add(i));
            }
            spent = own().unwrap();
        }
        assert!(
            spent.energy_nj > before.energy_nj,
            "the additions cost energy: {before:?} {spent:?}"
        );
        let fds = open_fds(me).unwrap();
        let file = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(
            open_fds(me).unwrap(),
            fds + 1,
            "one more file, one more descriptor"
        );
        drop(file);
        assert!(threads(me).unwrap() >= 1, "at least the one running this");
    }

    #[test]
    fn a_gone_process_reads_none() {
        assert_eq!(usage(-1), None, "no such pid");
        assert_eq!(open_fds(-1), None, "no such pid");
        assert_eq!(threads(-1), None, "no such pid");
    }
}
