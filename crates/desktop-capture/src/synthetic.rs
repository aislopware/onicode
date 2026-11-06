//! A capture source that draws its own pictures: [`Canvas`].
//!
//! It is the instrument for timing the frame path end to end where ScreenCaptureKit cannot run.
//! A worker a test starts from a shell has no Screen Recording grant, and nothing may ask for
//! one. [`Canvas`] stands in for every active display (CoreGraphics lists them without a grant).
//! On the display's beat, which every canvas at one rate shares, it hands the stream an
//! `IOSurface`-backed full-range picture of the size and format asked for (NV12, or 10-bit 4:4:4
//! for a full-chroma stream), in a surface padded as a capture's is ([`CaptureConfig::surface`],
//! black past the picture), stamped on the host time clock ScreenCaptureKit stamps with, into the
//! same sink a real capture feeds. Nothing in the product names it: only a stream built on it
//! draws.
//!
//! The picture is a dark desktop with a window of text scrolling in its middle, so every frame
//! changes as a scrolling page does. A strip of [`MARK_BITS`] blocks along the top spells how
//! many inputs the process has taken ([`take_input`]), and [`inputs_shown`] reads it back off a
//! decoded picture. That is what times input to glass: the first picture presented whose strip
//! counts an input is the one that shows it.

use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering};

use dispatch2::{DispatchQoS, DispatchQueue, DispatchQueueAttr, DispatchRetained, DispatchTime};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType, Type as _};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
};
use onicode_desktop_codec::PixelBuffer;
use onicode_desktop_proto::screen::{CaptureTarget, CursorShape, DisplayInfo};
use parking_lot::Mutex;

use crate::geometry;
use crate::source::{
    AUDIO_CHANNELS, AUDIO_RATE, AudioSink, CaptureConfig, CaptureError, CaptureSource,
    CapturedAudio, CapturedFrame, Console, Heard, PixelFormat, Rect,
};
use crate::stream::host_now_us;

/// What the drawn Mac's screens show of its session ([`set_console`]), as a [`Console`]'s index.
static CONSOLE: AtomicU8 = AtomicU8::new(0);

/// Lock the drawn Mac, move its session off its screens, or bring it back.
///
/// It is what [`CaptureSource::console`] answers for every canvas in the process from here on.
/// Its pictures go on as they were; a real capture's would not, and nothing may rely on that.
pub fn set_console(console: Console) {
    let index = match console {
        Console::Shown => 0,
        Console::Locked => 1,
        Console::Away => 2,
    };
    CONSOLE.store(index, Ordering::Relaxed);
}

/// Blocks in the input strip: the low bits of the input count it spells.
pub const MARK_BITS: u32 = 16;
/// The desktop behind the window.
const DESKTOP: u8 = 40;
/// A padded surface past the picture: the black a capture's background paints it.
const PADDING: u8 = 0;
/// The window's page and its text.
const PAPER: u8 = 24;
const INK: u8 = 225;
/// A strip block that is a one or a zero, and the level between them a decoded one is read at.
const MARK_ON: u8 = 235;
const MARK_OFF: u8 = 20;
const MARK_THRESHOLD: u8 = 128;
/// Neutral chroma.
const GREY: u8 = 128;
/// Pixel rows the page scrolls a second: three a frame at 60 Hz. The speed is set in time, not
/// per beat, so a faster beat draws the same motion in smaller steps, as an application's own
/// scroll does on a faster display.
const SCROLL_PX_PER_S: u64 = 180;
/// A character cell of the page, in pixels.
const CELL_W: usize = 8;
const CELL_H: usize = 16;
/// Pictures the canvas keeps at most. A picture the stream or the encoder still holds is never
/// drawn over; past this many held at once the beat drops its frame, as a capture whose pool is
/// exhausted does.
const SLOTS: usize = 12;
/// The beat when the display reports no refresh rate.
const FALLBACK_HZ: f64 = 60.0;

/// How a canvas picture lays its samples out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Samples {
    /// Full-range NV12 (`420f`): a byte a sample, chroma at half size.
    Nv12,
    /// Full-range 10-bit bi-planar 4:4:4 (`xf44`): 16 bits a sample, the value in the high 10,
    /// chroma at full size.
    Xf44,
}

impl Samples {
    const fn of(format: PixelFormat) -> Self {
        match format {
            PixelFormat::Yuv444Full10 => Self::Xf44,
            PixelFormat::Nv12Full | PixelFormat::Bgra => Self::Nv12,
        }
    }

    const fn os_type(self) -> u32 {
        match self {
            Self::Nv12 => kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            Self::Xf44 => kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
        }
    }

    fn from_os_type(os_type: u32) -> Option<Self> {
        [Self::Nv12, Self::Xf44]
            .into_iter()
            .find(|samples| samples.os_type() == os_type)
    }

    /// Bytes a sample takes.
    const fn width(self) -> usize {
        match self {
            Self::Nv12 => 1,
            Self::Xf44 => 2,
        }
    }

