//! The pointer's picture on the worker: which cursor the window server is showing, as
//! premultiplied BGRA pixels with the hotspot, for a client to show as its own pointer.
//!
//! Two window-server calls, private and looked up at run time (`decisions/input.md`, "Private
//! input APIs"). `CGSCurrentCursorSeed` is a counter the window server moves whenever the
//! cursor on screen changes, whichever process set it. Reading it costs nanoseconds and no
//! round trip, so a loop can poll it at 120 Hz and ask for the picture
//! (`CGSGetGlobalCursorData`, a round trip of about 100 µs) only when it moved. The picture is
//! the one the window server draws, at the scale of the display it draws it on.
//!
//! Both calls are exported by CoreGraphics (re-exported from SkyLight) on every macOS onicode
//! runs on, and a per-version test pins them. On a macOS that dropped one, the worker sends no
//! picture and the client shows its own arrow; there is no second reading to fall back on.
//! `NSCursor.currentSystemCursor`, the public reading this replaced, is deprecated and "will
//! always be `nil` in a future version" (`AppKit/NSCursor.h`).

use std::ffi::{CStr, c_int, c_void};
use std::sync::LazyLock;

use objc2_core_foundation::{CGPoint, CGRect};
use objc2_core_graphics::CGError;
use onicode_desktop_proto::screen::CursorShape;

/// `CGSConnectionID CGSMainConnectionID(void)`: this process's connection to the window
/// server, made on the first call. SkyLight export, re-exported by CoreGraphics; the signature
/// is the one `yabai` and `OSXvnc` declare.
type MainConnection = unsafe extern "C-unwind" fn() -> c_int;
/// `int CGSCurrentCursorSeed(void)`: moves whenever the cursor on screen changes. Reads the
/// window server's shared memory, no round trip. As `RustDesk` declares it.
type CurrentSeed = unsafe extern "C-unwind" fn() -> c_int;
/// `CGError CGSGetGlobalCursorDataSize(CGSConnectionID, int *size)`, as `OSXvnc` declares it.
type GlobalDataSize = unsafe extern "C-unwind" fn(c_int, *mut c_int) -> CGError;
/// `CGError CGSGetGlobalCursorData(CGSConnectionID, unsigned char *data, int *size,
/// int *rowBytes, CGRect *rect, CGPoint *hotspot, int *depth, int *components,
/// int *bitsPerComponent)`, as `OSXvnc` (`mousecursor.c`) declares it. `rect` and `hotspot` are
/// in points, the pixels are rows of `rowBytes` at the display's scale.
type GlobalData = unsafe extern "C-unwind" fn(
    c_int,
    *mut u8,
    *mut c_int,
    *mut c_int,
    *mut CGRect,
    *mut CGPoint,
    *mut c_int,
    *mut c_int,
    *mut c_int,
) -> CGError;

/// The window server's cursor calls.
struct Calls {
    main_connection: MainConnection,
    seed: CurrentSeed,
    data_size: GlobalDataSize,
    data: GlobalData,
}

fn symbol(name: &CStr) -> Option<*mut c_void> {
    // SAFETY: `dlsym(3)` with `RTLD_DEFAULT` searches every loaded image (CoreGraphics, which
    // this crate links, among them) for a NUL-terminated name.
    let found = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!found.is_null()).then_some(found)
}

static CALLS: LazyLock<Option<Calls>> = LazyLock::new(|| {
    let found = (|| {
        let (main_connection, seed, data_size, data) = (
            symbol(c"CGSMainConnectionID")?,
            symbol(c"CGSCurrentCursorSeed")?,
            symbol(c"CGSGetGlobalCursorDataSize")?,
            symbol(c"CGSGetGlobalCursorData")?,
        );
        // SAFETY: CoreGraphics exports `CGSMainConnectionID` with the signature `MainConnection`
        // names.
        let main_connection =
            unsafe { std::mem::transmute::<*mut c_void, MainConnection>(main_connection) };
        // SAFETY: as above, `CGSCurrentCursorSeed` with `CurrentSeed`'s.
        let seed = unsafe { std::mem::transmute::<*mut c_void, CurrentSeed>(seed) };
        // SAFETY: as above, `CGSGetGlobalCursorDataSize` with `GlobalDataSize`'s.
        let data_size = unsafe { std::mem::transmute::<*mut c_void, GlobalDataSize>(data_size) };
        // SAFETY: as above, `CGSGetGlobalCursorData` with `GlobalData`'s.
        let data = unsafe { std::mem::transmute::<*mut c_void, GlobalData>(data) };
        Some(Calls {
            main_connection,
            seed,
            data_size,
            data,
        })
    })();
    if found.is_none() {
        tracing::warn!("no window-server cursor calls: clients show their own arrow");
    }
    found
});

