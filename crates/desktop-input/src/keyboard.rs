//! The keys of a remote tile, by their place on the keyboard, taken before this Mac's text
//! system composes them.
//!
//! A remote Mac that has taken this Mac's input source composes on its own: a dead key, an
//! input method's marked text and its candidates happen in the remote app. For that the keys
//! must reach it raw. GPUI hands a key to the text system first (a dead key, an input method's
//! keys) and never to the view, so while a tile grabs the keyboard a local event monitor takes
//! the keys without ⌘ or ⌃ off `-[NSApplication sendEvent:]`, ahead of the window and the input
//! method, and queues them for the view ([`Taken`]). A chord with ⌘ or ⌃ goes on to GPUI, whose
//! key bindings (⌃Tab out of the tile, the workspace's ⌘ chords) come first; the view reads its
//! position off the event being handled (`current`). The decision is pure ([`Taking`]).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use onicode_desktop_proto::input::Mods;

/// One key event as the keyboard reported it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NativeKey {
    /// The Mac virtual key code (`kVK_*`): the key's position.
    pub vk: u16,
    /// What happened to it.
    pub kind: KeyKind,
    /// The modifiers held, with their side, Caps Lock and fn.
    pub mods: Mods,
}

/// What a [`NativeKey`] event is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KeyKind {
    /// Went down.
    Down,
    /// The system's auto-repeat.
    Repeat,
    /// Let go.
    Up,
    /// A modifier key went down or up (`flagsChanged:`).
    Flags,
}

/// Which key events the monitor takes: every press and repeat without ⌘ or ⌃, and the
/// release of each key whose press it took, even once ⌘ is down, so a key is never half taken.
#[derive(Clone, Debug, Default)]
pub struct Taking {
    held: Vec<u16>,
}

impl Taking {
    /// Whether to take `key`.
    pub fn take(&mut self, key: NativeKey) -> bool {
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
                if key.mods.intersects(Mods::SUPER | Mods::CTRL) {
                    return false;
                }
                self.held.push(key.vk);
                true
            }
        }
    }
}

/// What the monitor and the view share while a tile grabs the keyboard.
struct Shared {
    taking: RefCell<Taking>,
    queue: RefCell<VecDeque<NativeKey>>,
    wake: Box<dyn Fn()>,
}

/// The keys taken for one tile, in order, until the last clone is dropped. Main thread only.
#[derive(Clone)]
pub struct Taken {
    shared: Rc<Shared>,
}

impl std::fmt::Debug for Taken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Taken")
            .field("queued", &self.shared.queue.borrow().len())
            .finish()
    }
}

impl Taken {
    /// A queue no monitor feeds: what `take_keys` hands out, before it is registered, and a
    /// stand-in for tests, which [`Self::offer`] feeds as the monitor would. `wake` is called
    /// after each key taken.
    #[must_use]
    pub fn detached(wake: Box<dyn Fn()>) -> Self {
        Self {
            shared: Rc::new(Shared {
                taking: RefCell::new(Taking::default()),
                queue: RefCell::new(VecDeque::new()),
                wake,
            }),
        }
    }

    /// Offer a key event, as the monitor does: taken (queued, and the view woken) or left to
    /// the app. Whether it was taken.
    pub fn offer(&self, key: NativeKey) -> bool {
        if !self.shared.taking.borrow_mut().take(key) {
            return false;
        }
        self.shared.queue.borrow_mut().push_back(key);
        (self.shared.wake)();
        true
    }

    /// The keys taken since the last drain, oldest first.
    #[must_use]
    pub fn drain(&self) -> Vec<NativeKey> {
        self.shared.queue.borrow_mut().drain(..).collect()
    }
}

#[cfg(target_os = "macos")]
pub use mac::{caps_lock, current, mods_of, take_keys};

