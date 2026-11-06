//! The sizing policy: what a client asks for, and the display and mode that answer it.
//!
//! Pure arithmetic, so it runs and is tested on every platform.

/// The display's name in System Settings.
pub const NAME: &str = "Onicode";

/// EDID's packed manufacturer letters for "ONI" (five bits each, `A` = 1). macOS files a
/// display's arrangement, mode and mirror choice under vendor, product and serial.
pub const VENDOR_ID: u32 = (15 << 10) | (14 << 5) | 9;

/// The longest side a mode or the descriptor may have, in pixels: 8K.
pub const MAX_SIDE_PIXELS: u32 = 7680;

/// The shortest side a mode may have, in points: a floor so a tiny tile never asks macOS for a
/// desktop it cannot lay out.
pub const MIN_SIDE_POINTS: u32 = 480;

/// The refresh a request without one gets.
pub const DEFAULT_REFRESH_HZ: u32 = 60;

/// The refresh range a mode is clamped into.
pub const REFRESH_HZ: core::ops::RangeInclusive<u32> = 30..=120;

/// Apple's desktop densities: 1× at 109 PPI (27" 2560×1440) and 2× at 218 PPI (27" 5K). Sizing
/// the panel at these makes macOS treat the one mode offered as the native one.
const STANDARD_PPI: f64 = 109.0;
const RETINA_PPI: f64 = 218.0;

const MM_PER_INCH: f64 = 25.4;

/// The descriptor's maximum is fixed at creation, so it leaves room for the tile to grow and
/// the client to rotate without recreating the display (which redistributes every window).
const HEADROOM_NUMERATOR: u32 = 5;
const HEADROOM_DENOMINATOR: u32 = 4;
const MAX_ALIGN: u32 = 64;

/// A client's stable identity, from which its display's product and serial come.
///
/// Built from bytes that survive app restarts (a device identity, not a connection's), so macOS
/// finds the arrangement and mode it stored for this client the last time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ClientKey(u64);

impl ClientKey {
    /// The key for a client identified by `id` (FNV-1a, so it is the same on every build).
    #[must_use]
    pub fn new(id: &[u8]) -> Self {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0100_0000_01b3;
        Self(id.iter().fold(OFFSET, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
        }))
    }

    /// The EDID product code: 16 bits.
    #[must_use]
    pub fn product_id(self) -> u32 {
        u32::try_from(self.0 >> 48).unwrap_or_default()
    }

    /// The serial number, never 0 (EDID's "no serial").
    #[must_use]
    pub fn serial(self) -> u32 {
        u32::try_from(self.0 & u64::from(u32::MAX))
            .unwrap_or_default()
            .max(1)
    }
}

/// What a client asks for: its drawable size in pixels, its backing scale and the fastest
/// refresh it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Request {
    /// Width and height in pixels.
    pub pixels: (u32, u32),
    /// The client's backing scale factor (1 for a standard display, 2 for Retina, 3 for an
    /// iPhone).
    pub scale: f64,
    /// The client's maximum refresh, in hertz; 0 for "don't know".
    pub refresh_hz: u32,
    /// Who is asking.
    pub client: ClientKey,
}

/// The fixed part of a display, set once when it is created.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Descriptor {
    /// Always [`VENDOR_ID`].
    pub vendor_id: u32,
    /// From the client key.
    pub product_id: u32,
    /// From the client key.
    pub serial: u32,
    /// The largest mode the display will ever take, in pixels (both sides, so it rotates).
    pub max_pixels: (u32, u32),
    /// The panel's physical size.
    pub size_mm: (f64, f64),
}

/// The mode the display runs in; changed in place by a resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    /// The size macOS lays the desktop out in.
    pub points: (u32, u32),
    /// The size of the pictures captured from it: `points` × 2 when `hidpi`, else `points`.
    pub pixels: (u32, u32),
    /// Backed at 2× (a Retina mode).
    pub hidpi: bool,
    /// The refresh rate, in hertz.
    pub refresh_hz: u32,
}

impl Mode {
    /// Whether this mode fits a display created with `max_pixels`.
    #[must_use]
    pub const fn fits(&self, max_pixels: (u32, u32)) -> bool {
        self.pixels.0 <= max_pixels.0 && self.pixels.1 <= max_pixels.1
    }