/// Serialises picture reads: the connection is the process's one, shared by every stream's
/// loop, and nothing says it takes two requests at once. A read is about 100 µs.
static READING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// The window server's cursor seed, which moves whenever the cursor on screen changes; `None`
/// without the call. Tens of nanoseconds, no round trip.
#[must_use]
pub fn cursor_seed() -> Option<i32> {
    let calls = CALLS.as_ref()?;
    // SAFETY: a plain call with no arguments, as `CurrentSeed` names it.
    Some(unsafe { (calls.seed)() })
}

/// The cursor on screen right now, or `None` when the window server does not say, or its
/// picture is in a layout this reader does not take. A window-server round trip.
///
/// The picture is at the scale of the display the cursor is on: 2 on a Retina display, 1 on
/// a 1080p one. `CursorShape::scale` says which, so a client shows it at the same size in
/// points whatever its own display.
#[must_use]
pub fn read_cursor() -> Option<CursorShape> {
    let calls = CALLS.as_ref()?;
    let one_at_a_time = READING.lock();
    // SAFETY: a plain call with no arguments, as `MainConnection` names it.
    let connection = unsafe { (calls.main_connection)() };
    let mut size: c_int = 0;
    // SAFETY: `size` is a live `int` for the call to write, as `GlobalDataSize` names it.
    if unsafe { (calls.data_size)(connection, &raw mut size) } != CGError::Success {
        return None;
    }
    let mut data = vec![0_u8; usize::try_from(size).ok()?];
    let (mut row_bytes, mut depth, mut components, mut bits) = (0, 0, 0, 0);
    let mut rect = CGRect::default();
    let mut hotspot = CGPoint::default();
    // SAFETY: every pointer is to a live local of the type `GlobalData` names; `data` holds
    // `size` bytes, which the call takes as the buffer's length and never writes past.
    let read = unsafe {
        (calls.data)(
            connection,
            data.as_mut_ptr(),
            &raw mut size,
            &raw mut row_bytes,
            &raw mut rect,
            &raw mut hotspot,
            &raw mut depth,
            &raw mut components,
            &raw mut bits,
        )
    };
    drop(one_at_a_time);
    if read != CGError::Success || (depth, components, bits) != (32, 4, 8) {
        return None;
    }
    data.truncate(usize::try_from(size).ok()?);
    shape_of(&data, usize::try_from(row_bytes).ok()?, rect, hotspot)
}

/// The picture the global cursor data describes: `data` is rows of `row_bytes`, as many as
/// fill it, drawing `rect` (points) at the display's scale, with the hotspot in points.
fn shape_of(data: &[u8], row_bytes: usize, rect: CGRect, hotspot: CGPoint) -> Option<CursorShape> {
    if rect.size.height <= 0.0 || rect.size.width <= 0.0 {
        return None;
    }
    let rows = data.len().checked_div(row_bytes)?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "a cursor is a few hundred pixels"
    )]
    let ratio = rows as f64 / rect.size.height;
    #[expect(clippy::cast_possible_truncation, reason = "clamped to a byte")]
    #[expect(clippy::cast_sign_loss, reason = "clamped to at least 1")]
    let scale = ratio.round().clamp(1.0, 4.0) as u8;
    let to_px = |v: f64| -> Option<u16> {
        let p = (v * f64::from(scale)).round();
        #[expect(clippy::cast_possible_truncation, reason = "range-checked")]
        #[expect(clippy::cast_sign_loss, reason = "range-checked")]
        let p = (0.0..=f64::from(u16::MAX)).contains(&p).then_some(p as u16);
        p
    };
    let layout = Layout {
        width: to_px(rect.size.width)?,
        height: u16::try_from(rows).ok()?,
        bytes_per_row: row_bytes,
        // A 32-bit ARGB word in host order (`OSXvnc` reads blue at shift 0, alpha at 24).
        little_endian: true,
        alpha: AlphaAt::First,
        premultiplied: true,
        opaque: false,
    };
    let (hot_x, hot_y) = (to_px(hotspot.x)?, to_px(hotspot.y)?);
    if hot_x >= layout.width || hot_y >= layout.height {
        return None;
    }
    Some(CursorShape {
        w: layout.width,
        h: layout.height,
        hot_x,
        hot_y,
        bgra: bgra_premultiplied(&layout, data)?,
        scale,
    })
}