    /// An 8-bit level as one sample: the byte itself, or the level in the high byte of a
    /// little-endian 16-bit sample (CoreVideo keeps the 10 bits in the high bits).
    const fn put(self, sample: &mut [u8], level: u8) {
        match (self, sample) {
            (Self::Nv12, [byte]) => *byte = level,
            (Self::Xf44, [low, high]) => (*low, *high) = (0, level),
            _other => {}
        }
    }

    /// A sample's 8-bit level: its high byte.
    fn level(self, sample: &[u8]) -> Option<u8> {
        sample.get(self.width().checked_sub(1)?).copied()
    }

    /// Set every sample of `bytes` to `level`.
    fn fill(self, bytes: &mut [u8], level: u8) {
        for sample in bytes.chunks_exact_mut(self.width()) {
            self.put(sample, level);
        }
    }
}

/// Inputs this process has taken, all canvases together.
static INPUTS: AtomicU64 = AtomicU64::new(0);

/// The beat every canvas stands in for, hertz; 0 follows each display's own refresh.
static BEAT_HZ: AtomicU16 = AtomicU16::new(0);

/// Draw every canvas started from now on at `hz`; `None` goes back to the displays' own refresh.
///
/// Every display is listed as refreshing at it too. A measurement sets it to time a 120 Hz
/// source on a Mac whose panels run slower.
pub fn set_beat(hz: Option<u16>) {
    BEAT_HZ.store(hz.unwrap_or(0), Ordering::Release);
}

/// The beat set by [`set_beat`], if any.
fn beat_hz() -> Option<f64> {
    let hz = BEAT_HZ.load(Ordering::Acquire);
    (hz > 0).then(|| f64::from(hz))
}

/// Every canvas's page stands still while set: nothing scrolls, and a canvas delivers a picture
/// only when an input changed it, as ScreenCaptureKit sends nothing for a screen that does not
/// change. A measurement sets it to time what a still screen does between inputs.
static STILL: AtomicBool = AtomicBool::new(false);

/// Stand every canvas's page still (`true`) or let it scroll again.
pub fn set_still(still: bool) {
    STILL.store(still, Ordering::Release);
}

/// Take one input: every picture drawn from now on counts it. Returns the new count.
pub fn take_input() -> u64 {
    INPUTS.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
}

/// Inputs taken so far.
#[must_use]
pub fn inputs_taken() -> u64 {
    INPUTS.load(Ordering::Acquire)
}

/// The input count a canvas picture spells in its strip, modulo `2^MARK_BITS`; `None` when the
/// buffer is too small to carry a strip, is in neither of the canvas's formats, or its planes
/// cannot be read.
#[must_use]
pub fn inputs_shown(buffer: &CVPixelBuffer) -> Option<u16> {
    // SAFETY: CoreVideo rule: the base address may only be read between a lock and its unlock;
    // a read-only lock is what a reader that writes nothing takes.
    let locked = unsafe { CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    if locked != 0 {
        return None;
    }
    let shown = read_strip(buffer);
    // SAFETY: matches the lock above, with the same flags as CoreVideo requires.
    let _unlocked =
        unsafe { CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly) };
    shown
}

/// The strip's bits off a locked buffer.
fn read_strip(buffer: &CVPixelBuffer) -> Option<u16> {
    let samples = Samples::from_os_type(CVPixelBufferGetPixelFormatType(buffer))?;
    let width = CVPixelBufferGetWidthOfPlane(buffer, 0);
    let height = CVPixelBufferGetHeightOfPlane(buffer, 0);
    let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
    let base = NonNull::new(CVPixelBufferGetBaseAddressOfPlane(buffer, 0).cast::<u8>())?;
    let layout = Layout::new(width, height)?;
    let row = layout.strip_h.checked_div(2)?;
    let len = stride.checked_mul(height)?;
    // SAFETY: the buffer is locked, so its luma plane is mapped for `stride * height` bytes from
    // its base address and nothing writes it while the lock is held.
    let plane = unsafe { std::slice::from_raw_parts(base.as_ptr(), len) };
    let line = plane.chunks_exact(stride).nth(row)?;
    let mut shown = 0_u16;
    for bit in 0..MARK_BITS {
        let x = layout.mark_centre(bit)?;
        let at = x.checked_mul(samples.width())?;
        if samples.level(line.get(at..at.checked_add(samples.width())?)?)? > MARK_THRESHOLD {
            shown |= 1_u16.checked_shl(bit)?;
        }
    }
    Some(shown)
}

/// Where things are on a picture of one size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    width: usize,
    height: usize,
    /// Rows of the input strip, from the top.
    strip_h: usize,
    /// Width of one strip block.
    mark_w: usize,
    /// The scrolling window: left edge, top edge, width, height.
    page: (usize, usize, usize, usize),
}

impl Layout {
    fn new(width: usize, height: usize) -> Option<Self> {
        let strip_h = height.checked_div(24)?.max(8);
        let mark_w = width.checked_div(usize::try_from(MARK_BITS).ok()?)?;
        if mark_w == 0 || strip_h >= height {
            return None;
        }
        let page_w = width.checked_div(2)?;
        let page_h = height.checked_div(2)?;
        let page = (
            width.checked_div(4)?,
            height.checked_div(4)?.max(strip_h),
            page_w,
            page_h,
        );
        Some(Self {
            width,
            height,
            strip_h,
            mark_w,
            page,
        })
    }

