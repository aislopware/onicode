//! The display itself, through CoreGraphics' private `CGVirtualDisplay` classes.
//!
//! The classes have no public header, so they are looked up by name at runtime and every
//! selector is checked before it is sent; anything missing is [`DisplayError::Unavailable`].

use core::ffi::CStr;

use dispatch2::DispatchQueue;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{MainThreadMarker, msg_send};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFRetained, CFString};
use objc2_core_graphics::{
    CGBeginDisplayConfiguration, CGCancelDisplayConfiguration, CGCompleteDisplayConfiguration,
    CGConfigureDisplayMirrorOfDisplay, CGConfigureDisplayWithDisplayMode, CGConfigureOption,
    CGDirectDisplayID, CGDisplayConfigRef, CGDisplayCopyAllDisplayModes, CGDisplayCopyDisplayMode,
    CGDisplayIsOnline, CGDisplayMirrorsDisplay, CGDisplayMode, CGError, kCGNullDirectDisplay,
};
use objc2_foundation::{NSArray, NSSize, NSString};

use crate::plan::{Mode, NAME, Plan};
use crate::{DisplayError, Enforced};

/// A class and the instance selectors this crate sends to it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Wanted {
    pub class: &'static CStr,
    pub selectors: &'static [&'static CStr],
}

/// The descriptor, mode, settings and display classes, in that order.
pub(crate) const WANTED: [Wanted; 4] = [
    Wanted {
        class: c"CGVirtualDisplayDescriptor",
        selectors: &[
            c"setVendorID:",
            c"setProductID:",
            c"setSerialNum:",
            c"setName:",
            c"setSizeInMillimeters:",
            c"setMaxPixelsWide:",
            c"setMaxPixelsHigh:",
            c"setDispatchQueue:",
        ],
    },
    Wanted {
        class: c"CGVirtualDisplayMode",
        selectors: &[c"initWithWidth:height:refreshRate:"],
    },
    Wanted {
        class: c"CGVirtualDisplaySettings",
        selectors: &[c"setHiDPI:", c"setModes:"],
    },
    Wanted {
        class: c"CGVirtualDisplay",
        selectors: &[c"initWithDescriptor:", c"applySettings:", c"displayID"],
    },
];

/// The four classes, each known to answer every selector [`WANTED`] lists for it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Classes {
    descriptor: &'static AnyClass,
    mode: &'static AnyClass,
    settings: &'static AnyClass,
    display: &'static AnyClass,
}

impl Classes {
    pub(crate) fn resolve() -> Result<Self, DisplayError> {
        Self::resolve_from(&WANTED)
    }

    pub(crate) fn resolve_from(wanted: &[Wanted; 4]) -> Result<Self, DisplayError> {
        let [descriptor, mode, settings, display] = wanted.map(|w| {
            let class = AnyClass::get(w.class).ok_or_else(|| {
                DisplayError::Unavailable(format!("no class {}", w.class.to_string_lossy()))
            })?;
            for name in w.selectors {
                if !class.responds_to(Sel::register(name)) {
                    return Err(DisplayError::Unavailable(format!(
                        "{} does not answer {}",
                        w.class.to_string_lossy(),
                        name.to_string_lossy()
                    )));
                }
            }
            Ok(class)
        });
        Ok(Self {
            descriptor: descriptor?,
            mode: mode?,
            settings: settings?,
            display: display?,
        })
    }

    /// A configured `CGVirtualDisplayDescriptor`. Building one creates no display.
    pub(crate) fn descriptor(self, plan: &Plan) -> Result<Retained<AnyObject>, DisplayError> {
        let d = &plan.descriptor;
        // SAFETY: `new` on an `NSObject` subclass returns a +1 instance or nil.
        let descriptor: Option<Retained<AnyObject>> = unsafe { msg_send![self.descriptor, new] };
        let descriptor = descriptor.ok_or(DisplayError::Refused)?;
        let name = NSString::from_str(NAME);
        let size = NSSize::new(d.size_mm.0, d.size_mm.1);
        // Each setter below was checked by `resolve_from` and takes the header's type.
        // SAFETY: `unsigned int`.
        let () = unsafe { msg_send![&*descriptor, setVendorID: d.vendor_id] };
        // SAFETY: `unsigned int`.
        let () = unsafe { msg_send![&*descriptor, setProductID: d.product_id] };
        // SAFETY: `unsigned int`.
        let () = unsafe { msg_send![&*descriptor, setSerialNum: d.serial] };
        // SAFETY: `NSString *`, retained by the descriptor.
        let () = unsafe { msg_send![&*descriptor, setName: &*name] };
        // SAFETY: `CGSize`.
        let () = unsafe { msg_send![&*descriptor, setSizeInMillimeters: size] };
        // SAFETY: `unsigned int`.
        let () = unsafe { msg_send![&*descriptor, setMaxPixelsWide: d.max_pixels.0] };
        // SAFETY: `unsigned int`.
        let () = unsafe { msg_send![&*descriptor, setMaxPixelsHigh: d.max_pixels.1] };
        // SAFETY: a dispatch queue object; the main queue lives forever.
        let () = unsafe { msg_send![&*descriptor, setDispatchQueue: DispatchQueue::main()] };
        Ok(descriptor)
    }

