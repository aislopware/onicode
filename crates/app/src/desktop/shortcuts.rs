//! The Mac's shortcuts while its window is key and holds the controls: ⌘Tab, ⌘Space, ⌘`,
//! Mission Control, ⌃ and the arrows, ⌘H and ⌘M reach the Mac instead of this one, as Jump
//! Desktop does by default for a Mac host. ⌘⌥⎋ and ⌃⌘Q stay here: they are the ways out.
//!
//! Two pieces take them. The window server handles the system's shortcuts before any app sees
//! them; while the grab holds, this app turns them off for itself (HIToolbox's
//! `PushSymbolicHotKeyMode`, which macOS honours only for a process trusted for Accessibility),
//! so they arrive as ordinary key events. A local event monitor then takes those keys off
//! `-[NSApplication sendEvent:]`, ahead of the menu's key equivalents (⌘H would hide this app),
//! and queues them for the view ([`Taken`]). Everything else goes the way it always did: GPUI's
//! key handlers send it, and the app's own shortcuts stay its own. The decision is pure
//! ([`Taking`]); the platform is a seam ([`Shortcuts`]) the tests drive without AppKit.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use onicode_desktop_proto::input::Mods;

use super::keys::{KeyKind, NativeKey};

/// `kVK_Tab`, `kVK_Space`, `kVK_ANSI_Grave`, `kVK_ANSI_H`, `kVK_ANSI_M`, `kVK_F3` and the arrows
/// (`<HIToolbox/Events.h>`).
const TAB: u16 = 0x30;
const SPACE: u16 = 0x31;
const GRAVE: u16 = 0x32;
const H: u16 = 0x04;
const M: u16 = 0x2e;
const F3: u16 = 0x63;
const ARROWS: [u16; 4] = [0x7b, 0x7c, 0x7d, 0x7e];

/// What an Apple keyboard's Mission Control key (F3 in its media row) reports as its key code.
/// No position has it, so the view sends ⌃↑, Mission Control's own shortcut on the Mac.
pub(crate) const MISSION_CONTROL: u16 = 0xa0;

/// Whether `vk` held with `mods` is one of the system's shortcuts the Mac gets.
#[must_use]
pub(crate) fn is_system_shortcut(vk: u16, mods: Mods) -> bool {
    let command = mods.contains(Mods::SUPER);
    let control = mods.contains(Mods::CTRL);
    match vk {
        // ⌘Tab and ⌘⇧Tab, the app switcher; ⌘` between an app's windows; ⌘H and ⌘⌥H, hide;
        // ⌘M, minimise.
        TAB | GRAVE | H | M => command && !control,
        // ⌘Space, Spotlight; ⌃Space, the next input source; ⌘⌥Space, Finder's search.
        SPACE => command || control,
        // ⌃ and an arrow: Mission Control, App Exposé, and the Spaces either side.
        _ if ARROWS.contains(&vk) => control && !command,
        F3 | MISSION_CONTROL => true,
        _ => false,
    }
}

/// Which key events the grab takes: the press of each system shortcut and its repeats, and the
/// release of each key whose press it took, whatever the modifiers by then, so a key is never
/// half taken.
#[derive(Clone, Debug, Default)]
pub(crate) struct Taking {
    held: Vec<u16>,
}

impl Taking {
    /// Whether to take `key`.
    pub(crate) fn take(&mut self, key: NativeKey) -> bool {
        match key.kind {
            KeyKind::Flags => false,
            KeyKind::Up => {
                let Some(at) = self.held.iter().position(|&vk| vk == key.vk) else {
                    return false;
                };
                self.held.swap_remove(at);
                true
            }
            KeyKind::Down | KeyKind::Repeat => {
                if self.held.contains(&key.vk) {
                    return true;
                }
                if !is_system_shortcut(key.vk, key.mods) {
                    return false;
                }
                self.held.push(key.vk);
                true
            }
        }
    }
}

struct Shared {
    taking: RefCell<Taking>,
    queue: RefCell<VecDeque<NativeKey>>,
    wake: Box<dyn Fn()>,
}

/// The keys taken for one window, in order, while the grab holds. Dropping the last clone lets
/// go: the system's shortcuts are this Mac's again. Main thread only.
#[derive(Clone)]
pub(crate) struct Taken {
    shared: Rc<Shared>,
    _held: Option<Rc<dyn Held>>,
}

impl std::fmt::Debug for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Taken")
            .field("queued", &self.shared.queue.borrow().len())
            .finish_non_exhaustive()
    }
}

/// What the platform keeps while a grab holds; dropped, it lets go.
pub(crate) trait Held {}

impl Taken {
    /// A queue fed by [`Self::offer`]: the monitor's, and a test's. `wake` is called after
    /// each key taken; `held` is what lets go when the last clone goes.
    #[must_use]
    pub(crate) fn new(wake: Box<dyn Fn()>, held: Option<Rc<dyn Held>>) -> Self {
        Self {
            shared: Rc::new(Shared {
                taking: RefCell::new(Taking::default()),
                queue: RefCell::new(VecDeque::new()),
                wake,
            }),
            _held: held,
        }
    }