    /// The column at the middle of strip block `bit`.
    fn mark_centre(&self, bit: u32) -> Option<usize> {
        let index = usize::try_from(bit).ok()?;
        self.mark_w
            .checked_mul(index)?
            .checked_add(self.mark_w.checked_div(2)?)
    }
}

/// A picture the canvas draws into, and whether its unchanging parts are drawn yet.
struct Slot {
    buffer: PixelBuffer,
    /// Desktop and chroma are drawn; a new buffer starts with neither.
    ready: bool,
}

/// What the beat draws with.
struct Painter {
    layout: Layout,
    /// The size of each picture's surface, the layout at its top-left.
    surface: (usize, usize),
    samples: Samples,
    /// The page's text, twice the window's height, scrolled through at [`SCROLL_PX_PER_S`].
    text: Vec<u8>,
    slots: Vec<Slot>,
    /// When the first picture was drawn, host microseconds: the scroll counts from it.
    origin_us: Option<u64>,
    /// Pixel rows the page is scrolled by in the picture being drawn.
    scrolled: usize,
}

impl Painter {
    fn new(config: &CaptureConfig) -> Option<Self> {
        let size = |side: u32| usize::try_from(side).ok();
        let layout = Layout::new(size(config.width)?, size(config.height)?)?;
        let (surface_w, surface_h) = config.surface();
        let (_, _, page_w, page_h) = layout.page;
        Some(Self {
            layout,
            surface: (size(surface_w)?, size(surface_h)?),
            samples: Samples::of(config.format),
            text: text(page_w, page_h.checked_mul(2)?),
            slots: Vec::new(),
            origin_us: None,
            scrolled: 0,
        })
    }

    /// Draw the picture shown at `at_us` (host microseconds), counting `inputs`; `None` when
    /// every picture is still held.
    fn paint(&mut self, inputs: u64, at_us: u64) -> Option<PixelBuffer> {
        let origin = *self.origin_us.get_or_insert(at_us);
        let elapsed = at_us.saturating_sub(origin);
        if !STILL.load(Ordering::Acquire) {
            self.scrolled =
                usize::try_from(elapsed.saturating_mul(SCROLL_PX_PER_S) / 1_000_000).unwrap_or(0);
        }
        let slot = self.free_slot()?;
        let slot = self.slots.get_mut(slot)?;
        let buffer = slot.buffer.as_cv().retain();
        // SAFETY: CoreVideo rule: the planes are written only between a lock and its unlock.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        if locked != 0 {
            return None;
        }
        if !slot.ready {
            fill_luma(&buffer, self.samples, &self.layout);
            fill_plane(&buffer, self.samples, 1, GREY);
            slot.ready = true;
        }
        let drawn = draw(&buffer, self, inputs);
        // SAFETY: matches the lock above.
        let _unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        drawn.then(|| PixelBuffer::from_retained(buffer))
    }

    /// A picture nobody else holds, made when there is none and room for one.
    fn free_slot(&mut self) -> Option<usize> {
        // A reference beyond the canvas's own is the stream's held frame or the encoder's.
        if let Some(free) = self
            .slots
            .iter()
            .position(|s| s.buffer.as_cv().retain_count() == 1)
        {
            return Some(free);
        }
        if self.slots.len() >= SLOTS {
            return None;
        }
        let (width, height) = self.surface;
        let buffer = PixelBuffer::from_retained(surface(width, height, self.samples)?);
        self.slots.push(Slot {
            buffer,
            ready: false,
        });
        Some(self.slots.len().saturating_sub(1))
    }
}

/// Text on a page `width` wide and `rows` tall: lines of 5×7 glyphs in 8×16 cells, each line a
/// random length, from a fixed seed so every run draws the same page.
fn text(width: usize, rows: usize) -> Vec<u8> {
    let mut page = vec![PAPER; width.saturating_mul(rows)];
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let columns = width.checked_div(CELL_W).unwrap_or(0);
    for band in page.chunks_mut(width.saturating_mul(CELL_H)) {
        let length = usize::try_from(next() % 97)
            .unwrap_or(0)
            .saturating_mul(columns)
            / 97;
        for cell in 0..length.min(columns) {
            let glyph = next();
            // A space now and then, so the line reads as words.
            if glyph.is_multiple_of(7) {
                continue;
            }
            let x0 = cell.saturating_mul(CELL_W).saturating_add(1);
            for (row, pixels) in band.chunks_exact_mut(width).enumerate().skip(4).take(7) {
                for col in 0..5_usize {
                    let bit = row.saturating_sub(4).saturating_mul(5).saturating_add(col);
                    if glyph
                        .checked_shr(u32::try_from(bit).unwrap_or(63))
                        .unwrap_or(0)
                        & 1
                        == 1
                        && let Some(p) = pixels.get_mut(x0.saturating_add(col))
                    {
                        *p = INK;
                    }
                }
            }
        }
    }
    page
}

