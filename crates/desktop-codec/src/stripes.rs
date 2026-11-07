//! One capture coded as two horizontal stripes, a session each, on two encode engines.
//!
//! A picture larger than one engine's pixel rate then runs on both (`docs/decisions/video.md`,
//! "Two stripes halve the encode at 3K and above" and "A large stream takes both encode engines
//! while it has them").
//!
//! - [`layout`]: where the seam falls and which rows each stripe codes and shows. The worker and
//!   the client both call it, so they agree without the wire saying more than the height.
//! - `StripeCopy` (macOS): a stripe's coded rows copied out of the capture into a picture of its
//!   own, `IOSurface`-backed so the session codes it inside the submit.
//!
//! Which streams are striped is the host's call, by their size alone (`onicode-desktopd`'s
//! `stripes`).

/// Rows each stripe codes past the seam, one HEVC coding tree unit.
///
/// Scrolled content keeps its reference inside the stripe, and the seam's rows code within
/// 0.4 dB of the whole picture's (MEASUREMENTS.md, "stripes across the two encode engines").
pub const OVERLAP: u32 = 64;
const _: () = assert!(
    OVERLAP.is_multiple_of(16),
    "a stripe's coded rows stay a multiple of 16 when the picture's are"
);

/// What a stripe codes and shows of a picture, in rows from its top.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Stripe {
    /// The first row the stripe's session codes.
    pub coded_top: u32,
    /// Rows the stripe's session codes: its picture's height.
    pub coded_rows: u32,
    /// The first row of the picture the stripe shows.
    pub shown_top: u32,
    /// Rows of the picture the stripe shows.
    pub shown_rows: u32,
}

impl Stripe {
    /// Rows of the stripe's own picture above the ones it shows: the overlap it codes above
    /// the seam, 0 for the top stripe.
    #[must_use]
    pub const fn shown_from(&self) -> u32 {
        self.shown_top.saturating_sub(self.coded_top)
    }
}

/// The two stripes of a picture `height` rows high, top first.
///
/// The seam is the multiple of 64 nearest half (the lower one on a tie), and each stripe codes
/// [`OVERLAP`] rows past it. Every coded height is a multiple of 16 when `height` is, as the
/// worker's padded pictures are. `None` for a picture too short to split, under four coding tree
/// units.
#[must_use]
pub const fn layout(height: u32) -> Option<[Stripe; 2]> {
    if height < OVERLAP.saturating_mul(4) {
        return None;
    }
    let seam = ((height / 2).saturating_add(OVERLAP / 2 - 1) / OVERLAP).saturating_mul(OVERLAP);
    let lower_top = seam.saturating_sub(OVERLAP);
    Some([
        Stripe {
            coded_top: 0,
            coded_rows: seam.saturating_add(OVERLAP),
            shown_top: 0,
            shown_rows: seam,
        },
        Stripe {
            coded_top: lower_top,
            coded_rows: height.saturating_sub(lower_top),
            shown_top: seam,
            shown_rows: height.saturating_sub(seam),
        },
    ])
}

#[cfg(target_os = "macos")]
pub use imp::StripeCopy;

#[cfg(target_os = "macos")]
mod imp {
    use std::ptr::{self, NonNull};