/// Which key of a pair holds a modifier: the device-dependent bits of an event's flags.
///
/// `NX_DEVICE*KEYMASK` in `<IOKit/hidsystem/IOLLEvent.h>`, which objc2 does not bind. The
/// client reads them and the host writes them.
pub mod device {
    /// `NX_DEVICELCTLKEYMASK`.
    pub const LEFT_CONTROL: usize = 0x0000_0001;
    /// `NX_DEVICELSHIFTKEYMASK`.
    pub const LEFT_SHIFT: usize = 0x0000_0002;
    /// `NX_DEVICERSHIFTKEYMASK`.
    pub const RIGHT_SHIFT: usize = 0x0000_0004;
    /// `NX_DEVICELCMDKEYMASK`.
    pub const LEFT_COMMAND: usize = 0x0000_0008;
    /// `NX_DEVICERCMDKEYMASK`.
    pub const RIGHT_COMMAND: usize = 0x0000_0010;
    /// `NX_DEVICELALTKEYMASK`.
    pub const LEFT_OPTION: usize = 0x0000_0020;
    /// `NX_DEVICERALTKEYMASK`.
    pub const RIGHT_OPTION: usize = 0x0000_0040;
    /// `NX_DEVICERCTLKEYMASK`.
    pub const RIGHT_CONTROL: usize = 0x0000_2000;
}