/// An `IOSurface`-backed full-range buffer in `samples`' format, as ScreenCaptureKit delivers.
fn surface(width: usize, height: usize, samples: Samples) -> Option<CFRetained<CVPixelBuffer>> {
    let format = CFNumber::new_i32(i32::from_ne_bytes(samples.os_type().to_ne_bytes()));
    let attrs = CFDictionary::<CFString, CFType>::from_slices(
        &[
            // SAFETY: framework-provided constant string.
            unsafe { kCVPixelBufferIOSurfacePropertiesKey },
            // SAFETY: framework-provided constant string.
            unsafe { kCVPixelBufferPixelFormatTypeKey },
        ],
        &[&CFDictionary::<CFString, CFType>::empty(), &format],
    );
    let mut raw: *mut CVPixelBuffer = ptr::null_mut();
    // SAFETY: CoreVideo rule: a valid out-pointer and an attributes dictionary of CFString keys.
    let status = unsafe {
        CVPixelBufferCreate(
            None,
            width,
            height,
            samples.os_type(),
            Some(attrs.as_opaque()),
            NonNull::from(&mut raw),
        )
    };
    if status != 0 {
        tracing::warn!(status, width, height, "canvas: CVPixelBufferCreate failed");
        return None;
    }
    // SAFETY: `CVPixelBufferCreate` returned a +1 reference, which this takes over.
    Some(unsafe { CFRetained::from_raw(NonNull::new(raw)?) })
}

/// Run `write` over plane `plane` of a buffer the caller holds the write lock of, as its bytes
/// and its row stride.
fn with_plane<R>(
    buffer: &CVPixelBuffer,
    plane: usize,
    write: impl FnOnce(&mut [u8], usize) -> R,
) -> Option<R> {
    let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
    let rows = CVPixelBufferGetHeightOfPlane(buffer, plane);
    let base = NonNull::new(CVPixelBufferGetBaseAddressOfPlane(buffer, plane).cast::<u8>())?;
    let len = stride.checked_mul(rows)?;
    // SAFETY: the caller holds the buffer's write lock, so the plane is mapped for
    // `stride * rows` bytes from its base address, and only this beat writes it (a picture
    // another holder retains is never drawn into).
    let bytes = unsafe { std::slice::from_raw_parts_mut(base.as_ptr(), len) };
    Some(write(bytes, stride))
}

fn fill_plane(buffer: &CVPixelBuffer, samples: Samples, plane: usize, level: u8) {
    let _filled = with_plane(buffer, plane, |bytes, _stride| samples.fill(bytes, level));
}

/// The desktop over the layout's picture, and black past it to the surface's edges.
fn fill_luma(buffer: &CVPixelBuffer, samples: Samples, layout: &Layout) {
    let _filled = with_plane(buffer, 0, |bytes, stride| {
        let picture = layout.width.saturating_mul(samples.width());
        for (y, row) in bytes.chunks_exact_mut(stride).enumerate() {
            let desktop = if y < layout.height {
                picture.min(row.len())
            } else {
                0
            };
            let (inside, outside) = row.split_at_mut(desktop);
            samples.fill(inside, DESKTOP);
            samples.fill(outside, PADDING);
        }
    });
}

/// The painter's page scrolled to its time, and the strip spelling `inputs`. `false` when the
/// planes could not be read.
fn draw(buffer: &CVPixelBuffer, painter: &Painter, inputs: u64) -> bool {
    with_plane(buffer, 0, |luma, stride| {
        draw_luma(luma, stride, painter, inputs)
    })
    .is_some()
}

fn draw_luma(luma: &mut [u8], stride: usize, painter: &Painter, inputs: u64) {
    let Painter {
        layout,
        samples,
        text,
        scrolled,
        ..
    } = painter;
    let samples = *samples;
    let (x0, y0, page_w, page_h) = layout.page;
    let text_rows = text.len().checked_div(page_w).unwrap_or(0);
    let scrolled = *scrolled;
    for (y, row) in luma
        .chunks_exact_mut(stride)
        .enumerate()
        .skip(y0)
        .take(page_h)
    {
        let from = y
            .saturating_sub(y0)
            .wrapping_add(scrolled)
            .checked_rem(text_rows)
            .unwrap_or(0);
        let source = from
            .checked_mul(page_w)
            .and_then(|at| text.get(at..at.checked_add(page_w)?));
        let span = |x: usize| x.saturating_mul(samples.width());
        if let (Some(source), Some(dest)) = (
            source,
            row.get_mut(span(x0)..span(x0.saturating_add(page_w))),
        ) {
            for (sample, &level) in dest.chunks_exact_mut(samples.width()).zip(source) {
                samples.put(sample, level);
            }
        }
    }
    for row in luma.chunks_exact_mut(stride).take(layout.strip_h) {
        for bit in 0..MARK_BITS {
            let on = inputs.checked_shr(bit).unwrap_or(0) & 1 == 1;
            let start = layout
                .mark_w
                .saturating_mul(usize::try_from(bit).unwrap_or(0));
            let span = |x: usize| x.saturating_mul(samples.width());
            if let Some(block) = row.get_mut(span(start)..span(start.saturating_add(layout.mark_w)))
            {
                samples.fill(block, if on { MARK_ON } else { MARK_OFF });
            }
        }
    }
}