    /// Whether a display mode macOS lists is this one. A refresh of 0 is how CoreGraphics
    /// reports a display that does not say.
    #[must_use]
    pub fn matches(&self, points: (usize, usize), pixels: (usize, usize), refresh_hz: f64) -> bool {
        let same = |ours: (u32, u32), theirs: (usize, usize)| {
            usize::try_from(ours.0).is_ok_and(|w| w == theirs.0)
                && usize::try_from(ours.1).is_ok_and(|h| h == theirs.1)
        };
        same(self.points, points)
            && same(self.pixels, pixels)
            && (refresh_hz == 0.0 || (refresh_hz - f64::from(self.refresh_hz)).abs() < 0.5)
    }
}

/// The display and mode that answer a request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    /// Set at creation.
    pub descriptor: Descriptor,
    /// Applied at creation and on every resize.
    pub mode: Mode,
}

/// Size a virtual display for `request`.
///
/// A scale of 2 or more gets a 2× (hiDPI) mode, the only Retina factor macOS draws at; the
/// desktop is then laid out in half the client's pixels. Sides are clamped to
/// [`MIN_SIDE_POINTS`] and, keeping the aspect, to [`MAX_SIDE_PIXELS`]. A 1× mode's sides are
/// rounded down to even, as the 4:2:0 encoder wants; a 2× mode's always are.
#[must_use]
pub fn plan(request: &Request) -> Plan {
    let hidpi = request.scale.is_finite() && request.scale >= 2.0;
    let backing: u32 = if hidpi { 2 } else { 1 };
    let max_points = MAX_SIDE_PIXELS
        .checked_div(backing)
        .unwrap_or(MAX_SIDE_PIXELS);
    let asked = (
        request.pixels.0.checked_div(backing).unwrap_or(0),
        request.pixels.1.checked_div(backing).unwrap_or(0),
    );
    let fitted = fit_within(asked, max_points);
    let mut points = (fitted.0.max(MIN_SIDE_POINTS), fitted.1.max(MIN_SIDE_POINTS));
    if !hidpi {
        points = (points.0 & !1, points.1 & !1);
    }
    let pixels = (
        points.0.saturating_mul(backing),
        points.1.saturating_mul(backing),
    );
    let refresh_hz = match request.refresh_hz {
        0 => DEFAULT_REFRESH_HZ,
        hz => hz.clamp(*REFRESH_HZ.start(), *REFRESH_HZ.end()),
    };
    let ppi = if hidpi { RETINA_PPI } else { STANDARD_PPI };
    let mm = |px: u32| f64::from(px) * MM_PER_INCH / ppi;
    let side = with_headroom(pixels.0.max(pixels.1));
    Plan {
        descriptor: Descriptor {
            vendor_id: VENDOR_ID,
            product_id: request.client.product_id(),
            serial: request.client.serial(),
            max_pixels: (side, side),
            size_mm: (mm(pixels.0), mm(pixels.1)),
        },
        mode: Mode {
            points,
            pixels,
            hidpi,
            refresh_hz,
        },
    }
}

/// Scale `size` down, keeping its aspect, until neither side exceeds `max`.
fn fit_within(size: (u32, u32), max: u32) -> (u32, u32) {
    let longest = size.0.max(size.1);
    if longest <= max {
        return size;
    }
    let scaled = |side: u32| {
        let side = u64::from(side)
            .saturating_mul(u64::from(max))
            .checked_div(u64::from(longest));
        side.and_then(|s| u32::try_from(s).ok()).unwrap_or(max)
    };
    (scaled(size.0), scaled(size.1))
}

/// The descriptor's side for a mode whose longest side is `longest` pixels.
fn with_headroom(longest: u32) -> u32 {
    let grown = longest
        .saturating_mul(HEADROOM_NUMERATOR)
        .div_ceil(HEADROOM_DENOMINATOR)
        .next_multiple_of(MAX_ALIGN);
    grown.clamp(longest, MAX_SIDE_PIXELS.max(longest))
}

#[cfg(test)]
mod tests;
