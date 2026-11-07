//! A stream's pictures, from the decoder's thread to a layer of their own (slopty-ui's
//! `screen/glass.rs`, without zoom regions).
//!
//! The decoder hands every picture to [`Glass::offer`] on its own thread
//! (`ScreenHandle::set_present`). The glass gives it straight to a layer (`VideoLayer` in the
//! app, [`Layer`] so tests put up their own), which draws it on its own thread and never waits
//! for a GPUI frame; the view only places the layer where the picture goes, like any native.
//! A picture costs the window no frame. The view is told only when what it draws around the
//! picture changes: the first picture, and a new size, chroma or seam ([`Shape`]).
//!
//! The layer's report (`VideoLayer::on_presented`, on a Metal thread) says when the window
//! server showed the picture: the glass hands that to the stream (`ScreenHandle::presented`),
//! whose pacer times capture → glass, and keeps a [`Pacer`] of its own for the overlay. A
//! picture the layer's mailbox replaced before it was shown is reported from inside the
//! present with no time, and counted as replaced. Until a layer is attached, the newest picture
//! waits here and goes to the first layer.
//!
//! A stream coded as two stripes decodes each on a session of its own (`Stitched`), and each
//! goes to a layer of its own with no copy between them: the view stacks the two layers, each
//! clipped to the rows it shows. The glass counts the captures whose stripes reached the glass
//! apart ([`Glass::seams`]).
//!
//! Locks: `slot` is held across a present, so pictures reach the layer in the order they came,
//! and a replaced layer never gets one late. `book` is taken inside it for a moment, and alone
//! by the layer's report, which a present can make on the presenting thread itself. Nothing
//! takes `slot` while it holds `book`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use gpui::PresentedFrame;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
    kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
};
use onicode_desktop_client::{Picture as Buffer, Presentable, ScreenHandle};
use onicode_desktop_media::{FrameStamp, GlassStats, Offer, Pacer, PacingStats, PaintRate};
use onicode_desktop_proto::screen::Chroma;
use parking_lot::Mutex;
use tokio::sync::watch;

/// Pictures put up and not yet reported that are remembered: a report can go missing when the
/// layer leaves the screen, and a forgotten one is a picture never timed, not a leak.
const FLYING: usize = 8;

/// Two stripes' pictures seen apart at the glass by more than this showed a tear: a refresh
/// is 8.3 ms at 120 Hz, and two layers shown in one refresh are timed within a millisecond.
const SPLIT_AFTER: Duration = Duration::from_millis(4);

/// Captures whose stripes are remembered until both layers report: as [`FLYING`].
const PAIRS: usize = 8;

/// Where a layer's reports go: the picture's sequence on the layer, and when it was shown.
pub(crate) type Reported = Arc<dyn Fn(u64, PresentedFrame) + Send + Sync>;

/// What pictures go up on: a `VideoLayer` in a native host, or a test's stand-in.
pub(crate) trait Layer: Send {
    /// Put `picture` up; its sequence on the layer, which counts every picture it took from 0.
    ///
    /// # Errors
    ///
    /// The layer refused it.
    fn present(&mut self, picture: &CVPixelBuffer) -> gpui::Result<u64>;
}

impl Layer for gpui_apple::fast::video_layer::VideoLayer {
    fn present(&mut self, picture: &CVPixelBuffer) -> gpui::Result<u64> {
        gpui_apple::fast::video_layer::VideoLayer::present(self, picture)
    }
}

/// How much colour a decoded picture of CoreVideo pixel format `format` carries: the decoder
/// hands back bi-planar 4:4:4 (`xf44` at 10 bits, `444f` at 8) only for a stream encoded so.
#[must_use]
pub(crate) const fn chroma_of(format: u32) -> Chroma {
    if format == kCVPixelFormatType_444YpCbCr10BiPlanarFullRange
        || format == kCVPixelFormatType_444YpCbCr8BiPlanarFullRange
    {
        Chroma::Full
    } else {
        Chroma::Subsampled
    }
}