/// The capture platform that draws instead of capturing.
#[derive(Clone, Copy, Debug)]
pub enum Canvas {}

/// A display the canvas stands in for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CanvasTarget {
    display: u32,
    native: (u32, u32),
    scale: f32,
}

/// A running canvas: its beat and the queue it runs on.
pub struct CanvasStream {
    beat: Arc<Beat>,
    queue: DispatchRetained<DispatchQueue>,
}

impl std::fmt::Debug for CanvasStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CanvasStream")
            .field("period_us", &self.beat.period_us)
            .finish_non_exhaustive()
    }
}

/// What the beat shares with the stream that owns it.
struct Beat {
    stopped: AtomicBool,
    painter: Mutex<Option<Painter>>,
    period_us: u64,
    sink: Box<dyn Fn(CapturedFrame) + Send + Sync>,
    /// The input count of the last picture delivered, `u64::MAX` before the first: while the
    /// page stands still ([`set_still`]) a picture goes only when it changed.
    delivered_inputs: AtomicU64,
}

impl Beat {
    /// Draw and deliver one picture, then schedule the next beat after `due_us`.
    fn tick(self: Arc<Self>, queue: &DispatchRetained<DispatchQueue>, due_us: u64) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        // Stamped with its beat, the refresh it stands for, as ScreenCaptureKit stamps a frame
        // with the display time it was composed for: a beat run late draws that refresh's
        // picture, and is that much older when it arrives. Every canvas at one rate then
        // stamps the same instants, however late each queue runs.
        let shown_us = due_us;
        let inputs = inputs_taken();
        let unchanged = STILL.load(Ordering::Acquire)
            && self.delivered_inputs.load(Ordering::Acquire) == inputs;
        let picture = if unchanged {
            None
        } else {
            self.painter
                .lock()
                .as_mut()
                .and_then(|p| p.paint(inputs, shown_us))
        };
        if let Some(image) = picture {
            self.delivered_inputs.store(inputs, Ordering::Release);
            let now_us = host_now_us();
            let age_us = now_us.saturating_sub(shown_us);
            (self.sink)(CapturedFrame {
                image,
                capture_ts_us: shown_us,
                display_ts_us: Some(shown_us),
                age_us,
                latency_us: age_us,
            });
        }
        // Stay in phase, skipping the beats a stall ran past rather than bunching them.
        let now_us = host_now_us();
        let mut next = due_us.saturating_add(self.period_us);
        if next <= now_us {
            let behind = now_us.saturating_sub(next);
            let skipped = behind
                .checked_div(self.period_us)
                .unwrap_or(0)
                .saturating_add(1);
            next = next.saturating_add(skipped.saturating_mul(self.period_us));
        }
        self.tick_at(queue, next, now_us);
    }

    /// Run the beat due at `due_us`, `now_us` being now.
    fn tick_at(self: Arc<Self>, queue: &DispatchRetained<DispatchQueue>, due_us: u64, now_us: u64) {
        let wait_ns = due_us.saturating_sub(now_us).saturating_mul(1_000);
        let when = DispatchTime::NOW.time(i64::try_from(wait_ns).unwrap_or(i64::MAX));
        let again = queue.clone();
        let _scheduled = queue.after(when, move || self.tick(&again, due_us));
    }
}

/// A tone where a capture would hear the applications' sound: [`TONE_HZ`] in both channels,
/// in [`TONE_CHUNK_US`] chunks on their own beat, for as long as it runs.
pub struct Tone {
    beat: Arc<ToneBeat>,
    queue: DispatchRetained<DispatchQueue>,
}

impl std::fmt::Debug for Tone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tone")
            .field("heard", &*self.beat.heard.lock())
            .finish_non_exhaustive()
    }
}

impl Tone {
    /// Whose sound it stands for now.
    #[must_use]
    pub fn heard(&self) -> Heard {
        self.beat.heard.lock().clone()
    }

    fn start(heard: Heard, sink: AudioSink) -> Self {
        let beat = Arc::new(ToneBeat {
            stopped: AtomicBool::new(false),
            heard: Mutex::new(heard),
            sink,
            phase: AtomicU64::new(0),
        });
        let interactive = DispatchQueueAttr::with_qos_class(
            DispatchQueueAttr::SERIAL,
            DispatchQoS::UserInteractive,
            0,
        );
        let queue = DispatchQueue::new("dev.onicode.canvas.tone", Some(&interactive));
        let first = Arc::clone(&beat);
        let again = queue.clone();
        queue.exec_async(move || first.chunk(&again, host_now_us()));
        Self { beat, queue }
    }
}

/// The tone a [`Tone`] plays, in hertz.
pub const TONE_HZ: f64 = 440.0;

/// How much sound each chunk of a [`Tone`] carries, and how often one comes: what
/// ScreenCaptureKit hands over at a time is about this.
pub const TONE_CHUNK_US: u64 = 10_000;