    use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CFType};
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
        CVPixelBufferGetHeight, CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPixelFormatType,
        CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
        CVPixelBufferPool, CVPixelBufferUnlockBaseAddress, kCVPixelBufferHeightKey,
        kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey,
        kCVPixelBufferWidthKey,
    };

    use super::Stripe;
    use crate::CodecError;
    use crate::cf::check;
    use crate::encoder::pixel_format;
    use crate::video::Chroma;

    /// The attributes of an `IOSurface`-backed picture `width` × `height` in `format`.
    fn surface_attributes(
        width: usize,
        height: usize,
        format: u32,
    ) -> CFRetained<CFDictionary<CFString, CFType>> {
        let none = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let width = CFNumber::new_i64(i64::try_from(width).unwrap_or(i64::MAX));
        let height = CFNumber::new_i64(i64::try_from(height).unwrap_or(i64::MAX));
        let format = CFNumber::new_i64(i64::from(format));
        // SAFETY: framework-provided constant strings.
        let keys = unsafe {
            [
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferWidthKey,
                kCVPixelBufferHeightKey,
                kCVPixelBufferPixelFormatTypeKey,
            ]
        };
        CFDictionary::<CFString, CFType>::from_slices(&keys, &[&*none, &*width, &*height, &*format])
    }

    /// A picture's base address locked for as long as this lives.
    struct Locked<'a> {
        buffer: &'a CVPixelBuffer,
        flags: CVPixelBufferLockFlags,
    }

    impl<'a> Locked<'a> {
        fn new(
            buffer: &'a CVPixelBuffer,
            flags: CVPixelBufferLockFlags,
        ) -> Result<Self, CodecError> {
            // SAFETY: CoreVideo's rule for `CVPixelBufferLockBaseAddress`: a valid buffer; the
            // unlock with the same flags is in `Drop`.
            let status = unsafe { CVPixelBufferLockBaseAddress(buffer, flags) };
            check("CVPixelBufferLockBaseAddress", status)?;
            Ok(Self { buffer, flags })
        }

        /// Plane `plane`'s first byte, bytes a row, and rows.
        fn plane(&self, plane: usize) -> (*mut u8, usize, usize) {
            (
                CVPixelBufferGetBaseAddressOfPlane(self.buffer, plane).cast::<u8>(),
                CVPixelBufferGetBytesPerRowOfPlane(self.buffer, plane),
                CVPixelBufferGetHeightOfPlane(self.buffer, plane),
            )
        }
    }

    impl Drop for Locked<'_> {
        fn drop(&mut self) {
            // SAFETY: matches the lock in `new`, with the same flags.
            let _unlocked = unsafe { CVPixelBufferUnlockBaseAddress(self.buffer, self.flags) };
        }
    }

    /// One stripe's pictures: its coded rows of a capture, copied into `IOSurface`-backed
    /// buffers from a pool of its own.
    ///
    /// A session codes an `IOSurface`-backed picture whose sides are multiples of 16 inside the
    /// submit and keeps nothing after it; given any other buffer it copies it and codes the copy
    /// behind a queue (MEASUREMENTS.md, "Stream sides padded to 16"). A stripe cannot be a view
    /// on the capture's surface, so its rows are copied, a plain copy of each plane's rows.
    ///
    /// A stripe takes the captures its session takes. A 4:4:4 session takes only 4:4:4. A 4:2:0
    /// session takes 4:4:4 as well, as the whole picture's does: ScreenCaptureKit goes on
    /// delivering 4:4:4 for a few frames after a stream switches back to 4:2:0, and the session
    /// converts them.
    pub struct StripeCopy {
        /// A pool for each capture format the stripe takes, its session's own first.
        pools: Vec<(u32, CFRetained<CVPixelBufferPool>)>,
        /// The captures' size.
        size: (usize, usize),
        stripe: Stripe,
        chroma: Chroma,
    }

    impl std::fmt::Debug for StripeCopy {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("StripeCopy")
                .field("size", &self.size)
                .field("stripe", &self.stripe)
                .field("chroma", &self.chroma)
                .finish_non_exhaustive()
        }
    }

    // SAFETY: a `CVPixelBufferPool` may be used from any thread (CoreVideo's pools are
    // thread-safe: `CVPixelBufferPool.h` documents no thread affinity, and VideoToolbox's own
    // sessions hand theirs to their callers' threads); nothing else here is shared.
    unsafe impl Send for StripeCopy {}
    // SAFETY: as above; `copy` only asks a pool for a new buffer.
    unsafe impl Sync for StripeCopy {}

    /// A pool of `IOSurface`-backed pictures `width` × `rows` in `format`.
    fn pool(
        width: usize,
        rows: usize,
        format: u32,
    ) -> Result<CFRetained<CVPixelBufferPool>, CodecError> {
        let attributes = surface_attributes(width, rows, format);
        let mut raw: *mut CVPixelBufferPool = ptr::null_mut();
        // SAFETY: CoreVideo's rule for `CVPixelBufferPoolCreate`: a valid out-pointer and a
        // dictionary of `kCVPixelBuffer*` keys; the pool comes back owned (+1).
        let status = unsafe {
            CVPixelBufferPool::create(
                None,
                None,
                Some(attributes.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        check("CVPixelBufferPoolCreate", status)?;
        let raw = NonNull::new(raw).ok_or(CodecError::Os {
            call: "CVPixelBufferPoolCreate",
            status: -1,
        })?;
        // SAFETY: owned (+1), as above.
        Ok(unsafe { CFRetained::from_raw(raw) })
    }

    impl StripeCopy {
        /// Pictures of `stripe`'s coded rows of `width` × `height` captures, for a session
        /// carrying `chroma`.
        pub fn new(
            (width, height): (usize, usize),
            stripe: Stripe,
            chroma: Chroma,
        ) -> Result<Self, CodecError> {
            let rows = usize::try_from(stripe.coded_rows).unwrap_or(usize::MAX);
            let top = usize::try_from(stripe.coded_top).unwrap_or(usize::MAX);
            if top.saturating_add(rows) > height {
                return Err(CodecError::WrongSize {
                    image: (width, height),
                    session: (width, top.saturating_add(rows)),
                });
            }
            let formats: &[Chroma] = match chroma {
                Chroma::Subsampled => &[Chroma::Subsampled, Chroma::Full],
                Chroma::Full => &[Chroma::Full],
            };
            let pools = formats
                .iter()
                .map(|&chroma| {
                    let format = pixel_format(chroma);
                    Ok((format, pool(width, rows, format)?))
                })
                .collect::<Result<_, CodecError>>()?;
            Ok(Self {
                pools,
                size: (width, height),
                stripe,
                chroma,
            })
        }

        /// The stripe this copies.
        #[must_use]
        pub const fn stripe(&self) -> Stripe {
            self.stripe
        }

        /// The stripe's coded rows of `capture`, both planes, as a picture of its own, with the
        /// capture's attachments (its colour, its timing) carried over.
        pub fn copy(
            &self,
            capture: &CVPixelBuffer,
        ) -> Result<CFRetained<CVPixelBuffer>, CodecError> {
            let format = CVPixelBufferGetPixelFormatType(capture);
            let Some((_, pool)) = self.pools.iter().find(|(taken, _)| *taken == format) else {
                return Err(match self.chroma {
                    Chroma::Full => CodecError::NotFullChroma(format),
                    Chroma::Subsampled => CodecError::PixelFormat {
                        expected: pixel_format(Chroma::Subsampled),
                        got: format,
                    },
                });
            };
            let top = usize::try_from(self.stripe.coded_top).unwrap_or(usize::MAX);
            let rows = usize::try_from(self.stripe.coded_rows).unwrap_or(usize::MAX);
            let size = (
                CVPixelBufferGetWidth(capture),
                CVPixelBufferGetHeight(capture),
            );
            // Exactly the size the stripes were laid out for: rows of a capture of another
            // height are not this stripe's rows.
            if size != self.size {
                return Err(CodecError::WrongSize {
                    image: size,
                    session: self.size,
                });
            }
            let mut raw: *mut CVPixelBuffer = ptr::null_mut();
            // SAFETY: CoreVideo's rule for `CVPixelBufferPoolCreatePixelBuffer`: a valid pool
            // and out-pointer; the buffer comes back owned (+1).
            let status = unsafe {
                CVPixelBufferPool::create_pixel_buffer(None, pool, NonNull::from(&mut raw))
            };
            check("CVPixelBufferPoolCreatePixelBuffer", status)?;
            let raw = NonNull::new(raw).ok_or(CodecError::Os {
                call: "CVPixelBufferPoolCreatePixelBuffer",
                status: -1,
            })?;
            // SAFETY: owned (+1), as above.
            let stripe = unsafe { CFRetained::from_raw(raw) };
            {
                let from = Locked::new(capture, CVPixelBufferLockFlags::ReadOnly)?;
                let to = Locked::new(&stripe, CVPixelBufferLockFlags::empty())?;
                for plane in 0..2 {
                    // Both formats are bi-planar; 4:2:0 halves the chroma plane's rows.
                    let (first, count) = match (plane, format == pixel_format(Chroma::Full)) {
                        (1, false) => (top / 2, rows / 2),
                        _ => (top, rows),
                    };
                    copy_rows(from.plane(plane), to.plane(plane), first, count)?;
                }
            }
            capture.propagate_attachments(&stripe);
            Ok(stripe)
        }
    }

    /// Copy `count` rows of a locked plane from row `first` into the top of another: one copy
    /// when both lay their rows out alike, else a copy a row.
    fn copy_rows(
        (from, from_stride, from_rows): (*mut u8, usize, usize),
        (to, to_stride, to_rows): (*mut u8, usize, usize),
        first: usize,
        count: usize,
    ) -> Result<(), CodecError> {
        let unmapped = CodecError::Os {
            call: "CVPixelBufferGetBaseAddressOfPlane",
            status: -1,
        };
        let (Some(from_len), Some(to_len)) = (
            from_stride.checked_mul(from_rows),
            to_stride.checked_mul(to_rows),
        ) else {
            return Err(unmapped);
        };
        if from.is_null() || to.is_null() {
            return Err(unmapped);
        }
        // SAFETY: CoreVideo's rule for a locked planar buffer: plane `p`'s base address is
        // valid for `bytes per row × height of plane` bytes until the unlock, which the callers'
        // `Locked` guards hold off; the source is only read.
        let from = unsafe { std::slice::from_raw_parts(from.cast_const(), from_len) };
        // SAFETY: as above, the destination plane, locked for writing, and a different buffer
        // from the source, so the two slices do not overlap.
        let to = unsafe { std::slice::from_raw_parts_mut(to, to_len) };
        let rows = |stride: usize, first: usize, count: usize| {
            Some(stride.checked_mul(first)?..stride.checked_mul(first.checked_add(count)?)?)
        };
        if from_stride == to_stride {
            let (Some(src), Some(dst)) = (
                rows(from_stride, first, count).and_then(|r| from.get(r)),
                rows(to_stride, 0, count).and_then(|r| to.get_mut(r)),
            ) else {
                return Err(unmapped);
            };
            dst.copy_from_slice(src);
            return Ok(());
        }
        let row = from_stride.min(to_stride);
        let (Some(src), Some(dst)) = (
            rows(from_stride, first, count).and_then(|r| from.get(r)),
            rows(to_stride, 0, count).and_then(|r| to.get_mut(r)),
        ) else {
            return Err(unmapped);
        };
        for (src, dst) in src
            .chunks_exact(from_stride)
            .zip(dst.chunks_exact_mut(to_stride))
        {
            if let (Some(src), Some(dst)) = (src.get(..row), dst.get_mut(..row)) {
                dst.copy_from_slice(src);
            }
        }
        Ok(())
    }

    /// An `IOSurface`-backed picture of blocks like text, scrolled `scroll` × 4 rows, so each
    /// frame is work for the encoder as a changing screen is.
    #[cfg(test)]
    fn picture(
        width: usize,
        height: usize,
        scroll: usize,
        chroma: Chroma,
    ) -> Result<CFRetained<CVPixelBuffer>, CodecError> {
        use objc2_core_video::CVPixelBufferCreate;

        let format = pixel_format(chroma);
        let attributes = surface_attributes(width, height, format);
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo's rule for `CVPixelBufferCreate`: a valid out-pointer and a
        // dictionary of `kCVPixelBuffer*` keys; the buffer comes back owned (+1).
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                width,
                height,
                format,
                Some(attributes.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        check("CVPixelBufferCreate", status)?;
        let raw = NonNull::new(raw).ok_or(CodecError::Os {
            call: "CVPixelBufferCreate",
            status: -1,
        })?;
        // SAFETY: owned (+1), as above.
        let buffer = unsafe { CFRetained::from_raw(raw) };
        {
            let locked = Locked::new(&buffer, CVPixelBufferLockFlags::empty())?;
            for plane in 0..2 {
                let (base, stride, rows) = locked.plane(plane);
                if base.is_null() {
                    return Err(CodecError::Os {
                        call: "CVPixelBufferGetBaseAddressOfPlane",
                        status: -1,
                    });
                }
                let len = stride.checked_mul(rows).unwrap_or(0);
                // SAFETY: CoreVideo's rule for a locked planar buffer: the plane's base address
                // is valid for `bytes per row × height of plane` bytes until the unlock, which
                // `locked` holds off.
                let bytes = unsafe { std::slice::from_raw_parts_mut(base, len) };
                for (y, row) in bytes.chunks_exact_mut(stride.max(1)).enumerate() {
                    let scrolled = y.wrapping_add(scroll.wrapping_mul(4));
                    let line = scrolled / 16;
                    for (x, byte) in row.iter_mut().enumerate() {
                        let cell = line
                            .wrapping_mul(131)
                            .wrapping_add((x / 8).wrapping_mul(71));
                        let shade = x.wrapping_mul(7).wrapping_add(line.wrapping_mul(13)) % 64;
                        *byte = if plane == 1 {
                            0x80
                        } else if cell % 97 > 20 && scrolled % 16 > 2 {
                            u8::try_from(shade).unwrap_or(0) | 0x20
                        } else {
                            0xe0
                        };
                    }
                }
            }
        }
        Ok(buffer)
    }

    #[cfg(test)]
    mod tests {
        use std::time::{Duration, Instant};

        use onicode_desktop_media::wire::VideoCodec;

        use super::*;
        use crate::PixelBuffer;
        use crate::stripes::layout;
        use crate::video::{EncoderConfig, FrameOptions};

        /// Each stripe's picture holds exactly its coded rows of the capture, in both planes
        /// and both chroma formats (a 4:2:0 session's 4:4:4 capture too), and the two stripes
        /// together cover every row.
        #[test]
        fn a_stripe_holds_its_rows_of_the_capture() {
            let pairs = [
                (Chroma::Subsampled, Chroma::Subsampled),
                (Chroma::Full, Chroma::Full),
                (Chroma::Subsampled, Chroma::Full),
            ];
            for (session, chroma) in pairs {
                let (w, h) = (256_usize, 512_usize);
                let capture = picture(w, h, 3, chroma).unwrap();
                let stripes = layout(512).unwrap();
                for stripe in stripes {
                    let copy = StripeCopy::new((w, h), stripe, session).unwrap();
                    let out = copy.copy(&capture).unwrap();
                    assert_eq!(
                        (CVPixelBufferGetWidth(&out), CVPixelBufferGetHeight(&out)),
                        (w, stripe.coded_rows as usize)
                    );
                    let from = Locked::new(&capture, CVPixelBufferLockFlags::ReadOnly).unwrap();
                    let to = Locked::new(&out, CVPixelBufferLockFlags::ReadOnly).unwrap();
                    for plane in 0..2 {
                        let (src, src_stride, src_rows) = from.plane(plane);
                        let (dst, dst_stride, dst_rows) = to.plane(plane);
                        // SAFETY: CoreVideo's rule for a locked planar buffer: the plane is
                        // readable for `bytes per row × height` bytes until the unlock.
                        let src = unsafe { std::slice::from_raw_parts(src, src_stride * src_rows) };
                        // SAFETY: as above, the stripe's plane.
                        let dst = unsafe { std::slice::from_raw_parts(dst, dst_stride * dst_rows) };
                        let first = if plane == 1 && chroma == Chroma::Subsampled {
                            stripe.coded_top as usize / 2
                        } else {
                            stripe.coded_top as usize
                        };
                        let row_bytes = src_stride.min(dst_stride);
                        for r in 0..dst_rows {
                            let a = &src[(first + r) * src_stride..][..row_bytes];
                            let b = &dst[r * dst_stride..][..row_bytes];
                            assert_eq!(a, b, "{chroma:?} plane {plane} row {r} of {stripe:?}");
                        }
                    }
                }
            }
        }

        /// A capture of another size is refused, not coded wrong: a taller one of the same
        /// width too, whose rows under the seam are not this stripe's (a resize the sessions
        /// have not followed yet). A 4:4:4 session refuses 4:2:0 as the whole picture's does.
        #[test]
        fn a_capture_that_does_not_fit_is_refused() {
            for stripe in layout(512).unwrap() {
                let copy = StripeCopy::new((256, 512), stripe, Chroma::Subsampled).unwrap();
                for (w, h) in [(128, 512), (256, 448), (256, 528), (272, 512)] {
                    let other = picture(w, h, 0, Chroma::Subsampled).unwrap();
                    assert!(
                        matches!(copy.copy(&other), Err(CodecError::WrongSize { .. })),
                        "{w}×{h} into {stripe:?}"
                    );
                }
                let full = StripeCopy::new((256, 512), stripe, Chroma::Full).unwrap();
                let subsampled = picture(256, 512, 0, Chroma::Subsampled).unwrap();
                assert!(matches!(
                    full.copy(&subsampled),
                    Err(CodecError::NotFullChroma(_))
                ));
            }
            assert!(matches!(
                StripeCopy::new((256, 448), layout(512).unwrap()[1], Chroma::Subsampled),
                Err(CodecError::WrongSize { .. })
            ));
        }

        /// A 4:2:0 stripe's session codes a 4:4:4 capture, as the whole picture's does: the
        /// captures of the few frames after a stream switches back from 4:4:4.
        #[test]
        fn a_subsampled_stripe_codes_a_full_chroma_capture() {
            if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
                return;
            }
            use crate::video::VideoEncoder as _;
            let (w, h) = (256_u32, 512_u32);
            let config = EncoderConfig {
                width: w,
                height: h,
                codec: VideoCodec::Hevc,
                fps: 60,
                bitrate_bps: 4_000_000,
                chroma: Chroma::Subsampled,
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let stripe = layout(h).unwrap()[1];
            let session = crate::VideoToolbox::stripe(config, stripe, move |packet| {
                let _gone = tx.send(packet.keyframe);
            })
            .unwrap();
            let full = PixelBuffer::from_retained(picture(256, 512, 0, Chroma::Full).unwrap());
            let options = FrameOptions {
                force_keyframe: true,
                ..FrameOptions::default()
            };
            session.encode(&full, 1, &options).unwrap();
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(5)),
                Ok(true),
                "its keyframe"
            );
        }

        /// What a stripe's copy costs at the sizes stripes are for: run by hand (MEASUREMENTS.md,
        /// "large streams on the encode engines").
        #[test]
        #[ignore = "a measurement; run with --run-ignored only --no-capture"]
        fn stripes_copy_cost() {
            for (w, h) in [(3024_u32, 1968_u32), (3840, 2160), (5120, 2880)] {
                for chroma in [Chroma::Subsampled, Chroma::Full] {
                    let capture = picture(w as usize, h as usize, 1, chroma).unwrap();
                    let copies: Vec<StripeCopy> = layout(h)
                        .unwrap()
                        .iter()
                        .map(|&s| StripeCopy::new((w as usize, h as usize), s, chroma).unwrap())
                        .collect();
                    let mut took: Vec<Duration> = std::iter::repeat_with(|| {
                        let started = Instant::now();
                        for copy in &copies {
                            std::hint::black_box(copy.copy(&capture).unwrap());
                        }
                        started.elapsed()
                    })
                    .take(200)
                    .collect();
                    took.sort_unstable();
                    eprintln!(
                        "MEASURE stripes {w}x{h} {chroma:?}: both copies p50 {:?} p95 {:?} max {:?}",
                        took[took.len() / 2],
                        took[took.len() * 95 / 100],
                        took[took.len() - 1],
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seams the design names at the three sizes stripes are for, the coded rows each a
    /// multiple of 16, and a picture too short to split.
    #[test]
    fn the_seam_falls_on_a_coding_tree_unit_near_half() {
        let rows =
            |h| layout(h).map(|[a, b]| (a.shown_rows, b.shown_rows, a.coded_rows, b.coded_rows));
        assert_eq!(rows(1968), Some((960, 1008, 1024, 1072)));
        assert_eq!(rows(2160), Some((1088, 1072, 1152, 1136)));
        assert_eq!(rows(2880), Some((1408, 1472, 1472, 1536)));
        for h in (256..4400).step_by(16) {
            let [a, b] = layout(h).unwrap();
            assert_eq!(a.shown_rows + b.shown_rows, h);
            assert_eq!(b.coded_top + b.coded_rows, h);
            assert_eq!(a.coded_rows - a.shown_rows, OVERLAP);
            assert_eq!(b.shown_from(), OVERLAP);
            assert!(a.coded_rows % 16 == 0 && b.coded_rows % 16 == 0, "{h}");
            assert!(a.shown_rows.abs_diff(b.shown_rows) <= OVERLAP, "{h}");
        }
        assert_eq!(layout(255), None);
    }
}