/// What the view draws around the picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Shape {
    /// The picture in pixels: both stripes together for a striped one.
    pub size: (u32, u32),
    /// How much colour it carries.
    pub chroma: Chroma,
    /// Where a striped picture's two stripes meet; `None` for one picture.
    pub seam: Option<Seam>,
}

/// How a striped picture's two pictures stack: each is its stripe's coded rows, which run past
/// the seam into the other's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Seam {
    /// The top stripe's picture's height.
    pub top: u32,
    /// Its rows shown, from its first: the rows above the seam.
    pub top_rows: u32,
    /// The lower stripe's picture's height.
    pub lower: u32,
    /// The first of its rows shown, the one at the seam.
    pub lower_from: u32,
}

impl Shape {
    /// The shape of `picture`.
    pub(crate) fn of(picture: &Picture) -> Self {
        let (width, top) = side(picture.top.as_cv());
        let seam = picture.lower.as_ref().map(|lower| Seam {
            top,
            top_rows: lower.top_rows,
            lower: side(lower.buffer.as_cv()).1,
            lower_from: lower.shown_from,
        });
        let height = seam.map_or(top, |seam| {
            seam.top_rows
                .saturating_add(seam.lower.saturating_sub(seam.lower_from))
        });
        Self {
            size: (width, height),
            chroma: chroma_of(CVPixelBufferGetPixelFormatType(picture.top.as_cv())),
            seam,
        }
    }
}

/// A buffer's width and height.
fn side(buffer: &CVPixelBuffer) -> (u32, u32) {
    let side = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    (
        side(CVPixelBufferGetWidth(buffer)),
        side(CVPixelBufferGetHeight(buffer)),
    )
}

/// A decoded picture the decoder's thread, the layer's and the main thread all hold: one
/// picture, or a striped one's top stripe and its lower one.
#[derive(Clone)]
pub(crate) struct Picture {
    top: Buffer,
    lower: Option<Lower>,
}

/// A striped picture's lower stripe.
#[derive(Clone)]
struct Lower {
    buffer: Buffer,
    /// Rows of the top stripe's picture shown.
    top_rows: u32,
    /// The first row of this one's shown.
    shown_from: u32,
}

impl Picture {
    /// The decoder's picture, held once more: the layer takes the decoder's own buffer.
    pub(crate) fn of(frame: &Presentable) -> Self {
        Self {
            top: frame.frame.image.clone(),
            lower: frame.stripes.as_ref().map(|stitched| Lower {
                buffer: stitched.lower.image.clone(),
                top_rows: stitched.top_rows,
                shown_from: stitched.lower_from,
            }),
        }
    }

    /// One picture a test made.
    #[cfg(test)]
    pub(crate) const fn new(top: Buffer) -> Self {
        Self { top, lower: None }
    }

    /// A striped picture a test made: `top` shows its first `top_rows`, `lower` its rows from
    /// `lower_from`.
    #[cfg(test)]
    pub(crate) const fn striped(
        top: Buffer,
        top_rows: u32,
        lower: Buffer,
        lower_from: u32,
    ) -> Self {
        Self {
            top,
            lower: Some(Lower {
                buffer: lower,
                top_rows,
                shown_from: lower_from,
            }),
        }
    }
}

/// A layer the pictures go to.
struct Held {
    layer: Box<dyn Layer>,
    /// Tells this layer's reports from a replaced one's.
    generation: u64,
    /// The sequence the layer gives the next picture.
    next: u64,
}

#[derive(Default)]
struct Slot {
    layer: Option<Held>,
    /// The lower stripe's layer, once a striped picture came.
    lower: Option<Held>,
    /// The newest picture: a layer attached after it came shows it first.
    last: Option<Picture>,
    /// `last` has not been put up on a layer yet.
    waiting: bool,
    generations: u64,
    /// Where the stripes meet in the layout the view last placed the layers for; `None` before
    /// it has placed them. A picture whose stripes meet elsewhere, or that has none where the
    /// layout has, waits for the view to place the layers for it ([`Glass::place`]): shown
    /// before, the top stripe would fill the whole picture's place for a frame.
    placed: Option<Option<Seam>>,
}