/// Stereo frames in a chunk of a [`Tone`].
const TONE_FRAMES: u64 = AUDIO_RATE as u64 * TONE_CHUNK_US / 1_000_000;

struct ToneBeat {
    stopped: AtomicBool,
    heard: Mutex<Heard>,
    sink: AudioSink,
    /// Frames played so far: where the next chunk's sine starts.
    phase: AtomicU64,
}

impl ToneBeat {
    /// Hand over the chunk due at `due_us`, and ask for the next.
    fn chunk(self: Arc<Self>, queue: &DispatchRetained<DispatchQueue>, due_us: u64) {
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let frames = TONE_FRAMES;
        let start = self.phase.fetch_add(frames, Ordering::Relaxed);
        let channels = usize::try_from(AUDIO_CHANNELS).unwrap_or(2);
        let mut samples = Vec::with_capacity(
            usize::try_from(frames)
                .unwrap_or(0)
                .saturating_mul(channels),
        );
        for frame in start..start.saturating_add(frames) {
            #[expect(
                clippy::cast_precision_loss,
                reason = "a sine's phase, wrapped well in range"
            )]
            let t = frame.checked_rem(u64::from(AUDIO_RATE)).unwrap_or(0) as f64
                / f64::from(AUDIO_RATE);
            #[expect(clippy::cast_possible_truncation, reason = "a sample in [-0.25, 0.25]")]
            let v = (0.25 * (std::f64::consts::TAU * TONE_HZ * t).sin()) as f32;
            samples.extend(std::iter::repeat_n(v, channels));
        }
        (self.sink)(CapturedAudio {
            pts_us: due_us,
            samples,
        });
        let next = due_us.saturating_add(TONE_CHUNK_US);
        let wait_ns = next.saturating_sub(host_now_us()).saturating_mul(1_000);
        let when = DispatchTime::NOW.time(i64::try_from(wait_ns).unwrap_or(i64::MAX));
        let again = queue.clone();
        let _scheduled = queue.after(when, move || self.chunk(&again, next));
    }
}

/// The first beat of a canvas started at `now_us`: the next multiple of the period on the host
/// clock. Every canvas at one rate then beats on the same instants, as the windows of one
/// display are captured on its one refresh, whenever each was started.
const fn first_beat(now_us: u64, period_us: u64) -> u64 {
    match now_us.checked_next_multiple_of(period_us) {
        Some(due) => due,
        None => now_us,
    }
}

impl CaptureSource for Canvas {
    type Content = Vec<DisplayInfo>;
    type Image = PixelBuffer;
    type Sound = Tone;
    type Stream = CanvasStream;
    type Target = CanvasTarget;

    fn can_capture() -> bool {
        true
    }