/// Follows the cursor by its seed, reading the picture only when the seed moved.
///
/// One per loop. [`CursorWatch::poll`] costs a seed read when nothing changed, so it can run
/// at the display's rate: a shape change is seen within one poll, where a 33 ms read of the
/// whole picture took up to 33 ms and a round trip every time.
#[derive(Clone, Copy, Debug, Default)]
pub struct CursorWatch {
    seed: Option<i32>,
}

impl CursorWatch {
    /// A watch that reads the picture on its first poll.
    #[must_use]
    pub const fn new() -> Self {
        Self { seed: None }
    }

    /// The cursor's picture when it changed since the last poll (always on the first), else
    /// `None`. A read that fails is not retried until the cursor changes again.
    pub fn poll(&mut self) -> Option<CursorShape> {
        let seed = cursor_seed()?;
        if self.seed == Some(seed) {
            return None;
        }
        // The seed is taken before the picture: a change between the two is read again at the
        // next poll, never missed.
        self.seed = Some(seed);
        read_cursor()
    }
}

/// Connect to the window server and read the cursor once, so the first read a stream makes is
/// not the one that pays for the connection (about 45 ms). Returns how long it took.
#[must_use]
pub fn warm_cursor() -> std::time::Duration {
    let started = std::time::Instant::now();
    let _shape = read_cursor();
    started.elapsed()
}

/// Where the alpha byte sits in a pixel's four bytes as CoreGraphics describes it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlphaAt {
    /// The alpha component comes first in the pixel's logical order (`ARGB`).
    First,
    /// The alpha component comes last (`RGBA`).
    Last,
}

/// How a 32-bit picture's bytes are laid out, enough to read every pixel.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    /// Pixels across.
    pub width: u16,
    /// Pixels down.
    pub height: u16,
    /// Bytes from one row to the next (at least `width * 4`).
    pub bytes_per_row: usize,
    /// The bytes of a pixel are the logical components reversed (`kCGImageByteOrder32Little`).
    pub little_endian: bool,
    /// Where alpha sits in the logical order.
    pub alpha: AlphaAt,
    /// Colour is already multiplied by alpha.
    pub premultiplied: bool,
    /// The alpha byte is padding: the pixel is opaque.
    pub opaque: bool,
}

impl Layout {
    /// Byte offsets of blue, green, red and alpha within a pixel.
    const fn offsets(self) -> [usize; 4] {
        // Logical order is RGBA or ARGB; a little-endian picture stores it reversed.
        match (self.alpha, self.little_endian) {
            (AlphaAt::Last, false) => [2, 1, 0, 3],
            (AlphaAt::First, false) => [3, 2, 1, 0],
            (AlphaAt::Last, true) => [1, 2, 3, 0],
            (AlphaAt::First, true) => [0, 1, 2, 3],
        }
    }
}