    /// A `CGVirtualDisplaySettings` offering exactly `mode`. Building one creates no display.
    pub(crate) fn settings(self, mode: &Mode) -> Result<Retained<AnyObject>, DisplayError> {
        // SAFETY: `alloc` on a class returns an uninitialised instance for `init…` to consume.
        let allocated: Allocated<AnyObject> = unsafe { msg_send![self.mode, alloc] };
        let refresh = f64::from(mode.refresh_hz);
        // With hiDPI the mode is given in points; macOS backs it at twice that.
        // SAFETY: `resolve_from` checked the selector; the header takes `unsigned int`,
        // `unsigned int`, `double` and returns the instance or nil.
        let virtual_mode: Option<Retained<AnyObject>> = unsafe {
            msg_send![allocated, initWithWidth: mode.points.0, height: mode.points.1, refreshRate: refresh]
        };
        let virtual_mode = virtual_mode.ok_or(DisplayError::Refused)?;
        // SAFETY: `new` on an `NSObject` subclass returns a +1 instance or nil.
        let settings: Option<Retained<AnyObject>> = unsafe { msg_send![self.settings, new] };
        let settings = settings.ok_or(DisplayError::Refused)?;
        let modes = NSArray::from_retained_slice(&[virtual_mode]);
        // SAFETY: checked by `resolve_from`; takes `unsigned int`.
        let () = unsafe { msg_send![&*settings, setHiDPI: u32::from(mode.hidpi)] };
        // SAFETY: checked by `resolve_from`; takes an `NSArray` of `CGVirtualDisplayMode`.
        let () = unsafe { msg_send![&*settings, setModes: &*modes] };
        Ok(settings)
    }
}

/// Whether this Mac has every class and selector a virtual display needs. Creates nothing.
#[must_use]
pub fn available() -> bool {
    Classes::resolve().is_ok()
}

/// Whether `display` is one an onicode host made for a client (its vendor is [`VENDOR_ID`]):
/// what a client sees rather than a screen someone sits at, which the curtain leaves uncovered.
///
/// [`VENDOR_ID`]: crate::VENDOR_ID
#[must_use]
pub fn made_for_a_client(display: u32) -> bool {
    objc2_core_graphics::CGDisplayVendorNumber(display) == crate::VENDOR_ID
}

/// A virtual display, alive as long as this value.
///
/// Created on the main thread (`initWithDescriptor:` returns nil anywhere else) and, holding a
/// main-thread object, never leaves it. Dropping it releases the `CGVirtualDisplay`, which is
/// what removes the display; macOS then puts the remaining displays back as it stored them.
#[derive(Debug)]
pub struct VirtualDisplay {
    display: Retained<AnyObject>,
    classes: Classes,
    id: CGDirectDisplayID,
    max_pixels: (u32, u32),
    mode: Mode,
}

impl VirtualDisplay {
    /// Create the display `plan` describes and offer it its mode. Call [`Self::enforce`] until
    /// it settles: macOS picks its own mode first.
    ///
    /// # Errors
    ///
    /// [`DisplayError::Unavailable`] without the private classes, [`DisplayError::NotMainThread`]
    /// off the main thread, [`DisplayError::Refused`] or [`DisplayError::Rejected`] when
    /// CoreGraphics declines.
    pub fn create(plan: &Plan) -> Result<Self, DisplayError> {
        let classes = Classes::resolve()?;
        MainThreadMarker::new().ok_or(DisplayError::NotMainThread)?;
        let descriptor = classes.descriptor(plan)?;
        // SAFETY: `alloc` on a class returns an uninitialised instance for `init…` to consume.
        let allocated: Allocated<AnyObject> = unsafe { msg_send![classes.display, alloc] };
        // SAFETY: `resolve_from` checked the selector; it takes the descriptor built above and
        // returns the display or nil. We are on the main thread, as the class requires.
        let display: Option<Retained<AnyObject>> =
            unsafe { msg_send![allocated, initWithDescriptor: &*descriptor] };
        let display = display.ok_or(DisplayError::Refused)?;
        // SAFETY: `resolve_from` checked the getter; it returns `unsigned int`.
        let id: u32 = unsafe { msg_send![&*display, displayID] };
        if id == kCGNullDirectDisplay {
            return Err(DisplayError::Refused);
        }
        let mut created = Self {
            display,
            classes,
            id,
            max_pixels: plan.descriptor.max_pixels,
            mode: plan.mode,
        };
        created.resize(plan)?;
        Ok(created)
    }