    fn start_sound(
        heard: &Heard,
        sink: AudioSink,
        _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<Tone, CaptureError>) + Send + 'static,
    ) {
        done(Ok(Tone::start(heard.clone(), sink)));
    }

    fn hear(
        sound: &Tone,
        heard: &Heard,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        *sound.beat.heard.lock() = heard.clone();
        done(Ok(()));
    }

    fn stop_sound(sound: &Tone, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        sound.beat.stopped.store(true, Ordering::Release);
        // Behind any chunk already running, so none follows the answer.
        sound.queue.exec_async(move || done(Ok(())));
    }

    fn enumerate(done: impl FnOnce(Result<Vec<DisplayInfo>, CaptureError>) + Send + 'static) {
        let mut displays = geometry::active_displays();
        if let Some(hz) = beat_hz() {
            for display in &mut displays {
                #[expect(clippy::cast_possible_truncation, reason = "a u16 rate")]
                let hz = hz as f32;
                display.hz = hz;
            }
        }
        done(Ok(displays));
    }

    fn displays(content: &Vec<DisplayInfo>) -> Vec<DisplayInfo> {
        content.clone()
    }

    fn resolve(
        content: &Vec<DisplayInfo>,
        kind: CaptureTarget,
    ) -> Result<CanvasTarget, CaptureError> {
        let CaptureTarget::Display(id) = kind;
        let display = content
            .iter()
            .find(|d| d.id == id)
            .ok_or(CaptureError::NotFound(kind))?;
        let px = |points: f32| {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped"
            )]
            let v = (f64::from(points) * f64::from(display.scale))
                .round()
                .clamp(2.0, 16_384.0) as u32;
            v.next_multiple_of(2)
        };
        Ok(CanvasTarget {
            display: id.0,
            native: (px(display.w), px(display.h)),
            scale: display.scale,
        })
    }

    fn pixel_size(target: &CanvasTarget) -> (u32, u32) {
        target.native
    }

    fn point_scale(target: &CanvasTarget) -> f32 {
        target.scale
    }

    fn start(
        target: &CanvasTarget,
        config: &CaptureConfig,
        sink: impl Fn(CapturedFrame) + Send + Sync + 'static,
        _on_stop: impl Fn(CaptureError) + Send + Sync + 'static,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) -> Result<CanvasStream, CaptureError> {
        let hz = if config.fps == 0 {
            beat_hz()
                .or_else(|| geometry::display_refresh_hz(target.display))
                .unwrap_or(FALLBACK_HZ)
        } else {
            f64::from(config.fps)
        };
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped"
        )]
        let period_us = (1e6 / hz.clamp(1.0, 240.0)).round() as u64;
        let beat = Arc::new(Beat {
            stopped: AtomicBool::new(false),
            painter: Mutex::new(Painter::new(config)),
            period_us,
            sink: Box::new(sink),
            delivered_inputs: AtomicU64::new(u64::MAX),
        });
        // User-interactive, as the capture queue it stands in for is.
        let interactive = DispatchQueueAttr::with_qos_class(
            DispatchQueueAttr::SERIAL,
            DispatchQoS::UserInteractive,
            0,
        );
        let queue = DispatchQueue::new("dev.onicode.canvas", Some(&interactive));
        let first = Arc::clone(&beat);
        let again = queue.clone();
        queue.exec_async(move || {
            done(Ok(()));
            let now_us = host_now_us();
            first.tick_at(&again, first_beat(now_us, period_us), now_us);
        });
        Ok(CanvasStream { beat, queue })
    }

    fn update(
        stream: &CanvasStream,
        config: &CaptureConfig,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        let beat = Arc::clone(&stream.beat);
        let config = *config;
        stream.queue.exec_async(move || {
            *beat.painter.lock() = Painter::new(&config);
            done(Ok(()));
        });
    }

    fn retarget(
        stream: &CanvasStream,
        _target: &CanvasTarget,
        done: impl FnOnce(Result<(), CaptureError>) + Send + 'static,
    ) {
        stream.queue.exec_async(move || done(Ok(())));
    }

    fn stop(stream: &CanvasStream, done: impl FnOnce(Result<(), CaptureError>) + Send + 'static) {
        stream.beat.stopped.store(true, Ordering::Release);
        // Behind any beat already running, so no picture follows the answer.
        stream.queue.exec_async(move || done(Ok(())));
    }

    fn now_us() -> u64 {
        host_now_us()
    }

    fn console() -> Option<Console> {
        Some(match CONSOLE.load(Ordering::Relaxed) {
            1 => Console::Locked,
            2 => Console::Away,
            _ => Console::Shown,
        })
    }

    fn target_bounds(target: CaptureTarget) -> Option<Rect> {
        geometry::target_bounds(target)
    }

    fn refresh_hz(target: CaptureTarget) -> Option<f64> {
        beat_hz().or_else(|| geometry::target_refresh_hz(target))
    }

    fn display_bounds(id: u32) -> Rect {
        geometry::display_bounds(id)
    }

    fn pointer_moves() -> u32 {
        0
    }

    fn pointer_location() -> (f64, f64) {
        (0.0, 0.0)
    }

    fn cursor_seed() -> Option<i32> {
        None
    }

    fn cursor_shape() -> Option<CursorShape> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(width: u32, height: u32, format: PixelFormat) -> CaptureConfig {
        CaptureConfig {
            width,
            height,
            align: 16,
            fps: 60,
            format,
            queue_depth: 3,
        }
    }

    /// A picture spells the input count in its strip, and the reader gets it back, in either
    /// format the canvas draws; a picture too small for a strip reads as none.
    #[test]
    fn the_strip_reads_back_what_was_drawn() {
        for format in [PixelFormat::Nv12Full, PixelFormat::Yuv444Full10] {
            let mut painter = Painter::new(&config(640, 360, format)).unwrap();
            for inputs in [0_u64, 1, 0x5a5a, 0xffff, 0x1_0003] {
                let picture = painter.paint(inputs, 0).unwrap();
                assert_eq!(
                    CVPixelBufferGetPixelFormatType(picture.as_cv()),
                    Samples::of(format).os_type(),
                    "{format:?}"
                );
                assert_eq!(
                    inputs_shown(picture.as_cv()),
                    Some(u16::try_from(inputs & 0xffff).unwrap()),
                    "{format:?}"
                );
            }
        }
        let tiny = surface(8, 8, Samples::Nv12).unwrap();
        assert_eq!(inputs_shown(&tiny), None, "8 px has no room for 16 blocks");
    }

    /// A picture something else still holds is never drawn over: the next beat takes another,
    /// and once all are held the beat drops its frame instead of tearing one.
    #[test]
    fn a_held_picture_is_never_drawn_over() {
        let mut painter = Painter::new(&config(320, 180, PixelFormat::Nv12Full)).unwrap();
        let first = painter.paint(1, 0).unwrap();
        let second = painter.paint(2, 0).unwrap();
        assert!(
            !ptr::eq(first.as_cv(), second.as_cv()),
            "the held picture was reused"
        );
        assert_eq!(
            inputs_shown(first.as_cv()),
            Some(1),
            "drawing the second changed the first"
        );
        let mut held = vec![first, second];
        while let Some(p) = painter.paint(3, 0) {
            held.push(p);
        }
        assert_eq!(held.len(), SLOTS);
        drop(held.pop());
        assert!(
            painter.paint(4, 0).is_some(),
            "a picture let go of is drawn into again"
        );
    }

    /// A picture whose sides are off 16 comes in a surface padded to 16, black past the
    /// picture's right and bottom edges and the desktop inside them, as a capture's does.
    #[test]
    fn a_picture_off_16_is_drawn_into_a_padded_surface() {
        for format in [PixelFormat::Nv12Full, PixelFormat::Yuv444Full10] {
            let mut painter = Painter::new(&config(1000, 590, format)).unwrap();
            let picture = painter.paint(0, 0).unwrap();
            assert_eq!(
                (picture.width(), picture.height()),
                (1008, 592),
                "{format:?}"
            );
            let samples = Samples::of(format);
            let image = picture.as_cv();
            // SAFETY: CoreVideo rule: read between a lock and its unlock.
            let locked =
                unsafe { CVPixelBufferLockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
            assert_eq!(locked, 0);
            let level = |x: usize, y: usize| {
                with_plane(image, 0, |plane, stride| {
                    let at = y * stride + x * samples.width();
                    samples.level(&plane[at..at + samples.width()])
                })
                .flatten()
            };
            let corners = [
                level(999, 589),
                level(1000, 589),
                level(999, 590),
                level(1007, 591),
            ];
            // SAFETY: matches the lock above.
            let _unlocked =
                unsafe { CVPixelBufferUnlockBaseAddress(image, CVPixelBufferLockFlags::ReadOnly) };
            let (inside, padding) = (Some(DESKTOP), Some(PADDING));
            assert_eq!(corners, [inside, padding, padding, padding], "{format:?}");
        }
    }

    /// The scrolling page changes every frame, as a scrolling window does, so the encoder sees
    /// motion on every beat rather than a still desktop; the scroll is set in time, so a
    /// 120 Hz beat moves it half as far a frame as a 60 Hz one.
    #[test]
    fn the_page_moves_every_frame() {
        let mut painter = Painter::new(&config(320, 180, PixelFormat::Nv12Full)).unwrap();
        let row = |p: &CVPixelBuffer| {
            // SAFETY: CoreVideo rule: read between a lock and its unlock.
            let locked =
                unsafe { CVPixelBufferLockBaseAddress(p, CVPixelBufferLockFlags::empty()) };
            assert_eq!(locked, 0);
            let (bytes, stride) =
                with_plane(p, 0, |plane, stride| (plane.to_vec(), stride)).unwrap();
            // SAFETY: matches the lock above.
            let _unlocked =
                unsafe { CVPixelBufferUnlockBaseAddress(p, CVPixelBufferLockFlags::empty()) };
            bytes
                .chunks_exact(stride)
                .skip(45)
                .take(90)
                .flatten()
                .copied()
                .collect::<Vec<u8>>()
        };
        let a = painter.paint(0, 1_000).unwrap();
        let before = row(a.as_cv());
        drop(a);
        let b = painter.paint(0, 1_000 + 8_334).unwrap();
        assert_ne!(before, row(b.as_cv()), "the page did not scroll");
        assert_eq!(painter.scrolled, 1, "a 120 Hz beat scrolls one row");
        drop(b);
        let _c = painter.paint(0, 1_000 + 16_667).unwrap();
        assert_eq!(painter.scrolled, 3, "a 60 Hz beat scrolls three");
    }

    /// Two canvases at one rate, started half a period apart, deliver on the same instants, as
    /// the windows of one display are captured on its one refresh: every picture of either is
    /// stamped with a beat of the one grid, however late its queue ran it. (The distance between
    /// their stamps as delivered was held to 2 ms, and under `ThreadSanitizer` was 2.7 ms: Deep run
    /// 40989163091.)
    #[test]

    fn canvases_at_one_rate_beat_together() {
        let target = CanvasTarget {
            display: 0,
            native: (320, 180),
            scale: 1.0,
        };
        let config = config(320, 180, PixelFormat::Nv12Full);
        let (tx, rx) = std::sync::mpsc::channel();
        let start = |stream: usize| {
            let tx = tx.clone();
            Canvas::start(
                &target,
                &config,
                move |frame| {
                    let _gone = tx.send((stream, frame.capture_ts_us));
                },
                |_error| {},
                |_started| {},
            )
            .unwrap()
        };
        let first = start(0);
        std::thread::sleep(std::time::Duration::from_micros(8_333));
        let second = start(1);
        // A canvas's first picture waits on CoreVideo's first surfaces, over a second on a busy
        // machine, so the beats are counted rather than timed.
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while a.len().min(b.len()) < 20 {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok((stream, at)) = rx.recv_timeout(left) else {
                break;
            };
            if stream == 0 { a.push(at) } else { b.push(at) }
        }
        for stream in [&first, &second] {
            Canvas::stop(stream, |_stopped| {});
        }
        assert!(
            a.len() >= 20 && b.len() >= 20,
            "{} and {} beats",
            a.len(),
            b.len()
        );
        let period = first.beat.period_us;
        assert_eq!(second.beat.period_us, period, "one rate, one period");
        let off: Vec<u64> = a
            .iter()
            .chain(&b)
            .copied()
            .filter(|t| t.checked_rem(period) != Some(0))
            .collect();
        assert!(off.is_empty(), "stamped off the {period} µs grid: {off:?}");
    }
}