impl Slot {
    /// Whether `last` can go up on the layers as the view placed them.
    fn fits(&self) -> bool {
        let Some(picture) = &self.last else {
            return false;
        };
        let seam = Shape::of(picture).seam;
        self.placed.is_none_or(|placed| placed == seam)
    }
}

/// A picture on its way to the glass.
#[derive(Clone, Copy)]
struct Flying {
    generation: u64,
    sequence: u64,
    stamp: FrameStamp,
}

struct Book {
    pacer: Pacer,
    flying: VecDeque<Flying>,
    /// Captures whose stripes went up on the two layers, until both report.
    pairs: VecDeque<Pair>,
    seams: Seams,
    /// Pictures the layer's mailbox replaced before they were shown.
    replaced: u64,
    /// The thread inside a present now, which reports there a picture its mailbox replaced.
    presenting: Option<std::thread::ThreadId>,
    /// The stream told when its pictures reach the glass.
    handle: Option<ScreenHandle>,
}

/// A capture's two stripes on their way to the glass: each one's layer generation and
/// sequence, and what each layer reported of it.
#[derive(Clone, Copy)]
struct Pair {
    stripes: [(u64, u64); 2],
    reports: [Option<Report>; 2],
}

/// What a layer reported of a stripe's picture.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Report {
    /// On the glass then.
    Shown(Instant),
    /// Replaced in the layer's mailbox by a newer picture, never shown.
    Replaced,
}

/// How a striped stream's captures reached the glass.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct Seams {
    /// Captures whose two stripes were shown in the same refresh.
    pub together: u64,
    /// Captures whose stripes were shown apart, or only one of them: the seam tore for a
    /// refresh.
    pub split: u64,
}

/// One stream's way to the glass.
pub(crate) struct Glass {
    slot: Mutex<Slot>,
    book: Mutex<Book>,
    /// Pictures put up on a layer.
    put_up: AtomicU64,
    shape: watch::Sender<Option<Shape>>,
}

