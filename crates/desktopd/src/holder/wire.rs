//! What a desktopd and the display holder say to each other.
//!
//! One JSON message a line, on the holder's Unix socket. These bytes are the holder's
//! fingerprint ([`super::fingerprint`]), so
//! a desktopd that finds a holder of another wire has it quit and starts its own.

use onicode_desktop_proto::screen::DisplayKey;
use onicode_desktop_vdisplay::{Descriptor, DisplayError, Enforced, Mode, Plan};
use serde::{Deserialize, Serialize};

/// A display's plan ([`Plan`]), flat.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WirePlan {
    /// [`Descriptor::vendor_id`].
    pub vendor_id: u32,
    /// [`Descriptor::product_id`].
    pub product_id: u32,
    /// [`Descriptor::serial`].
    pub serial: u32,
    /// [`Descriptor::max_pixels`].
    pub max_pixels: (u32, u32),
    /// [`Descriptor::size_mm`].
    pub size_mm: (f64, f64),
    /// [`Mode::points`].
    pub points: (u32, u32),
    /// [`Mode::pixels`].
    pub pixels: (u32, u32),
    /// [`Mode::hidpi`].
    pub hidpi: bool,
    /// [`Mode::refresh_hz`].
    pub refresh_hz: u32,
}

impl From<&Plan> for WirePlan {
    fn from(plan: &Plan) -> Self {
        let Plan {
            descriptor: d,
            mode: m,
        } = *plan;
        Self {
            vendor_id: d.vendor_id,
            product_id: d.product_id,
            serial: d.serial,
            max_pixels: d.max_pixels,
            size_mm: d.size_mm,
            points: m.points,
            pixels: m.pixels,
            hidpi: m.hidpi,
            refresh_hz: m.refresh_hz,
        }
    }
}

impl From<WirePlan> for Plan {
    fn from(w: WirePlan) -> Self {
        Self {
            descriptor: Descriptor {
                vendor_id: w.vendor_id,
                product_id: w.product_id,
                serial: w.serial,
                max_pixels: w.max_pixels,
                size_mm: w.size_mm,
            },
            mode: Mode {
                points: w.points,
                pixels: w.pixels,
                hidpi: w.hidpi,
                refresh_hz: w.refresh_hz,
            },
        }
    }
}

/// What a desktopd asks of the holder.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Ask {
    /// Which wire it speaks: answered [`Answer::Hello`].
    Hello,
    /// The display for `key`, made to `plan`, or the one it holds put in `plan`'s mode:
    /// answered [`Answer::Made`].
    Create {
        /// The client's key.
        key: DisplayKey,
        /// Its plan.
        plan: WirePlan,
    },
    /// `key`'s display in `plan`'s mode: answered [`Answer::Done`].
    Resize {
        /// The client's key.
        key: DisplayKey,
        /// Its plan.
        plan: WirePlan,
    },
    /// `key`'s display put back in its mode: answered [`Answer::Enforced`].
    Enforce {
        /// The client's key.
        key: DisplayKey,
    },
    /// `key`'s display ended: answered [`Answer::Done`].
    Release {
        /// The client's key.
        key: DisplayKey,
    },
    /// Every display it holds: answered [`Answer::Held`].
    Held,
    /// End, leaving the displays to the next holder to make again: answered [`Answer::Done`].
    Quit,
}

/// One display the holder holds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HeldDisplay {
    /// The client's key.
    pub key: DisplayKey,
    /// Its plan.
    pub plan: WirePlan,
    /// Its `CGDirectDisplayID`.
    pub id: u32,
}

/// What the holder answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Answer {
    /// The wire it speaks.
    Hello {
        /// [`super::fingerprint`].
        fingerprint: u64,
    },
    /// The display's id.
    Made {
        /// Its `CGDirectDisplayID`.
        id: u32,
    },
    /// Done.
    Done,
    /// What the enforce found.
    Enforced(WireEnforced),
    /// The displays it holds.
    Held(Vec<HeldDisplay>),
    /// Why it could not.
    Failed(WireError),
}

/// [`Enforced`], on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireEnforced {
    /// [`Enforced::Pending`].
    Pending,
    /// [`Enforced::Settled`].
    Settled,
    /// [`Enforced::Applied`].
    Applied,
}

impl From<Enforced> for WireEnforced {
    fn from(enforced: Enforced) -> Self {
        match enforced {
            Enforced::Pending => Self::Pending,
            Enforced::Settled => Self::Settled,
            Enforced::Applied => Self::Applied,
        }
    }
}

impl From<WireEnforced> for Enforced {
    fn from(enforced: WireEnforced) -> Self {
        match enforced {
            WireEnforced::Pending => Self::Pending,
            WireEnforced::Settled => Self::Settled,
            WireEnforced::Applied => Self::Applied,
        }
    }
}

/// [`DisplayError`], on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WireError {
    /// [`DisplayError::Unavailable`].
    Unavailable(String),
    /// [`DisplayError::NotMainThread`].
    NotMainThread,
    /// [`DisplayError::Refused`].
    Refused,
    /// [`DisplayError::Rejected`].
    Rejected,
    /// [`DisplayError::Outgrown`].
    Outgrown {
        /// The mode's pixels.
        wanted: (u32, u32),
        /// The descriptor's maximum.
        max: (u32, u32),
    },
    /// [`DisplayError::Configure`].
    Configure(i32),
    /// The holder holds no display for the key.
    Unknown,
}

impl From<DisplayError> for WireError {
    fn from(error: DisplayError) -> Self {
        match error {
            DisplayError::Unavailable(why) => Self::Unavailable(why),
            DisplayError::NotMainThread => Self::NotMainThread,
            DisplayError::Refused => Self::Refused,
            DisplayError::Rejected => Self::Rejected,
            DisplayError::Outgrown { wanted, max } => Self::Outgrown { wanted, max },
            DisplayError::Configure(code) => Self::Configure(code),
        }
    }
}

impl From<WireError> for DisplayError {
    fn from(error: WireError) -> Self {
        match error {
            WireError::Unavailable(why) => Self::Unavailable(why),
            WireError::NotMainThread => Self::NotMainThread,
            WireError::Refused | WireError::Unknown => Self::Refused,
            WireError::Rejected => Self::Rejected,
            WireError::Outgrown { wanted, max } => Self::Outgrown { wanted, max },
            WireError::Configure(code) => Self::Configure(code),
        }
    }
}
