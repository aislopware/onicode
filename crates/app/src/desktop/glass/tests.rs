use std::sync::Arc;
use std::time::{Duration, Instant};

use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth,
    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
};
use parking_lot::Mutex;

use super::*;

/// A blank picture in CoreVideo pixel format `format` for a test to put up.
pub(crate) fn test_buffer(format: u32, width: usize, height: usize) -> Buffer {
    let mut raw: *mut CVPixelBuffer = std::ptr::null_mut();
    // SAFETY: CoreVideo's rule for `CVPixelBufferCreate`: a valid out-pointer, and no
    // allocator or attributes (the defaults).
    let status = unsafe {
        objc2_core_video::CVPixelBufferCreate(
            None,
            width,
            height,
            format,
            None,
            std::ptr::NonNull::from(&mut raw),
        )
    };
    assert_eq!(status, 0, "CVPixelBufferCreate");
    let raw = std::ptr::NonNull::new(raw).expect("CVPixelBufferCreate made a buffer");
    // SAFETY: `CVPixelBufferCreate` returned a +1 reference, which this takes over.
    Buffer::from_retained(unsafe { CFRetained::from_raw(raw) })
}

/// A stamp for a picture a test puts up: decoded and arrived now, after every one before.
pub(crate) fn test_stamp(index: u64) -> FrameStamp {
    let now = Instant::now();
    FrameStamp {
        pts_us: index.saturating_add(1),
        decode_seq: index,
        arrived: now,
        decoded: now,
        captured: None,
    }
}

fn picture(w: usize, h: usize) -> Picture {
    Picture::new(test_buffer(
        kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        w,
        h,
    ))
}

fn frame(presented_at: Option<Instant>) -> PresentedFrame {
    PresentedFrame {
        submitted_at: Instant::now(),
        presented_at,
    }
}

/// A layer that keeps the size of each picture it is given.
#[derive(Clone, Default)]
pub(crate) struct TestLayer {
    pub took: Arc<Mutex<Vec<(usize, usize)>>>,
}

impl Layer for TestLayer {
    fn present(&mut self, picture: &CVPixelBuffer) -> gpui::Result<u64> {
        let mut took = self.took.lock();
        took.push((
            CVPixelBufferGetWidth(picture),
            CVPixelBufferGetHeight(picture),
        ));
        Ok(u64::try_from(took.len())
            .unwrap_or(u64::MAX)
            .saturating_sub(1))
    }
}

/// Pictures put up on the layer of generation 1 as `sequence`s 0, 1 and 2.
fn flown(glass: &Glass) {
    let mut book = glass.book.lock();
    for sequence in 0..3 {
        assert_eq!(book.pacer.offer(test_stamp(sequence)), Offer::Present);
        let stamp = book.pacer.painted().expect("put up");
        book.flying.push_back(Flying {
            generation: 1,
            sequence,
            stamp,
        });
    }
}

/// The layer's report stops its picture's clock at the glass, and one its mailbox replaced
/// counts as replaced. A report from a replaced layer, a second report and one for a picture
/// never put up change nothing; nor does one after the glass is gone.
#[test]
fn a_report_times_its_picture_or_counts_it_replaced() {
    let glass = Glass::new();
    flown(&glass);
    let weak = Arc::downgrade(&glass);
    reported(&weak, 2, 0, frame(Some(Instant::now())));
    assert_eq!(glass.pacing().presented, 0, "another layer's");
    glass.book.lock().presenting = Some(std::thread::current().id());
    reported(&weak, 1, 0, frame(None));
    glass.book.lock().presenting = None;
    reported(&weak, 1, 1, frame(Some(Instant::now())));
    reported(&weak, 1, 1, frame(Some(Instant::now())));
    reported(&weak, 1, 7, frame(Some(Instant::now())));
    assert_eq!((glass.pacing().presented, glass.replaced()), (1, 1));
    assert_eq!(
        glass.book.lock().flying.len(),
        1,
        "the third, still on its way"
    );
    drop(glass);
    reported(&weak, 1, 2, frame(None));
}

/// A report with no time from anywhere but inside a present is a picture drawn that the
/// display did not time: left untimed, not counted against the stream.
#[test]
fn an_untimed_report_is_replaced_only_inside_a_present() {
    let glass = Glass::new();
    flown(&glass);
    let weak = Arc::downgrade(&glass);
    reported(&weak, 1, 0, frame(None));
    assert_eq!(glass.replaced(), 0, "drawn, untimed by the display");
    assert_eq!(glass.book.lock().flying.len(), 2, "and forgotten");
}

