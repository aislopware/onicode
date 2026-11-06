//! The OS's network path, on Apple platforms through `NWPathMonitor` (Network.framework's
//! plain C surface and one block).
//!
//! Each report of a usable path calls back: an interface handover (Wi-Fi to a hotspot, a VPN up
//! or down) arrives as one, and a link that died with the old path is worth dialling at once on
//! the new one rather than after its backoff ([`crate::PeerLinks::follow_path`]). Elsewhere
//! nothing reports, and the links' own liveness and redial carry the recovery.

/// Calls `on_usable` on every report of a usable path, for the life of the process.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn watch(on_usable: Box<dyn Fn() + Send + Sync>) {
    apple::start(on_usable);
}

/// Calls `on_usable` on every report of a usable path: never, on a platform without a monitor.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub fn watch(on_usable: Box<dyn Fn() + Send + Sync>) {
    drop(on_usable);
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
mod apple {
    use std::ffi::{c_char, c_void};

    use block2::{Block, RcBlock};

    // nw_path_monitor_t, nw_path_t and dispatch_queue_t are ObjC objects; opaque pointers are
    // all this bridge needs.
    #[link(name = "Network", kind = "framework")]
    unsafe extern "C" {
        fn nw_path_monitor_create() -> *mut c_void;
        fn nw_path_monitor_set_update_handler(
            monitor: *mut c_void,
            handler: &Block<dyn Fn(*mut c_void)>,
        );
        fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: *mut c_void);
        fn nw_path_monitor_start(monitor: *mut c_void);
        fn nw_path_get_status(path: *mut c_void) -> i32;
    }

    unsafe extern "C" {
        fn dispatch_queue_create(label: *const c_char, attr: *mut c_void) -> *mut c_void;
    }

    /// `nw_path_status_unsatisfied`. Only a definite "no path" holds the callback back:
    /// `invalid` and `satisfiable` count as usable, so a confused monitor can only make the
    /// links dial too often, never go quiet.
    const NW_PATH_STATUS_UNSATISFIED: i32 = 2;

    pub(super) fn start(on_usable: Box<dyn Fn() + Send + Sync>) {
        // SAFETY: Network.framework's C surface, called as documented: the monitor and queue
        // are checked for null, the handler block is copied by the monitor before `start`, and
        // the monitor, its queue and the block live for the process.
        unsafe {
            let monitor = nw_path_monitor_create();
            if monitor.is_null() {
                tracing::warn!("NWPathMonitor unavailable; links recover on their own");
                return;
            }
            let handler = RcBlock::new(move |path: *mut c_void| {
                if nw_path_get_status(path) != NW_PATH_STATUS_UNSATISFIED {
                    on_usable();
                }
            });
            let queue = dispatch_queue_create(c"onicode.net-path".as_ptr(), std::ptr::null_mut());
            nw_path_monitor_set_queue(monitor, queue);
            nw_path_monitor_set_update_handler(monitor, &handler);
            nw_path_monitor_start(monitor);
            std::mem::forget(handler);
        }
    }
}
