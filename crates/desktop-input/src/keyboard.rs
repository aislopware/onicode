//! The keyboard constants the host writes on the events it posts: which key of a pair holds a
//! modifier, and media keys as the HID system sends them.

/// Which key of a pair holds a modifier: the device-dependent bits of an event's flags.
///
/// `NX_DEVICE*KEYMASK` in `<IOKit/hidsystem/IOLLEvent.h>`, which objc2 does not bind. The
/// host writes them on the keys it posts.
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
/// key in bits 16–31 and its state in bits 8–15. The host posts them.
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