/// With no layer the newest picture waits, and the view hears its shape; one older than the
/// picture waiting is dropped. The first layer attached takes the newest at once, and the
/// pictures after it go straight up.
#[test]
fn pictures_wait_for_a_layer_and_go_to_the_first() {
    let glass = Glass::new();
    let shapes = glass.shapes();
    glass.offer(picture(64, 48), test_stamp(0));
    glass.offer(picture(32, 16), test_stamp(1));
    glass.offer(picture(64, 48), test_stamp(0));
    assert_eq!(glass.put_up(), 0);
    assert_eq!(
        shapes.borrow().map(|shape| shape.size),
        Some((32, 16)),
        "not the late one's"
    );
    let layer = TestLayer::default();
    let (generation, _sink) = glass.reports();
    glass.attach(Box::new(layer.clone()), generation);
    glass.offer(picture(80, 60), test_stamp(2));
    assert_eq!(*layer.took.lock(), [(32, 16), (80, 60)]);
    assert_eq!(glass.put_up(), 2);
    glass.detach();
    glass.offer(picture(80, 60), test_stamp(3));
    assert_eq!(glass.put_up(), 2, "no layer, no picture up");
}

/// A striped picture's shape is both stripes' shown rows together, with where they meet; a
/// picture of one piece has no seam.
#[test]
fn a_striped_pictures_shape_is_its_shown_rows() {
    // 2160 rows: the seam at 1088, each stripe coding 64 rows past it.
    let (top, lower) = (picture(3840, 1152).top, picture(3840, 1136).top);
    let striped = Picture::striped(top, 1088, lower, 64);
    let shape = Shape::of(&striped);
    assert_eq!(shape.size, (3840, 2160));
    assert_eq!(
        shape.seam,
        Some(Seam {
            top: 1152,
            top_rows: 1088,
            lower: 1136,
            lower_from: 64
        })
    );
    assert_eq!(Shape::of(&picture(800, 600)).seam, None);
    assert_eq!(Shape::of(&picture(800, 600)).chroma, Chroma::Subsampled);
}

/// A picture goes up only on layers placed for where its stripes meet: before the view has
/// placed any, anything goes; after, a whole picture waits for a layout without a seam, a
/// striped one for its own seam, and a resize that moves the seam waits too.
#[test]
fn a_picture_waits_for_layers_placed_for_its_seam() {
    let striped = |h_top, top_rows, h_lower| {
        Picture::striped(
            picture(64, h_top).top,
            top_rows,
            picture(64, h_lower).top,
            64,
        )
    };
    let mut slot = Slot {
        last: Some(striped(576, 512, 560)),
        ..Slot::default()
    };
    assert!(slot.fits(), "nothing placed yet");
    slot.placed = Some(None);
    assert!(!slot.fits(), "laid out for one picture");
    slot.placed = Some(Shape::of(&striped(576, 512, 560)).seam);
    assert!(slot.fits());
    slot.last = Some(striped(640, 576, 560));
    assert!(!slot.fits(), "the seam moved");
    slot.last = Some(picture(64, 1024));
    assert!(!slot.fits(), "a whole picture on striped layers");
    slot.placed = Some(None);
    assert!(slot.fits());

    let glass = Glass::new();
    glass.place(Shape::of(&striped(576, 512, 560)).seam);
    let layer = TestLayer::default();
    let (generation, _sink) = glass.reports();
    glass.attach(Box::new(layer.clone()), generation);
    glass.offer(picture(64, 1024), test_stamp(0));
    assert!(glass.slot.lock().waiting, "held for the layout");
    glass.place(None);
    assert!(!glass.slot.lock().waiting, "up once placed");
    assert_eq!(*layer.took.lock(), [(64, 1024)]);
}

/// A capture whose two stripes the layers showed within [`SPLIT_AFTER`] of each other went up
/// together; one shown a refresh later, or only one of them shown, split. One both layers
/// dropped for a newer capture counts neither way, and a report of a picture no pair names
/// changes nothing.
#[test]
fn a_capture_counts_its_stripes_together_or_split() {
    let at = Instant::now();
    let later = |ms: u64| {
        at.checked_add(Duration::from_millis(ms))
            .expect("a later time")
    };
    let mut book = Book {
        pacer: Pacer::default(),
        flying: VecDeque::new(),
        pairs: VecDeque::new(),
        seams: Seams::default(),
        replaced: 0,
        presenting: None,
        handle: None,
    };
    for sequence in 0..4 {
        book.pairs.push_back(Pair {
            stripes: [(1, sequence), (2, sequence)],
            reports: [None, None],
        });
    }
    book.stripe_shown(0, (1, 0), Some(at));
    assert_eq!(
        book.seams,
        Seams::default(),
        "the lower stripe still to report"
    );
    book.stripe_shown(1, (2, 0), Some(later(1)));
    book.stripe_shown(1, (2, 1), Some(later(9)));
    book.stripe_shown(0, (1, 1), Some(at));
    book.stripe_shown(0, (1, 2), Some(at));
    book.stripe_shown(1, (2, 2), None);
    book.stripe_shown(0, (1, 3), None);
    book.stripe_shown(1, (2, 3), None);
    book.stripe_shown(1, (2, 9), Some(at));
    assert_eq!(
        book.seams,
        Seams {
            together: 1,
            split: 2
        }
    );
    assert!(book.pairs.is_empty(), "every pair reported");
}