/// Media keys as the HID system sends them: system-defined events whose `data1` carries the
/// key in bits 16–31 and its state in bits 8–15. The client's tap reads them and the host
/// posts them.
///
/// `NX_KEYTYPE_*` in `<IOKit/hidsystem/ev_keymap.h>`, and the event's type, subtype and key
/// states in `<IOKit/hidsystem/IOLLEvent.h>`, none of which objc2 binds.
pub mod nx {
    /// `NX_KEYTYPE_PLAY`.
    pub const KEYTYPE_PLAY: isize = 16;
    /// `NX_KEYTYPE_NEXT`.
    pub const KEYTYPE_NEXT: isize = 17;
    /// `NX_KEYTYPE_PREVIOUS`.
    pub const KEYTYPE_PREVIOUS: isize = 18;
    /// `NX_KEYTYPE_FAST`: what an Apple keyboard's ⏭ sends.
    pub const KEYTYPE_FAST: isize = 19;
    /// `NX_KEYTYPE_REWIND`: what an Apple keyboard's ⏮ sends.
    pub const KEYTYPE_REWIND: isize = 20;
    /// `NX_KEYDOWN`: the key state of a press.
    pub const KEYDOWN: isize = 0x0a;
    /// `NX_KEYUP`: the key state of a release.
    pub const KEYUP: isize = 0x0b;
    /// `NX_SYSDEFINED`: the event type media keys come as.
    pub const SYSDEFINED: u32 = 14;
    /// `NX_SUBTYPE_AUX_CONTROL_BUTTONS`: a system-defined event that is a media key.
    pub const SUBTYPE_AUX_CONTROL_BUTTONS: i16 = 8;
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::{OnceCell, RefCell};
    use std::ptr::NonNull;
    use std::rc::{Rc, Weak};

    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};
    use onicode_desktop_proto::input::Mods;

    use super::device::{
        LEFT_COMMAND, LEFT_CONTROL, LEFT_OPTION, LEFT_SHIFT, RIGHT_COMMAND, RIGHT_CONTROL,
        RIGHT_OPTION, RIGHT_SHIFT,
    };
    use super::{KeyKind, NativeKey, Shared, Taken};

    /// An event's modifier flags as [`Mods`]: each modifier with the right side marked when
    /// only the right key holds it, Caps Lock and fn.
    #[must_use]
    pub fn mods_of(flags: NSEventModifierFlags) -> Mods {
        let raw = flags.0;
        let side = |held: bool, left: usize, right: usize, right_mod: Mods| {
            if held && raw & right != 0 && raw & left == 0 {
                right_mod
            } else {
                Mods::empty()
            }
        };
        let mut mods = Mods::empty();
        for (flag, generic, left, right, right_mod) in [
            (
                NSEventModifierFlags::Shift,
                Mods::SHIFT,
                LEFT_SHIFT,
                RIGHT_SHIFT,
                Mods::SHIFT_RIGHT,
            ),
            (
                NSEventModifierFlags::Control,
                Mods::CTRL,
                LEFT_CONTROL,
                RIGHT_CONTROL,
                Mods::CTRL_RIGHT,
            ),
            (
                NSEventModifierFlags::Option,
                Mods::ALT,
                LEFT_OPTION,
                RIGHT_OPTION,
                Mods::ALT_RIGHT,
            ),
            (
                NSEventModifierFlags::Command,
                Mods::SUPER,
                LEFT_COMMAND,
                RIGHT_COMMAND,
                Mods::SUPER_RIGHT,
            ),
        ] {
            let held = flags.contains(flag);
            if held {
                mods |= generic;
            }
            mods |= side(held, left, right, right_mod);
        }
        mods.set(
            Mods::CAPS_LOCK,
            flags.contains(NSEventModifierFlags::CapsLock),
        );
        mods.set(Mods::FN, flags.contains(NSEventModifierFlags::Function));
        mods
    }

    /// `event` as a [`NativeKey`], when it is a key or modifier event.
    fn native(event: &NSEvent) -> Option<NativeKey> {
        let kind = match event.r#type() {
            NSEventType::KeyDown if event.isARepeat() => KeyKind::Repeat,
            NSEventType::KeyDown => KeyKind::Down,
            NSEventType::KeyUp => KeyKind::Up,
            NSEventType::FlagsChanged => KeyKind::Flags,
            _ => return None,
        };
        Some(NativeKey {
            vk: event.keyCode(),
            kind,
            mods: mods_of(event.modifierFlags()),
        })
    }

    /// The key event AppKit is dispatching now (what GPUI's key handlers are being called
    /// for), or `None` outside one and off the main thread.
    #[must_use]
    pub fn current() -> Option<NativeKey> {
        let mtm = MainThreadMarker::new()?;
        let event = NSApplication::sharedApplication(mtm).currentEvent()?;
        native(&event)
    }

    /// Whether Caps Lock is on; `None` off the main thread.
    #[must_use]
    pub fn caps_lock() -> Option<bool> {
        MainThreadMarker::new()?;
        Some(NSEvent::modifierFlags_class().contains(NSEventModifierFlags::CapsLock))
    }

    thread_local! {
        /// The one monitor, installed on the first grab and kept for the app's life: it passes
        /// every event while nothing grabs.
        static MONITOR: OnceCell<Option<Retained<AnyObject>>> = const { OnceCell::new() };
        /// The tile grabbing the keyboard now.
        static GRABBING: RefCell<Weak<Shared>> = const { RefCell::new(Weak::new()) };
    }

    /// Grab the keyboard for a tile until the handle is dropped.
    ///
    /// Key presses without ⌘ or ⌃ and their releases are taken off the app, queued on the
    /// handle, and `wake` is called for each. A later grab replaces this one. `None` off the
    /// main thread or when AppKit refuses the monitor.
    #[must_use]
    pub fn take_keys(wake: Box<dyn Fn()>) -> Option<Taken> {
        MainThreadMarker::new()?;
        let installed = MONITOR.with(|monitor| monitor.get_or_init(install).is_some());
        if !installed {
            return None;
        }
        let taken = Taken::detached(wake);
        GRABBING.with(|grabbing| grabbing.replace(Rc::downgrade(&taken.shared)));
        Some(taken)
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
            let taken = Taken { shared };
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

    #[cfg(test)]
    mod tests {
        use objc2_app_kit::NSEventModifierFlags;
        use onicode_desktop_proto::input::Mods;

        use super::super::device::{
            LEFT_SHIFT, RIGHT_COMMAND, RIGHT_CONTROL, RIGHT_OPTION, RIGHT_SHIFT,
        };
        use super::mods_of;

        /// A modifier held by its right key alone says so; held by both, or by the left, it is
        /// the plain modifier; Caps Lock and fn ride along.
        #[test]
        fn modifier_flags_keep_their_side() {
            let flags = |generic: NSEventModifierFlags, device: usize| {
                NSEventModifierFlags(generic.0 | device)
            };
            assert_eq!(
                mods_of(flags(NSEventModifierFlags::Shift, RIGHT_SHIFT)),
                Mods::SHIFT | Mods::SHIFT_RIGHT
            );
            assert_eq!(
                mods_of(flags(NSEventModifierFlags::Shift, LEFT_SHIFT)),
                Mods::SHIFT
            );
            assert_eq!(
                mods_of(flags(NSEventModifierFlags::Shift, LEFT_SHIFT | RIGHT_SHIFT)),
                Mods::SHIFT
            );
            assert_eq!(
                mods_of(flags(NSEventModifierFlags::Command, RIGHT_COMMAND)),
                Mods::SUPER | Mods::SUPER_RIGHT
            );
            assert_eq!(
                mods_of(flags(NSEventModifierFlags::Option, RIGHT_OPTION)),
                Mods::ALT | Mods::ALT_RIGHT
            );
            assert_eq!(
                mods_of(flags(NSEventModifierFlags::Control, RIGHT_CONTROL)),
                Mods::CTRL | Mods::CTRL_RIGHT
            );
            let locks = NSEventModifierFlags::CapsLock | NSEventModifierFlags::Function;
            assert_eq!(mods_of(locks), Mods::CAPS_LOCK | Mods::FN);
            assert_eq!(
                mods_of(flags(NSEventModifierFlags::empty(), RIGHT_SHIFT)),
                Mods::empty()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use onicode_desktop_proto::input::Mods;

    use super::{KeyKind, NativeKey, Taken, Taking};

    fn key(vk: u16, kind: KeyKind, mods: Mods) -> NativeKey {
        NativeKey { vk, kind, mods }
    }

    /// Plain and shifted keys are taken, and a dead key or an input method's key with them,
    /// since the text system never sees them; a ⌘ or ⌃ chord goes on to the app's bindings;
    /// a taken key's release is taken even with ⌘ down, and one never taken passes.
    #[test]
    fn plain_keys_are_taken_and_chords_pass() {
        let mut taking = Taking::default();
        let e = 0x0e;
        let q = 0x0c;
        assert!(
            taking.take(key(e, KeyKind::Down, Mods::ALT)),
            "⌥E, a dead key"
        );
        assert!(
            taking.take(key(e, KeyKind::Repeat, Mods::SUPER)),
            "its repeat, ⌘ or not"
        );
        assert!(taking.take(key(e, KeyKind::Up, Mods::SUPER)), "its release");
        assert!(
            !taking.take(key(e, KeyKind::Up, Mods::empty())),
            "let go once"
        );
        assert!(
            !taking.take(key(q, KeyKind::Down, Mods::SUPER)),
            "⌘Q goes to the app"
        );
        assert!(
            !taking.take(key(q, KeyKind::Up, Mods::empty())),
            "and so does its release"
        );
        assert!(
            !taking.take(key(0x30, KeyKind::Down, Mods::CTRL)),
            "⌃Tab leaves the tile"
        );
        assert!(taking.take(key(0x00, KeyKind::Down, Mods::SHIFT | Mods::CAPS_LOCK)));
        assert!(
            !taking.take(key(0x38, KeyKind::Flags, Mods::SHIFT)),
            "modifiers go to GPUI"
        );
    }

    /// What the monitor takes is queued in order and wakes the view each time; what it leaves
    /// is not queued.
    #[test]
    fn taken_keys_queue_in_order_and_wake() {
        let woken = Rc::new(Cell::new(0_u32));
        let count = Rc::clone(&woken);
        let taken = Taken::detached(Box::new(move || count.set(count.get().saturating_add(1))));
        let a = key(0x00, KeyKind::Down, Mods::empty());
        let cmd_c = key(0x08, KeyKind::Down, Mods::SUPER);
        let a_up = key(0x00, KeyKind::Up, Mods::empty());
        assert!(taken.offer(a));
        assert!(!taken.offer(cmd_c));
        assert!(taken.offer(a_up));
        assert_eq!(taken.drain(), [a, a_up]);
        assert_eq!(woken.get(), 2);
        assert_eq!(taken.drain(), Vec::<NativeKey>::new());
    }
}