impl Glass {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            slot: Mutex::new(Slot::default()),
            book: Mutex::new(Book {
                pacer: Pacer::default(),
                flying: VecDeque::new(),
                pairs: VecDeque::new(),
                seams: Seams::default(),
                replaced: 0,
                presenting: None,
                handle: None,
            }),
            put_up: AtomicU64::new(0),
            shape: watch::Sender::new(None),
        })
    }

    /// Tell `handle` when the pictures reach the glass.
    pub(crate) fn report_to(&self, handle: Option<ScreenHandle>) {
        self.book.lock().handle = handle;
    }

    /// What the view draws around the picture, as it changes.
    pub(crate) fn shapes(&self) -> watch::Receiver<Option<Shape>> {
        self.shape.subscribe()
    }

    /// A decoded picture from the stream: put up at once unless it is older than the one up.
    /// Any thread.
    pub(crate) fn offer_frame(&self, frame: &Presentable) {
        self.offer(Picture::of(frame), frame.stamp);
    }

    /// A decoded picture: put up on the layer at once unless the pacer drops it (older than
    /// the one up). Any thread. `slot` is held from the pacer's verdict through the present, so
    /// pictures reach the layer in the order the pacer took them.
    pub(crate) fn offer(&self, picture: Picture, stamp: FrameStamp) {
        let mut slot = self.slot.lock();
        if self.book.lock().pacer.offer(stamp) == Offer::Drop {
            return;
        }
        let shape = Shape::of(&picture);
        self.shape.send_if_modified(|was| {
            let changed = *was != Some(shape);
            *was = Some(shape);
            changed
        });
        slot.last = Some(picture);
        slot.waiting = true;
        self.present(&mut slot);
    }

    /// Put `slot`'s newest picture up on its layer, if it has one: a striped picture's top
    /// stripe there and its lower one on the lower layer, back to back.
    fn present(&self, slot: &mut Slot) {
        if !slot.fits() {
            return;
        }
        let Slot {
            layer: Some(layer),
            lower,
            last: Some(picture),
            waiting,
            ..
        } = slot
        else {
            return;
        };
        // A picture shown on a replaced layer goes on the new one untimed: it was timed there.
        let stamp = if *waiting {
            self.book.lock().pacer.painted()
        } else {
            None
        };
        *waiting = false;
        let booked = stamp.map(|stamp| {
            let flying = Flying {
                generation: layer.generation,
                sequence: layer.next,
                stamp,
            };
            let mut book = self.book.lock();
            if book.flying.len() >= FLYING {
                book.flying.pop_front();
            }
            book.flying.push_back(flying);
            flying
        });
        self.book.lock().presenting = Some(std::thread::current().id());
        let presented = layer.layer.present(picture.top.as_cv());
        let lower = picture
            .lower
            .as_ref()
            .zip(lower.as_mut())
            .map(|(picture, lower)| {
                let presented = lower.layer.present(picture.buffer.as_cv());
                (lower, presented)
            });
        self.book.lock().presenting = None;
        if let (Ok(top), Some((lower, Ok(sequence)))) = (&presented, lower) {
            lower.next = sequence.saturating_add(1);
            let mut book = self.book.lock();
            if book.pairs.len() >= PAIRS {
                book.pairs.pop_front();
            }
            let stripes = [(layer.generation, *top), (lower.generation, sequence)];
            book.pairs.push_back(Pair {
                stripes,
                reports: [None, None],
            });
        }
        match presented {
            Ok(sequence) => {
                layer.next = sequence.saturating_add(1);
                self.put_up.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                if let Some(booked) = booked {
                    self.book.lock().flying.retain(|f| {
                        (f.generation, f.sequence) != (booked.generation, booked.sequence)
                    });
                }
                log::warn!("the video layer refused a picture: {error}");
            }
        }
    }

    /// Put the pictures on `layer`, made by the caller with [`Glass::reports`] as its report
    /// sink, replacing the one they went to. The newest picture goes up on it at once, `slot`
    /// held through the present so no picture reaches the replaced layer after it. Main thread.
    pub(crate) fn attach(&self, layer: Box<dyn Layer>, generation: u64) {
        let mut slot = self.slot.lock();
        slot.layer = Some(Held {
            layer,
            generation,
            next: 0,
        });
        self.present(&mut slot);
    }

    /// Put a striped picture's lower stripe on `layer`, made with [`Glass::lower_reports`],
    /// replacing the one it went to, as [`Glass::attach`] does. Main thread.
    pub(crate) fn attach_lower(&self, layer: Box<dyn Layer>, generation: u64) {
        let mut slot = self.slot.lock();
        slot.lower = Some(Held {
            layer,
            generation,
            next: 0,
        });
        self.present(&mut slot);
    }

    /// A new generation for a layer about to be made, and the sink its reports go to.
    pub(crate) fn reports(self: &Arc<Self>) -> (u64, Reported) {
        let generation = self.next_generation();
        let glass = Arc::downgrade(self);
        let sink: Reported = Arc::new(move |sequence, frame| {
            reported(&glass, generation, sequence, frame);
        });
        (generation, sink)
    }

    /// As [`Glass::reports`], for the lower stripe's layer.
    pub(crate) fn lower_reports(self: &Arc<Self>) -> (u64, Reported) {
        let generation = self.next_generation();
        let glass = Arc::downgrade(self);
        let sink: Reported = Arc::new(move |sequence, frame: PresentedFrame| {
            if let Some(glass) = glass.upgrade() {
                glass
                    .book
                    .lock()
                    .stripe_shown(1, (generation, sequence), frame.presented_at);
            }
        });
        (generation, sink)
    }

    fn next_generation(&self) -> u64 {
        let mut slot = self.slot.lock();
        slot.generations = slot.generations.saturating_add(1);
        slot.generations
    }

    /// The view placed the layers for pictures whose stripes meet at `seam` (`None` for one
    /// picture): a picture that waited for it goes up now. Main thread, when it changes.
    pub(crate) fn place(&self, seam: Option<Seam>) {
        let mut slot = self.slot.lock();
        let was = slot.placed.replace(seam);
        if was.is_some_and(|was| was != seam) && slot.waiting {
            self.present(&mut slot);
        }
    }

    /// Let go of the layers: pictures wait here until another is attached.
    pub(crate) fn detach(&self) {
        let (layer, lower) = {
            let mut slot = self.slot.lock();
            (slot.layer.take(), slot.lower.take())
        };
        drop((layer, lower));
    }

    /// How a striped stream's captures reached the glass so far.
    pub(crate) fn seams(&self) -> Seams {
        self.book.lock().seams
    }

    /// Pictures put up on a layer so far.
    #[cfg(test)]
    pub(crate) fn put_up(&self) -> u64 {
        self.put_up.load(Ordering::Relaxed)
    }

    /// Pictures the layer's mailbox replaced before they were shown.
    pub(crate) fn replaced(&self) -> u64 {
        self.book.lock().replaced
    }

    /// The pictures painted in the last second, and those that missed the display.
    pub(crate) fn rate(&self) -> PaintRate {
        self.book.lock().pacer.rate()
    }

    /// Arrival → glass over the last pictures shown, and the counters.
    pub(crate) fn pacing(&self) -> PacingStats {
        self.book.lock().pacer.stats()
    }

    /// Capture → glass and input → glass over the last pictures shown.
    pub(crate) fn glass(&self) -> GlassStats {
        self.book.lock().pacer.glass()
    }
}

