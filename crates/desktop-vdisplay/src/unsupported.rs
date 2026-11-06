//! Where there is no `CGVirtualDisplay`: the same API, never constructed.

use core::convert::Infallible;
use core::marker::PhantomData;
use std::rc::Rc;

use crate::plan::{Mode, Plan};
use crate::{DisplayError, Enforced};

/// Never: virtual displays exist only on macOS.
#[must_use]
pub const fn available() -> bool {
    false
}

/// A virtual display; this platform has none.
#[derive(Debug)]
#[expect(
    missing_copy_implementations,
    reason = "the macOS display owns an object"
)]
pub struct VirtualDisplay {
    never: Infallible,
    /// Not `Send`, like the macOS display, so code checked here holds there.
    _main_thread: PhantomData<Rc<()>>,
}

impl VirtualDisplay {
    /// Always [`DisplayError::Unavailable`].
    ///
    /// # Errors
    ///
    /// Always [`DisplayError::Unavailable`].
    pub fn create(_plan: &Plan) -> Result<Self, DisplayError> {
        Err(DisplayError::Unavailable(
            "virtual displays exist only on macOS".to_owned(),
        ))
    }

    /// Unreachable: no value exists.
    ///
    /// # Errors
    ///
    /// None; no value exists.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "the macOS display's signature"
    )]
    pub const fn resize(&mut self, _plan: &Plan) -> Result<(), DisplayError> {
        match self.never {}
    }

    /// Unreachable: no value exists.
    #[must_use]
    pub const fn display_id(&self) -> u32 {
        match self.never {}
    }

    /// Unreachable: no value exists.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        match self.never {}
    }

    /// Unreachable: no value exists.
    ///
    /// # Errors
    ///
    /// None; no value exists.
    pub const fn enforce(&self) -> Result<Enforced, DisplayError> {
        match self.never {}
    }
}