    /// Change the mode in place (a new size, a rotation, another refresh), keeping the display
    /// and so every window on it. Call [`Self::enforce`] afterwards.
    ///
    /// # Errors
    ///
    /// [`DisplayError::Outgrown`] when the mode is larger than the display was created for (make a
    /// new one), [`DisplayError::Rejected`] when CoreGraphics declines the settings.
    pub fn resize(&mut self, plan: &Plan) -> Result<(), DisplayError> {
        if !plan.mode.fits(self.max_pixels) {
            return Err(DisplayError::Outgrown {
                wanted: plan.mode.pixels,
                max: self.max_pixels,
            });
        }
        let settings = self.classes.settings(&plan.mode)?;
        // SAFETY: `resolve_from` checked the selector; it takes a `CGVirtualDisplaySettings`
        // and returns `_Bool`. `self` never leaves the main thread.
        let applied: bool = unsafe { msg_send![&*self.display, applySettings: &*settings] };
        if !applied {
            return Err(DisplayError::Rejected);
        }
        self.mode = plan.mode;
        Ok(())
    }

    /// The display's `CGDirectDisplayID`, for capture and input.
    #[must_use]
    pub const fn display_id(&self) -> u32 {
        self.id
    }

    /// The mode last applied.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// Put the display in its mode and out of any mirror set, if macOS moved it.
    ///
    /// macOS gives a new display a default mode, can restore a saved one seconds later, and may
    /// mirror it, so the caller calls this when the display comes online, on every display
    /// reconfiguration, and after [`Self::resize`].
    ///
    /// # Errors
    ///
    /// [`DisplayError::Configure`] when the configuration transaction fails.
    pub fn enforce(&self) -> Result<Enforced, DisplayError> {
        if !CGDisplayIsOnline(self.id) {
            return Ok(Enforced::Pending);
        }
        let current = CGDisplayCopyDisplayMode(self.id);
        let in_mode = current.as_deref().is_some_and(|m| self.is_ours(m));
        let mirroring = CGDisplayMirrorsDisplay(self.id) != kCGNullDirectDisplay;
        if in_mode && !mirroring {
            return Ok(Enforced::Settled);
        }
        let target = if in_mode {
            None
        } else {
            match self.listed_mode() {
                Some(target) => Some(target),
                None => return Ok(Enforced::Pending),
            }
        };
        let mut config: CGDisplayConfigRef = core::ptr::null_mut();
        // SAFETY: `config` is a valid out pointer.
        check(unsafe { CGBeginDisplayConfiguration(&raw mut config) })?;
        if let Err(error) = self.configure(config, target.as_deref(), mirroring) {
            // SAFETY: `config` came from `CGBeginDisplayConfiguration` and was not completed.
            let _: CGError = unsafe { CGCancelDisplayConfiguration(config) };
            return Err(error);
        }
        // SAFETY: `config` came from `CGBeginDisplayConfiguration` and is consumed here.
        check(unsafe { CGCompleteDisplayConfiguration(config, CGConfigureOption::ForSession) })?;
        Ok(Enforced::Applied)
    }

    fn configure(
        &self,
        config: CGDisplayConfigRef,
        target: Option<&CGDisplayMode>,
        mirroring: bool,
    ) -> Result<(), DisplayError> {
        if let Some(target) = target {
            // SAFETY: `config` is an open transaction; no options.
            check(unsafe {
                CGConfigureDisplayWithDisplayMode(config, self.id, Some(target), None)
            })?;
        }
        if mirroring {
            // SAFETY: `config` is an open transaction; mirroring "of" `kCGNullDirectDisplay` is
            // how CoreGraphics documents turning mirroring off.
            check(unsafe {
                CGConfigureDisplayMirrorOfDisplay(config, self.id, kCGNullDirectDisplay)
            })?;
        }
        Ok(())
    }

    fn is_ours(&self, mode: &CGDisplayMode) -> bool {
        let mode = Some(mode);
        self.mode.matches(
            (CGDisplayMode::width(mode), CGDisplayMode::height(mode)),
            (
                CGDisplayMode::pixel_width(mode),
                CGDisplayMode::pixel_height(mode),
            ),
            CGDisplayMode::refresh_rate(mode),
        )
    }

    /// Our mode among those macOS lists, the duplicate low-resolution ones included (a 2× mode
    /// and its 1× twin share a point size).
    fn listed_mode(&self) -> Option<CFRetained<CGDisplayMode>> {
        // SAFETY: framework-provided constant string.
        let key: &CFString =
            unsafe { objc2_core_graphics::kCGDisplayShowDuplicateLowResolutionModes };
        let options = CFDictionary::from_slices(&[key], &[CFBoolean::new(true)]);
        // SAFETY: the options dictionary maps a `kCGDisplay…` key to a `CFBoolean`, as the
        // function documents.
        let modes = unsafe { CGDisplayCopyAllDisplayModes(self.id, Some(options.as_opaque())) }?;
        // SAFETY: CoreGraphics documents the array's elements as `CGDisplayModeRef`s.
        let modes: CFRetained<CFArray<CGDisplayMode>> =
            unsafe { CFRetained::cast_unchecked(modes) };
        modes.iter().find(|m| self.is_ours(m))
    }
}

fn check(error: CGError) -> Result<(), DisplayError> {
    if error == CGError::Success {
        Ok(())
    } else {
        Err(DisplayError::Configure(error.0))
    }
}