impl Book {
    /// Stripe `index`'s layer reported the picture it numbered `at` (its generation and
    /// sequence): shown then, or never. Once both of a capture's stripes have reported, the
    /// capture counts as shown together or apart.
    fn stripe_shown(&mut self, index: usize, at: (u64, u64), shown: Option<Instant>) {
        let found = self
            .pairs
            .iter()
            .position(|pair| pair.stripes.get(index) == Some(&at));
        let Some((found, pair)) = found.and_then(|i| Some((i, self.pairs.get_mut(i)?))) else {
            return;
        };
        let Some(report) = pair.reports.get_mut(index) else {
            return;
        };
        *report = Some(shown.map_or(Report::Replaced, Report::Shown));
        let [Some(top), Some(lower)] = pair.reports else {
            return;
        };
        self.pairs.remove(found);
        match (top, lower) {
            (Report::Shown(top), Report::Shown(lower))
                if top.max(lower).saturating_duration_since(top.min(lower)) <= SPLIT_AFTER =>
            {
                self.seams.together = self.seams.together.saturating_add(1);
            }
            // Neither shown: a newer capture replaced both, and nothing tore.
            (Report::Replaced, Report::Replaced) => {}
            _apart => self.seams.split = self.seams.split.saturating_add(1),
        }
    }
}

/// What became of the picture the layer of `generation` numbered `sequence`.
fn reported(glass: &Weak<Glass>, generation: u64, sequence: u64, frame: PresentedFrame) {
    let Some(glass) = glass.upgrade() else {
        return;
    };
    let mut book = glass.book.lock();
    book.stripe_shown(0, (generation, sequence), frame.presented_at);
    let Some(at) = book
        .flying
        .iter()
        .position(|f| f.generation == generation && f.sequence == sequence)
    else {
        return;
    };
    let Some(flying) = book.flying.remove(at) else {
        return;
    };
    match frame.presented_at {
        Some(shown) => book.pacer.shown(flying.stamp, shown),
        // The layer reports a picture its mailbox replaced from inside the present.
        None if book.presenting == Some(std::thread::current().id()) => {
            book.replaced = book.replaced.saturating_add(1);
        }
        // Drawn, but the display did not say when it showed it (one that reports no scan-out,
        // as a remote desktop's virtual display): untimed.
        None => {}
    }
    let handle = book.handle.clone();
    drop(book);
    if let Some(handle) = handle {
        handle.presented(flying.stamp, frame.presented_at);
    }
}

#[cfg(test)]
pub(crate) mod tests;