    /// Offer a key event, as the monitor does: taken (queued, and the view woken) or left to
    /// the app. Whether it was taken.
    pub(crate) fn offer(&self, key: NativeKey) -> bool {
        if !self.shared.taking.borrow_mut().take(key) {
            return false;
        }
        self.shared.queue.borrow_mut().push_back(key);
        (self.shared.wake)();
        true
    }

    /// The keys taken since the last drain, oldest first.
    #[must_use]
    pub(crate) fn drain(&self) -> Vec<NativeKey> {
        self.shared.queue.borrow_mut().drain(..).collect()
    }
}

/// Why the grab did not take hold.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Refused {
    /// macOS turns the system's shortcuts off only for an app trusted for Accessibility, and
    /// this one is not.
    NeedsAccessibility,
    /// AppKit refused the monitor, or this is not the main thread.
    Unavailable,
}

/// What the view asks the platform for. A seam: tests drive a stand-in.
pub(crate) trait Shortcuts {
    /// Take the system's shortcuts until the answer is dropped, calling `wake` for each key
    /// taken.
    ///
    /// # Errors
    ///
    /// [`Refused`] when they stay this Mac's.
    fn grab(&self, wake: Box<dyn Fn()>) -> Result<Taken, Refused>;
}

/// This Mac's own.
pub(crate) struct System;

impl Shortcuts for System {
    fn grab(&self, wake: Box<dyn Fn()>) -> Result<Taken, Refused> {
        mac::grab(wake)
    }
}

/// A platform that never grabs: under test, where nothing is ever taken from the system.
#[cfg(test)]
pub(crate) struct Inert;

#[cfg(test)]
impl Shortcuts for Inert {
    fn grab(&self, _wake: Box<dyn Fn()>) -> Result<Taken, Refused> {
        Err(Refused::Unavailable)
    }
}