/// `data` as tightly packed premultiplied BGRA rows, or `None` when it is shorter than the
/// layout says.
#[must_use]
pub fn bgra_premultiplied(layout: &Layout, data: &[u8]) -> Option<Vec<u8>> {
    let (width, height) = (usize::from(layout.width), usize::from(layout.height));
    if layout.bytes_per_row < width.checked_mul(4)? {
        return None;
    }
    let needed = layout.bytes_per_row.checked_mul(height)?;
    if data.len() < needed {
        return None;
    }
    let [ob, og, or, oa] = layout.offsets();
    let mut out = Vec::with_capacity(width.checked_mul(height)?.checked_mul(4)?);
    for row in 0..height {
        let start = row.checked_mul(layout.bytes_per_row)?;
        let cells = data.get(start..start.checked_add(width.checked_mul(4)?)?)?;
        for px in cells.as_chunks::<4>().0 {
            let at = |o: usize| px.get(o).copied().unwrap_or(0);
            let (b, g, r) = (at(ob), at(og), at(or));
            let a = if layout.opaque { 255 } else { at(oa) };
            let (b, g, r) = if layout.premultiplied || layout.opaque {
                (b, g, r)
            } else {
                (premultiply(b, a), premultiply(g, a), premultiply(r, a))
            };
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    Some(out)
}

/// `c × a / 255`, rounded.
const fn premultiply(c: u8, a: u8) -> u8 {
    let p = (c as u32)
        .wrapping_mul(a as u32)
        .wrapping_add(127)
        .wrapping_div(255);
    #[expect(
        clippy::cast_possible_truncation,
        reason = "at most 255 by construction"
    )]
    let p = p as u8;
    p
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CGSize;
    use objc2_core_graphics::{CGImage, CGImageAlphaInfo, CGImageByteOrderInfo};

    use super::*;

    fn layout(little_endian: bool, alpha: AlphaAt, premultiplied: bool, opaque: bool) -> Layout {
        Layout {
            width: 2,
            height: 1,
            bytes_per_row: 8,
            little_endian,
            alpha,
            premultiplied,
            opaque,
        }
    }

    #[test]
    fn every_byte_order_comes_out_as_bgra() {
        // One pixel (r 10, g 20, b 30, a 255) and one (r 1, g 2, b 3, a 255), premultiplied.
        let want = vec![30, 20, 10, 255, 3, 2, 1, 255];
        let rgba = [10, 20, 30, 255, 1, 2, 3, 255];
        assert_eq!(
            bgra_premultiplied(&layout(false, AlphaAt::Last, true, false), &rgba).unwrap(),
            want
        );
        let argb = [255, 10, 20, 30, 255, 1, 2, 3];
        assert_eq!(
            bgra_premultiplied(&layout(false, AlphaAt::First, true, false), &argb).unwrap(),
            want
        );
        let bgra = [30, 20, 10, 255, 3, 2, 1, 255];
        assert_eq!(
            bgra_premultiplied(&layout(true, AlphaAt::First, true, false), &bgra).unwrap(),
            want,
            "the common macOS layout is a copy"
        );
        let abgr = [255, 30, 20, 10, 255, 3, 2, 1];
        assert_eq!(
            bgra_premultiplied(&layout(true, AlphaAt::Last, true, false), &abgr).unwrap(),
            want
        );
    }

    #[test]
    fn straight_alpha_is_multiplied_in_and_padding_alpha_reads_opaque() {
        let straight = [200, 100, 50, 128, 0, 0, 0, 0];
        let got =
            bgra_premultiplied(&layout(false, AlphaAt::Last, false, false), &straight).unwrap();
        assert_eq!(
            got,
            vec![25, 50, 100, 128, 0, 0, 0, 0],
            "each colour halves with the alpha"
        );
        let padded = [200, 100, 50, 7, 1, 2, 3, 0];
        let got = bgra_premultiplied(&layout(false, AlphaAt::Last, true, true), &padded).unwrap();
        assert_eq!(
            got,
            vec![50, 100, 200, 255, 3, 2, 1, 255],
            "the pad byte is not an alpha"
        );
    }

    #[test]
    fn padded_rows_are_cut_and_short_data_is_refused() {
        let mut l = layout(true, AlphaAt::First, true, false);
        l.bytes_per_row = 12;
        let data = [30, 20, 10, 255, 3, 2, 1, 255, 9, 9, 9, 9];
        assert_eq!(
            bgra_premultiplied(&l, &data).unwrap(),
            vec![30, 20, 10, 255, 3, 2, 1, 255]
        );
        assert_eq!(
            bgra_premultiplied(&l, &data[..11]),
            None,
            "a row short of its stride"
        );
        l.bytes_per_row = 4;
        assert_eq!(
            bgra_premultiplied(&l, &data),
            None,
            "a stride narrower than the row"
        );
    }

    #[test]
    fn premultiply_rounds_to_nearest() {
        assert_eq!(premultiply(255, 255), 255);
        assert_eq!(premultiply(255, 0), 0);
        assert_eq!(premultiply(1, 128), 1, "0.502 rounds up");
        assert_eq!(premultiply(1, 127), 0, "0.498 rounds down");
    }

    fn rect(w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint::default(),
            size: CGSize {
                width: w,
                height: h,
            },
        }
    }

    /// The window server's picture: a 2-point-wide, 1-point-high cursor at 2×, rows padded to
    /// 24 bytes, hotspot at (0.5, 0.5) points.
    #[test]
    fn the_global_data_is_read_at_its_displays_scale_with_the_hotspot_in_pixels() {
        let mut data = Vec::new();
        for row in 0..2_u8 {
            for col in 0..4_u8 {
                data.extend([col, row, 9, 255]);
            }
            data.extend([0xee; 8]);
        }
        let shape = shape_of(&data, 24, rect(2.0, 1.0), CGPoint { x: 0.5, y: 0.5 }).unwrap();
        assert_eq!((shape.w, shape.h, shape.scale), (4, 2, 2), "{shape:?}");
        assert_eq!(
            (shape.hot_x, shape.hot_y),
            (1, 1),
            "half a point is a pixel at 2×"
        );
        assert_eq!(shape.bgra.len(), 32, "4 × 2 pixels: the row padding is cut");
        assert_eq!(
            shape.bgra.get(..8),
            Some(&[0, 0, 9, 255, 1, 0, 9, 255][..]),
            "a copy"
        );
        assert!(!shape.bgra.contains(&0xee));
    }

    #[test]
    fn a_global_picture_that_does_not_add_up_is_refused() {
        let data = vec![0; 16];
        let at = CGPoint::default();
        assert!(
            shape_of(&data, 0, rect(1.0, 1.0), at).is_none(),
            "no row stride"
        );
        assert!(shape_of(&data, 8, rect(0.0, 2.0), at).is_none(), "no width");
        assert!(
            shape_of(&data, 8, rect(4.0, 2.0), at).is_none(),
            "wider than a row"
        );
        let off = CGPoint { x: 2.0, y: 0.0 };
        assert!(
            shape_of(&data, 8, rect(2.0, 2.0), off).is_none(),
            "hotspot off the right"
        );
        let before = CGPoint { x: -1.0, y: 0.0 };
        assert!(
            shape_of(&data, 8, rect(2.0, 2.0), before).is_none(),
            "hotspot before the left"
        );
        assert!(
            shape_of(&data, 8, rect(2.0, 2.0), at).is_some(),
            "the same, well formed"
        );
    }

    /// The per-version pin: every call the reader needs is exported by this macOS. Needs no
    /// window session and posts nothing. Passed on 27.0.1 (26A434).
    #[test]
    fn this_macos_exports_the_window_servers_cursor_calls() {
        assert!(
            CALLS.is_some(),
            "CGSMainConnectionID, CGSCurrentCursorSeed, CGSGetGlobalCursorData(Size)"
        );
        let (a, b) = (cursor_seed(), cursor_seed());
        assert!(a.is_some() && b.is_some());
    }

    fn percentile(mut samples: Vec<Duration>, p: usize) -> Duration {
        samples.sort_unstable();
        let at = samples
            .len()
            .saturating_sub(1)
            .saturating_mul(p)
            .checked_div(100)
            .unwrap_or(0);
        samples.get(at).copied().unwrap_or_default()
    }

    /// The window server has a cursor whenever a session is on screen. This reads it both ways,
    /// by the global data and by the deprecated `currentSystemCursor` it replaced, and checks
    /// they are the same picture pixel for pixel, which pins the global data's layout (order,
    /// premultiplied alpha, hotspot, scale) on this macOS. Then it times the seed and the read
    /// against the old reading. Needs a window session, so it runs only with
    /// `ONICODE_DESKTOP_LIVE=1` like the stream tests. Changes no cursor and posts nothing.
    #[test]
    fn the_global_cursor_is_the_system_cursors_picture_and_its_seed_costs_nanoseconds() {
        if std::env::var_os("ONICODE_DESKTOP_LIVE").is_none() {
            onicode_desktop_testkit::live::skip("set ONICODE_DESKTOP_LIVE=1");
            return;
        }
        let warm = warm_cursor();
        let shape = (0..20)
            .find_map(|_| {
                let seed = cursor_seed();
                let global = read_cursor()?;
                let system = system_cursor(global.scale)?;
                // The cursor may have changed between the two reads; then try again.
                (cursor_seed() == seed).then_some((global, system))
            })
            .expect("a steady cursor read both ways");
        let (global, system) = shape;
        eprintln!(
            "warm-up {warm:?}; {}×{} pixels at {}×, hotspot ({}, {})",
            global.w, global.h, global.scale, global.hot_x, global.hot_y
        );
        assert_eq!(
            (global.w, global.h, global.hot_x, global.hot_y, global.scale),
            (system.w, system.h, system.hot_x, system.hot_y, system.scale),
            "the same size, hotspot and scale"
        );
        let off = global
            .bgra
            .iter()
            .zip(&system.bgra)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            off <= 1,
            "the same pixels in the same order, premultiplied (off by {off})"
        );
        assert!(
            global.bgra.as_chunks::<4>().0.iter().any(|px| px[3] > 0),
            "something is drawn"
        );

        let (mut seeds, mut reads, mut olds) = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..200 {
            let started = Instant::now();
            let _seed = cursor_seed();
            seeds.push(started.elapsed());
            let started = Instant::now();
            let _shape = read_cursor();
            reads.push(started.elapsed());
            let started = Instant::now();
            let _shape = system_cursor(global.scale);
            olds.push(started.elapsed());
        }
        let mut watch = CursorWatch::new();
        assert!(watch.poll().is_some(), "the first poll reads");
        let (mut polls, mut read_again) = (Vec::new(), 0_u32);
        for _ in 0..10_000 {
            let started = Instant::now();
            read_again = read_again.saturating_add(u32::from(watch.poll().is_some()));
            polls.push(started.elapsed());
        }
        for (name, samples) in [
            ("seed", seeds),
            ("global_read", reads),
            ("current_system_cursor", olds),
            ("poll", polls),
        ] {
            eprintln!(
                "MEASURE cursor_{name} p50={:?} p99={:?}",
                percentile(samples.clone(), 50),
                percentile(samples, 99)
            );
        }
        eprintln!("MEASURE cursor_poll_reads={read_again} of 10000 polls");
    }

    /// The deprecated reading the global data replaced, kept here to pin the new one against:
    /// the representation of `NSCursor.currentSystemCursor` at `scale`.
    fn system_cursor(scale: u8) -> Option<CursorShape> {
        use objc2_app_kit::NSCursor;
        #[expect(deprecated, reason = "the reading the global data is checked against")]
        let cursor = NSCursor::currentSystemCursor()?;
        let image = cursor.image();
        let hot = cursor.hotSpot();
        let points = image.size();
        let wanted = (points.width * f64::from(scale)).round();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a cursor is a few hundred pixels"
        )]
        let wanted = wanted as isize;
        let rep = image
            .representations()
            .iter()
            .find(|rep| rep.pixelsWide() == wanted)?;
        // SAFETY: a null proposed rect asks for the whole image at the representation's own
        // pixel size; no context and no hints is the documented way to read its pixels.
        let cg =
            unsafe { rep.CGImageForProposedRect_context_hints(std::ptr::null_mut(), None, None) }?;
        let layout = layout_of(&cg)?;
        let provider = CGImage::data_provider(Some(&cg))?;
        let data = objc2_core_graphics::CGDataProvider::data(Some(&provider))?.to_vec();
        let to_px = |v: f64| -> u16 {
            #[expect(clippy::cast_possible_truncation, reason = "clamped")]
            #[expect(clippy::cast_sign_loss, reason = "clamped to zero")]
            let p = (v * f64::from(scale))
                .round()
                .clamp(0.0, f64::from(u16::MAX)) as u16;
            p
        };
        Some(CursorShape {
            w: layout.width,
            h: layout.height,
            hot_x: to_px(hot.x),
            hot_y: to_px(hot.y),
            bgra: bgra_premultiplied(&layout, &data)?,
            scale,
        })
    }

    /// The layout of a `CGImage`, or `None` for one that is not 8 bits × 4 components.
    fn layout_of(image: &CGImage) -> Option<Layout> {
        let some = Some(image);
        if CGImage::bits_per_pixel(some) != 32 || CGImage::bits_per_component(some) != 8 {
            return None;
        }
        let (alpha, premultiplied, opaque) = match CGImage::alpha_info(some) {
            CGImageAlphaInfo::PremultipliedLast => (AlphaAt::Last, true, false),
            CGImageAlphaInfo::PremultipliedFirst => (AlphaAt::First, true, false),
            CGImageAlphaInfo::Last => (AlphaAt::Last, false, false),
            CGImageAlphaInfo::First => (AlphaAt::First, false, false),
            CGImageAlphaInfo::NoneSkipLast => (AlphaAt::Last, true, true),
            CGImageAlphaInfo::NoneSkipFirst => (AlphaAt::First, true, true),
            _ => return None,
        };
        Some(Layout {
            width: u16::try_from(CGImage::width(some)).ok()?,
            height: u16::try_from(CGImage::height(some)).ok()?,
            bytes_per_row: CGImage::bytes_per_row(some),
            little_endian: CGImage::byte_order_info(some) == CGImageByteOrderInfo::Order32Little,
            alpha,
            premultiplied,
            opaque,
        })
    }
}