/// AppKit's and HIToolbox's side: the monitor and the shortcut mode.
mod mac {
    use std::cell::{OnceCell, RefCell};
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::rc::{Rc, Weak};

    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};

    use super::{Held, Refused, Shared, Taken};
    use crate::desktop::keys::{KeyKind, NativeKey, mods_of};

    /// `kHIHotKeyModeAllDisabled` (`<HIToolbox/CarbonEvents.h>`): every symbolic hot key off
    /// while this app is frontmost.
    const ALL_DISABLED: u32 = 1 << 0;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        /// `void *PushSymbolicHotKeyMode(OptionBits inOptions)`: a token for the pop.
        fn PushSymbolicHotKeyMode(options: u32) -> *mut c_void;
        /// `void PopSymbolicHotKeyMode(void *inToken)`.
        fn PopSymbolicHotKeyMode(token: *mut c_void);
    }

    thread_local! {
        /// The one monitor, installed on the first grab and kept for the app's life: it passes
        /// every event while nothing grabs.
        static MONITOR: OnceCell<Option<Retained<AnyObject>>> = const { OnceCell::new() };
        /// The grab holding now.
        static GRABBING: RefCell<Weak<Shared>> = const { RefCell::new(Weak::new()) };
    }

    /// The system's shortcuts turned off for this app, until dropped.
    struct Mode(NonNull<c_void>);

    impl Held for Mode {}

    impl Drop for Mode {
        fn drop(&mut self) {
            // SAFETY: the token `PushSymbolicHotKeyMode` returned, popped once.
            unsafe { PopSymbolicHotKeyMode(self.0.as_ptr()) };
        }
    }

    pub(super) fn grab(wake: Box<dyn Fn()>) -> Result<Taken, Refused> {
        MainThreadMarker::new().ok_or(Refused::Unavailable)?;
        if !onicode_grants::Grants::current().accessibility {
            return Err(Refused::NeedsAccessibility);
        }
        let installed = MONITOR.with(|monitor| monitor.get_or_init(install).is_some());
        if !installed {
            return Err(Refused::Unavailable);
        }
        // SAFETY: a documented HIToolbox call on the main thread; the token is popped once, by
        // `Mode`'s drop.
        let token = unsafe { PushSymbolicHotKeyMode(ALL_DISABLED) };
        let mode = NonNull::new(token).map(|token| Rc::new(Mode(token)) as Rc<dyn Held>);
        let taken = Taken::new(wake, mode);
        GRABBING.with(|grabbing| grabbing.replace(Rc::downgrade(&taken.shared)));
        Ok(taken)
    }

    /// `event` as a [`NativeKey`], when it is a key event.
    fn native(event: &NSEvent) -> Option<NativeKey> {
        let kind = match event.r#type() {
            NSEventType::KeyDown if event.isARepeat() => KeyKind::Repeat,
            NSEventType::KeyDown => KeyKind::Down,
            NSEventType::KeyUp => KeyKind::Up,
            _ => return None,
        };
        Some(NativeKey {
            vk: event.keyCode(),
            kind,
            mods: mods_of(event.modifierFlags()),
        })
    }

    /// Install the local monitor for key presses and releases.
    fn install() -> Option<Retained<AnyObject>> {
        let handler = block2::RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit hands the handler a live event for the length of the call.
            let key = native(unsafe { event.as_ref() });
            let grabbing = GRABBING.with(|grabbing| grabbing.borrow().upgrade());
            let (Some(key), Some(shared)) = (key, grabbing) else {
                return event.as_ptr();
            };
            let taken = Taken {
                shared,
                _held: None,
            };
            if taken.offer(key) {
                std::ptr::null_mut()
            } else {
                event.as_ptr()
            }
        });
        // SAFETY: the handler returns the event it was given or null, as a local monitor's
        // must (`NSEvent.h`: "return nil to stop dispatching").
        unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(
                NSEventMask::KeyDown | NSEventMask::KeyUp,
                &handler,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use onicode_desktop_proto::input::Mods;

    use super::{MISSION_CONTROL, Taken, Taking, is_system_shortcut};
    use crate::desktop::keys::{KeyKind, NativeKey};

    fn key(vk: u16, kind: KeyKind, mods: Mods) -> NativeKey {
        NativeKey { vk, kind, mods }
    }

    /// The system's shortcuts are taken; the ways out, the app's own shortcuts and plain typing
    /// are not.
    #[test]
    fn the_system_shortcuts_are_the_macs_and_the_ways_out_stay() {
        let cmd = Mods::SUPER;
        let ctrl = Mods::CTRL;
        for (vk, mods, name) in [
            (0x30, cmd, "⌘Tab"),
            (0x30, cmd | Mods::SHIFT, "⌘⇧Tab"),
            (0x32, cmd, "⌘`"),
            (0x31, cmd, "⌘Space"),
            (0x31, ctrl, "⌃Space"),
            (0x7e, ctrl, "⌃↑"),
            (0x7b, ctrl, "⌃←"),
            (0x04, cmd, "⌘H"),
            (0x04, cmd | Mods::ALT, "⌘⌥H"),
            (0x2e, cmd, "⌘M"),
            (0x63, Mods::empty(), "F3"),
            (MISSION_CONTROL, Mods::empty(), "Mission Control"),
        ] {
            assert!(is_system_shortcut(vk, mods), "{name} is the Mac's");
        }
        for (vk, mods, name) in [
            (0x35, cmd | Mods::ALT, "⌘⌥⎋"),
            (0x0c, cmd | ctrl, "⌃⌘Q"),
            (0x0c, cmd, "⌘Q"),
            (0x0d, cmd, "⌘W"),
            (0x30, Mods::empty(), "Tab"),
            (0x30, ctrl, "⌃Tab"),
            (0x7e, cmd, "⌘↑"),
            (0x00, Mods::empty(), "A"),
            (0x31, Mods::SHIFT, "⇧Space"),
        ] {
            assert!(!is_system_shortcut(vk, mods), "{name} is not taken");
        }
    }

    /// A taken press's repeats and release are taken, even once ⌘ is up; a key never taken
    /// passes, and a modifier is never taken.
    #[test]
    fn a_taken_key_is_taken_whole() {
        let mut taking = Taking::default();
        let tab = 0x30;
        assert!(taking.take(key(tab, KeyKind::Down, Mods::SUPER)));
        assert!(taking.take(key(tab, KeyKind::Repeat, Mods::SUPER)));
        assert!(
            taking.take(key(tab, KeyKind::Up, Mods::empty())),
            "its release"
        );
        assert!(
            !taking.take(key(tab, KeyKind::Up, Mods::empty())),
            "let go once"
        );
        assert!(!taking.take(key(0x00, KeyKind::Down, Mods::empty())));
        assert!(!taking.take(key(0x00, KeyKind::Up, Mods::empty())));
        assert!(!taking.take(key(0x37, KeyKind::Flags, Mods::SUPER)));
    }

    /// What is taken is queued in order and wakes the view each time; what is left is not.
    #[test]
    fn taken_keys_queue_in_order_and_wake() {
        let woken = Rc::new(Cell::new(0_u32));
        let count = Rc::clone(&woken);
        let taken = Taken::new(
            Box::new(move || count.set(count.get().saturating_add(1))),
            None,
        );
        let cmd_tab = key(0x30, KeyKind::Down, Mods::SUPER);
        let a = key(0x00, KeyKind::Down, Mods::empty());
        let tab_up = key(0x30, KeyKind::Up, Mods::SUPER);
        assert!(taken.offer(cmd_tab));
        assert!(!taken.offer(a));
        assert!(taken.offer(tab_up));
        assert_eq!(taken.drain(), [cmd_tab, tab_up]);
        assert_eq!(woken.get(), 2);
        assert!(taken.drain().is_empty());
    }
}
