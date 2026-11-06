//! What VideoToolbox on this Mac offers past 4:2:0 HEVC, and what it costs (macOS only).
//!
//! Streams are 4:2:0, so coloured text on a streamed desktop is softer than a 4:4:4 stream
//! makes it. This probe asks the framework directly: the encoder list, each HEVC encoder's
//! supported `ProfileLevel` values, sessions fed 4:2:2 and 4:4:4 pictures under every profile,
//! the chroma format the resulting SPS actually signals, and whether the decoder takes the
//! stream in hardware. Then it times encodes at 1080p and 5K and prices a synthetic coloured
//! text frame in bits and PSNR. It opens no window and captures nothing. `MEASURE` lines go to
//! stderr; `docs/MEASUREMENTS.md` ("4:4:4 HEVC on the low-latency encoder") records a run.
//!
//! The same sessions also price a frame rate: the shipped 4:2:0 encoder fed the text picture
//! in real time at 60 and at 120 frames a second, the same scroll speed in both, at several
//! target rates (`frame_rate_120_against_60`, MEASUREMENTS "120 fps against 60").
//!
//! The worker's own session is probed three more ways (MEASUREMENTS, 2026-09-29): encode time
//! against frame size (`encode_time_by_size`), the compression presets and the keys the
//! low-latency encoder lists (`compression_presets`), and which frames it marks as references
//! (`temporal_layers`).
//!
//! Stream sides padded to 16 (MEASUREMENTS, 2026-09-29): the SPS conformance window rewritten on
//! a padded session against the encoder's own SPS for the true size, and what the decoder makes
//! of it (`conformance_window`, probe P1); the size sweep on padded sessions
//! (`ONICODE_PROBE_ALIGN=16`); and what a submit costs the calling thread and how many source
//! pictures the session holds (`submit_blocking_and_pictures_held`), and whether a session
//! opens the M2 scaler for a picture off 16 (`the_scaler_is_opened_only_off_16`).
//!
//! Three more on the worker's session (MEASUREMENTS, 2026-09-29):
//! - "temporal layers on the worker's session": which layer-1 frames a decoder can do without, the
//!   live toggle and what the layers cost (`temporal_layers_skip_and_toggle`, probe P5);
//! - "a still picture refined": what coding a still picture again buys, frame by frame
//!   (`still_picture_refinement`, probe P6);
//! - "stripes across the two encode engines": whether sessions on stripes of one picture run on
//!   both engines at once (`stripes_across_engines`, probe P3).

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests {
    #![allow(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        clippy::cast_possible_wrap,
        clippy::cast_lossless,
        clippy::too_many_lines,
        clippy::many_single_char_names,
        reason = "measurement fixture: pixel arithmetic on small, bounded values"
    )]

    use std::ffi::c_void;
    use std::ptr::{self, NonNull};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::{
        CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType,
    };
    use objc2_core_media::{
        CMFormatDescription, CMSampleBuffer, CMTime, CMTimeFlags,
        CMVideoFormatDescriptionGetHEVCParameterSetAtIndex,
        kCMHEVCTemporalLevelInfoKey_TemporalLevel, kCMSampleAttachmentKey_DependsOnOthers,
        kCMSampleAttachmentKey_HEVCTemporalLevelInfo, kCMSampleAttachmentKey_IsDependedOnByOthers,
        kCMSampleAttachmentKey_NotSync, kCMTimeInvalid, kCMVideoCodecType_H264,
        kCMVideoCodecType_HEVC,
    };
    use objc2_core_video::{
        CVImageBuffer, CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress,
        CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRow,
        CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
        CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
        CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
        kCVImageBufferCleanApertureHeightKey, kCVImageBufferCleanApertureHorizontalOffsetKey,
        kCVImageBufferCleanApertureVerticalOffsetKey, kCVImageBufferCleanApertureWidthKey,
        kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVPixelBufferIOSurfacePropertiesKey,
        kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA,
        kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
        kCVPixelFormatType_422YpCbCr8BiPlanarFullRange,
        kCVPixelFormatType_422YpCbCr10BiPlanarFullRange,
        kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
        kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
    };
    use objc2_video_toolbox::{
        VTCompressionSession, VTCopySupportedPropertyDictionaryForEncoder, VTCopyVideoEncoderList,
        VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
        VTDecompressionSession, VTEncodeInfoFlags, VTIsHardwareDecodeSupported, VTSession,
        VTSessionCopyProperty, VTSessionSetProperty, kVTCompressionPreset_VideoConferencing,
        kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AllowOpenGOP,
        kVTCompressionPropertyKey_AverageBitRate,
        kVTCompressionPropertyKey_BaseLayerBitRateFraction,
        kVTCompressionPropertyKey_BaseLayerFrameRateFraction,
        kVTCompressionPropertyKey_CalculateMeanSquaredError,
        kVTCompressionPropertyKey_CleanAperture, kVTCompressionPropertyKey_DataRateLimits,
        kVTCompressionPropertyKey_EnableLTR, kVTCompressionPropertyKey_ExpectedFrameRate,
        kVTCompressionPropertyKey_MaxFrameDelayCount,
        kVTCompressionPropertyKey_MaxKeyFrameInterval,
        kVTCompressionPropertyKey_MaximumRealTimeFrameRate,
        kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
        kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
        kVTCompressionPropertyKey_RecommendedParallelizationLimit,
        kVTCompressionPropertyKey_SupportedPresetDictionaries,
        kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
        kVTCompressionPropertyKey_YCbCrMatrix,
        kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
        kVTEncodeFrameOptionKey_AcknowledgedLTRTokens, kVTEncodeFrameOptionKey_ForceKeyFrame,
        kVTEncodeFrameOptionKey_ForceLTRRefresh, kVTProfileLevel_H264_High_AutoLevel,
        kVTProfileLevel_HEVC_Main_AutoLevel, kVTPropertySupportedValueListKey,
        kVTSampleAttachmentKey_QualityMetrics,
        kVTSampleAttachmentKey_RequireLTRAcknowledgementToken,
        kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
        kVTVideoEncoderList_CodecType, kVTVideoEncoderList_EncoderID,
        kVTVideoEncoderList_IsHardwareAccelerated,
        kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
        kVTVideoEncoderSpecification_EncoderID,
        kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
    };
    use onicode_desktop_codec::nal::hevc;
    use onicode_desktop_testkit::stats::Spread;

    // ---- CoreFoundation plumbing -------------------------------------------------------------

    type Dict = CFDictionary<CFString, CFType>;

    fn dict(pairs: &[(&CFString, &CFType)]) -> CFRetained<Dict> {
        let keys: Vec<&CFString> = pairs.iter().map(|p| p.0).collect();
        let values: Vec<&CFType> = pairs.iter().map(|p| p.1).collect();
        Dict::from_slices(&keys, &values)
    }

    fn yes() -> &'static CFType {
        CFBoolean::new(true)
    }

    fn fourcc(format: u32) -> String {
        format
            .to_be_bytes()
            .iter()
            .map(|&b| char::from(b))
            .collect()
    }

    fn as_session(object: &CFType) -> &VTSession {
        // SAFETY: VTSession.h: compression and decompression sessions are `VTSessionRef`s;
        // the reference is only read and lives as long as `object`.
        unsafe { NonNull::from(object).cast::<VTSession>().as_ref() }
    }

    fn set(session: &CFType, key: &CFString, value: &CFType) -> i32 {
        // SAFETY: valid session, key and value for the duration of the call.
        unsafe { VTSessionSetProperty(as_session(session), key, Some(value)) }
    }

    fn copy_bool(session: &CFType, key: &CFString) -> Option<bool> {
        let mut out: *const CFType = ptr::null();
        // SAFETY: VTSession.h: the value comes back +1 through a `CFTypeRef *` out pointer.
        let status = unsafe {
            VTSessionCopyProperty(
                as_session(session),
                key,
                None,
                (&raw mut out).cast::<c_void>(),
            )
        };
        let raw = NonNull::new(out.cast_mut()).filter(|_| status == 0)?;
        // SAFETY: +1 reference from the copy call (Copy rule).
        let value: CFRetained<CFType> = unsafe { CFRetained::from_raw(raw) };
        value.downcast::<CFBoolean>().ok().map(|b| b.as_bool())
    }

    fn text(value: &CFType) -> String {
        value
            .downcast_ref::<CFString>()
            .map_or_else(|| format!("{value:?}"), ToString::to_string)
    }

    /// The `CFString`s of a `CFArray` value, as the framework handed them over.
    fn strings(value: &CFType) -> Vec<CFRetained<CFString>> {
        value
            .downcast_ref::<CFArray>()
            .map(|array| {
                // SAFETY: CFArray of CFTypes; elements are read, not mutated.
                let array: &CFArray<CFType> = unsafe { array.cast_unchecked() };
                array
                    .iter()
                    .filter_map(|v| v.downcast::<CFString>().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    // ---- The encoder list and the per-encoder property dictionaries --------------------------

    fn encoder_list() -> Vec<(String, bool)> {
        let mut raw: *const CFArray = ptr::null();
        // SAFETY: valid out pointer; the list comes back +1 (Copy rule).
        let status = unsafe { VTCopyVideoEncoderList(None, NonNull::from(&mut raw)) };
        assert_eq!(status, 0, "VTCopyVideoEncoderList");
        // SAFETY: +1 reference; VTVideoEncoderList.h: an array of CFDictionaries keyed by CFString.
        let list: CFRetained<CFArray<Dict>> =
            unsafe { CFRetained::from_raw(NonNull::new(raw.cast_mut()).unwrap().cast()) };
        let mut out = Vec::new();
        for entry in list.iter() {
            // SAFETY: framework-provided constant strings.
            let (codec_key, id_key, hw_key) = unsafe {
                (
                    kVTVideoEncoderList_CodecType,
                    kVTVideoEncoderList_EncoderID,
                    kVTVideoEncoderList_IsHardwareAccelerated,
                )
            };
            let codec = entry
                .get(codec_key)
                .and_then(|v| v.downcast::<CFNumber>().ok())
                .and_then(|n| n.as_i64())
                .unwrap_or(0) as u32;
            if codec != kCMVideoCodecType_HEVC {
                continue;
            }
            let id = entry.get(id_key).map(|v| text(&v)).unwrap_or_default();
            let hw = entry
                .get(hw_key)
                .and_then(|v| v.downcast::<CFBoolean>().ok())
                .is_some_and(|b| b.as_bool());
            out.push((id, hw));
        }
        out
    }

    /// `(status, encoder id, ProfileLevel's supported values, every supported key)`.
    fn supported_for(
        width: i32,
        height: i32,
        spec: &Dict,
    ) -> (i32, String, Vec<CFRetained<CFString>>, Vec<String>) {
        let mut id: *const CFString = ptr::null();
        let mut props: *const CFDictionary = ptr::null();
        // SAFETY: valid out pointers; both results come back +1 (Copy rule).
        let status = unsafe {
            VTCopySupportedPropertyDictionaryForEncoder(
                width,
                height,
                kCMVideoCodecType_HEVC,
                Some(spec.as_opaque()),
                &raw mut id,
                &raw mut props,
            )
        };
        // SAFETY: +1 references from the copy call when non-null.
        let id = NonNull::new(id.cast_mut())
            .map(|p| unsafe { CFRetained::from_raw(p) }.to_string())
            .unwrap_or_default();
        let Some(props) = NonNull::new(props.cast_mut()) else {
            return (status, id, Vec::new(), Vec::new());
        };
        // SAFETY: +1 reference; VTSession.h: keyed by CFString, values are CFDictionaries.
        let props: CFRetained<Dict> = unsafe { CFRetained::from_raw(props.cast()) };
        // SAFETY: framework-provided constant strings.
        let (profile_key, list_key) = unsafe {
            (
                kVTCompressionPropertyKey_ProfileLevel,
                kVTPropertySupportedValueListKey,
            )
        };
        let profiles = props
            .get(profile_key)
            .and_then(|v| v.downcast::<CFDictionary>().ok())
            .and_then(|d| {
                // SAFETY: the per-property dictionary is keyed by CFString (VTSession.h).
                let d: &Dict = unsafe { d.cast_unchecked() };
                d.get(list_key).map(|list| strings(&list))
            })
            .unwrap_or_default();
        let (keys, _values) = props.to_vecs();
        let mut keys: Vec<String> = keys.iter().map(ToString::to_string).collect();
        keys.sort_unstable();
        (status, id, profiles, keys)
    }

    // ---- Synthetic coloured text -------------------------------------------------------------

    /// Monokai on its background: saturated hues on a dark ground, the worst case for 4:2:0.
    const PALETTE: [[u8; 3]; 7] = [
        [249, 38, 114],
        [166, 226, 46],
        [102, 217, 239],
        [253, 151, 31],
        [174, 129, 255],
        [230, 219, 116],
        [248, 248, 242],
    ];
    const BACKGROUND: [u8; 3] = [39, 40, 34];

    fn hash(mut x: u64) -> u64 {
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        x ^ (x >> 33)
    }

    /// A full-range BT.709 4:4:4 picture of a code editor: 8×16 cells, glyphs of one-pixel
    /// strokes, tokens coloured from the palette, ragged line ends. `scroll` shifts it up by
    /// that many pixels so consecutive frames differ the way a scrolling terminal does.
    struct Picture {
        w: usize,
        h: usize,
        y: Vec<f32>,
        cb: Vec<f32>,
        cr: Vec<f32>,
        rgb: Vec<[u8; 3]>,
    }

    fn ycbcr(rgb: [u8; 3]) -> [f32; 3] {
        let [r, g, b] = rgb.map(|c| f32::from(c) / 255.0);
        let y = 0.0722_f32.mul_add(b, 0.2126_f32.mul_add(r, 0.7152 * g));
        let cb = (b - y) / 1.8556;
        let cr = (r - y) / 1.5748;
        [
            y * 255.0,
            cb.mul_add(255.0, 128.0),
            cr.mul_add(255.0, 128.0),
        ]
    }

    fn picture(w: usize, h: usize, scroll: usize) -> Picture {
        let bg = ycbcr(BACKGROUND);
        let colours: Vec<([f32; 3], [u8; 3])> = PALETTE.iter().map(|&c| (ycbcr(c), c)).collect();
        let mut p = Picture {
            w,
            h,
            y: vec![bg[0]; w * h],
            cb: vec![bg[1]; w * h],
            cr: vec![bg[2]; w * h],
            rgb: vec![BACKGROUND; w * h],
        };
        let cols = w / 8;
        for py in 0..h {
            let sy = py + scroll;
            let (line, gy) = (sy / 16, sy % 16);
            let line_len = (hash(line as u64) % (cols as u64)) as usize;
            if !(2..14).contains(&gy) {
                continue;
            }
            for col in 0..cols.min(line_len) {
                let token = hash(((line as u64) << 20) | (col as u64 / 5));
                if token.is_multiple_of(7) {
                    continue; // a space between tokens
                }
                let glyph = hash(((line as u64) << 32) | col as u64 | (1 << 60));
                let colour = colours[(token % colours.len() as u64) as usize];
                for gx in 1..7 {
                    // A 6×12 glyph of strokes: two verticals, three horizontals, one diagonal.
                    let v1 = gx == 1 + (glyph % 3) as usize;
                    let v2 = gx == 4 + (glyph >> 2) as usize % 3;
                    let row = gy - 2;
                    let h1 = row == (glyph >> 4) as usize % 4;
                    let h2 = row == 5 + (glyph >> 6) as usize % 2;
                    let h3 = row == 11;
                    let d = (glyph >> 8).is_multiple_of(2) && row / 2 == gx - 1;
                    let on = (v1 && (glyph >> 9).is_multiple_of(2))
                        || (v2 && !(glyph >> 10).is_multiple_of(3))
                        || (h1 && (glyph >> 11).is_multiple_of(2))
                        || (h2 && !(glyph >> 12).is_multiple_of(3))
                        || (h3 && (glyph >> 13).is_multiple_of(2))
                        || d;
                    if on {
                        let i = py * w + col * 8 + gx;
                        [p.y[i], p.cb[i], p.cr[i]] = colour.0;
                        p.rgb[i] = colour.1;
                    }
                }
            }
        }
        p
    }

    /// `(horizontal, vertical)` chroma subsampling and bit depth of a bi-planar format.
    fn layout(format: u32) -> (usize, usize, u32) {
        match format {
            f if f == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange => (2, 2, 8),
            f if f == kCVPixelFormatType_420YpCbCr10BiPlanarFullRange => (2, 2, 10),
            f if f == kCVPixelFormatType_422YpCbCr8BiPlanarFullRange => (2, 1, 8),
            f if f == kCVPixelFormatType_422YpCbCr10BiPlanarFullRange => (2, 1, 10),
            f if f == kCVPixelFormatType_444YpCbCr8BiPlanarFullRange => (1, 1, 8),
            f if f == kCVPixelFormatType_444YpCbCr10BiPlanarFullRange => (1, 1, 10),
            other => panic!("no layout for {}", fourcc(other)),
        }
    }

    fn buffer(w: usize, h: usize, format: u32) -> CFRetained<CVPixelBuffer> {
        let attrs = dict(&[(
            // SAFETY: framework-provided constant string.
            unsafe { kCVPixelBufferIOSurfacePropertiesKey },
            &Dict::empty(),
        )]);
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo rule: a valid out-pointer and a CFString-keyed attributes dictionary.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                w,
                h,
                format,
                Some(attrs.as_opaque()),
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0, "CVPixelBufferCreate {}", fourcc(format));
        // SAFETY: +1 reference from the create call.
        unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) }
    }

    /// Run `f(plane, base, stride)` over each plane of a locked buffer.
    fn with_planes(
        image: &CVPixelBuffer,
        read_only: bool,
        mut f: impl FnMut(usize, *mut u8, usize),
    ) {
        let flags = if read_only {
            CVPixelBufferLockFlags::ReadOnly
        } else {
            CVPixelBufferLockFlags::empty()
        };
        // SAFETY: CoreVideo rule: lock before touching the planes.
        assert_eq!(
            unsafe { CVPixelBufferLockBaseAddress(image, flags) },
            0,
            "lock"
        );
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(image, plane).cast::<u8>();
            f(
                plane,
                base,
                CVPixelBufferGetBytesPerRowOfPlane(image, plane),
            );
        }
        // SAFETY: matches the lock above.
        assert_eq!(
            unsafe { CVPixelBufferUnlockBaseAddress(image, flags) },
            0,
            "unlock"
        );
    }

    /// The picture in `format`, chroma box-filtered to the format's subsampling the way a
    /// capture that delivers that format would.
    /// The picture as packed BGRA, what ScreenCaptureKit delivers by default.
    fn fill_bgra(p: &Picture) -> CFRetained<CVPixelBuffer> {
        let image = buffer(p.w, p.h, kCVPixelFormatType_32BGRA);
        // SAFETY: CoreVideo rule: lock before touching the pixels.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&image, CVPixelBufferLockFlags::empty()) };
        assert_eq!(locked, 0, "lock");
        let base = CVPixelBufferGetBaseAddress(&image).cast::<u8>();
        let stride = CVPixelBufferGetBytesPerRow(&image);
        for y in 0..p.h {
            // SAFETY: the buffer is locked; row `y < h` starts inside it.
            let start = unsafe { base.add(y * stride) };
            // SAFETY: the row spans `4 * w <= stride` writable bytes.
            let row = unsafe { std::slice::from_raw_parts_mut(start, 4 * p.w) };
            for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let [r, g, b] = p.rgb[y * p.w + x];
                *px = [b, g, r, 255];
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&image, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0, "unlock");
        image
    }

    fn fill(p: &Picture, format: u32) -> CFRetained<CVPixelBuffer> {
        if format == kCVPixelFormatType_32BGRA {
            return fill_bgra(p);
        }
        let (sx, sy, bits) = layout(format);
        let image = buffer(p.w, p.h, format);
        let scale = if bits == 8 { 1.0 } else { 4.0 };
        with_planes(&image, false, |plane, base, stride| {
            let (cw, ch) = if plane == 0 {
                (p.w, p.h)
            } else {
                (p.w / sx, p.h / sy)
            };
            for y in 0..ch {
                for x in 0..cw {
                    let values: [f32; 2] = if plane == 0 {
                        [p.y[y * p.w + x], 0.0]
                    } else {
                        let mut acc = [0.0_f32; 2];
                        for dy in 0..sy {
                            for dx in 0..sx {
                                let i = (y * sy + dy) * p.w + x * sx + dx;
                                acc[0] += p.cb[i];
                                acc[1] += p.cr[i];
                            }
                        }
                        acc.map(|a| a / (sx * sy) as f32)
                    };
                    let count = if plane == 0 { 1 } else { 2 };
                    for (k, v) in values.iter().take(count).enumerate() {
                        let q = (v * scale).round().clamp(0.0, 255.0 * scale) as u16;
                        let at = if plane == 0 { x } else { x * 2 + k };
                        if bits == 8 {
                            // SAFETY: the plane is locked; `y < ch` rows of `stride` bytes and
                            // `at < stride` stay inside it.
                            let cell = unsafe { base.add(y * stride + at) };
                            // SAFETY: one writable byte of the locked plane.
                            unsafe { cell.write(q as u8) }
                        } else {
                            // 10 bits in the high bits of a little-endian u16 (CVPixelBuffer.h).
                            let bytes = (q << 6).to_le_bytes();
                            // SAFETY: as above, two bytes per sample.
                            let cell = unsafe { base.add(y * stride + at * 2) };
                            // SAFETY: two writable bytes of the locked plane.
                            unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), cell, 2) }
                        }
                    }
                }
            }
        });
        image
    }

    /// PSNR of the decoded picture against the 4:4:4 source, `(luma, chroma)` in dB. Chroma is
    /// read at the decoded buffer's own subsampling and replicated to full resolution, the way
    /// a nearest sampler would show it.
    fn psnr(p: &Picture, image: &CVPixelBuffer) -> (f64, f64) {
        psnr_at(p, image, 0)
    }

    /// [`psnr`] of `p` against the decoded rows from `first_row` down.
    fn psnr_at(p: &Picture, image: &CVPixelBuffer, first_row: usize) -> (f64, f64) {
        let format = CVPixelBufferGetPixelFormatType(image);
        let (sx, sy, bits) = layout(format);
        let (mut ey, mut ec) = (0.0_f64, 0.0_f64);
        with_planes(image, true, |plane, base, stride| {
            let read = |x: usize, y: usize, k: usize, cx: usize| -> f64 {
                if bits == 8 {
                    // SAFETY: the plane is locked and the offset is inside it.
                    let cell = unsafe { base.add(y * stride + x * cx + k) };
                    // SAFETY: one readable byte of the locked plane.
                    f64::from(unsafe { cell.read() })
                } else {
                    let mut b = [0_u8; 2];
                    // SAFETY: as above, two bytes per sample.
                    let cell = unsafe { base.add(y * stride + (x * cx + k) * 2) };
                    // SAFETY: two readable bytes of the locked plane.
                    unsafe { ptr::copy_nonoverlapping(cell, b.as_mut_ptr(), 2) }
                    f64::from(u16::from_le_bytes(b) >> 6) / 4.0
                }
            };
            for y in 0..p.h {
                for x in 0..p.w {
                    let i = y * p.w + x;
                    if plane == 0 {
                        let d = read(x, first_row + y, 0, 1) - f64::from(p.y[i]);
                        ey = f64::mul_add(d, d, ey);
                    } else {
                        let (cx, cy) = (x / sx, (first_row + y) / sy);
                        let db = read(cx, cy, 0, 2) - f64::from(p.cb[i]);
                        let dr = read(cx, cy, 1, 2) - f64::from(p.cr[i]);
                        ec += f64::mul_add(dr, dr, db * db);
                    }
                }
            }
        });
        let n = (p.w * p.h) as f64;
        let db = |mse: f64| 10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10();
        (db(ey / n), db(ec / (2.0 * n)))
    }

    // ---- Encode and decode sessions ----------------------------------------------------------

    struct Sample(CFRetained<CMSampleBuffer>);
    // SAFETY: a CMSampleBuffer is immutable once VideoToolbox hands it over and CF reference
    // counting is thread-safe; the test only reads it on another thread.
    unsafe impl Send for Sample {}

    unsafe extern "C-unwind" fn on_encoded(
        refcon: *mut c_void,
        _source: *mut c_void,
        status: i32,
        flags: VTEncodeInfoFlags,
        sample: *mut CMSampleBuffer,
    ) {
        // SAFETY: the refcon is the `Sender` boxed by `Session::new`, alive until the session
        // is invalidated in `Drop`.
        let tx = unsafe { &*refcon.cast::<mpsc::Sender<(Instant, Option<Sample>)>>() };
        let sample = NonNull::new(sample)
            .filter(|_| status == 0 && !flags.contains(VTEncodeInfoFlags::FrameDropped))
            // SAFETY: the callback borrows a valid sample buffer; retaining it keeps it.
            .map(|s| Sample(unsafe { CFRetained::retain(s) }));
        let _gone = tx.send((Instant::now(), sample));
    }

    struct Session {
        vt: CFRetained<VTCompressionSession>,
        rx: mpsc::Receiver<(Instant, Option<Sample>)>,
        _tx: Box<mpsc::Sender<(Instant, Option<Sample>)>>,
        /// `EnableLTR`'s status: 0 when the encoder took long-term references.
        ltr: i32,
        /// Frames a second the presentation stamps count in, and the rate the encoder expects.
        fps: i32,
    }

    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: invalidation stops callbacks before the boxed sender is freed.
            unsafe { self.vt.invalidate() }
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Spec {
        LowLatencyHardware,
        Hardware,
        Encoder(&'static str),
    }

    impl Session {
        /// A session like the worker's (real time, no reordering, infinite GOP, LTR), with
        /// `profile` and `format` as its declared source; the status on failure.
        fn new(
            w: usize,
            h: usize,
            spec: Spec,
            profile: Option<&CFString>,
            format: u32,
            bitrate: i64,
        ) -> Result<Self, (&'static str, i32)> {
            Self::of(
                kCMVideoCodecType_HEVC,
                (w, h),
                spec,
                profile,
                format,
                bitrate,
            )
        }

        /// [`Session::new`] for `codec`, a `CMVideoCodecType`.
        fn of(
            codec: u32,
            (w, h): (usize, usize),
            spec: Spec,
            profile: Option<&CFString>,
            format: u32,
            bitrate: i64,
        ) -> Result<Self, (&'static str, i32)> {
            // SAFETY: framework-provided constant strings.
            let (ll, hw, id_key) = unsafe {
                (
                    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                    kVTVideoEncoderSpecification_EncoderID,
                )
            };
            let id;
            let spec = match spec {
                Spec::LowLatencyHardware => dict(&[(ll, yes()), (hw, yes())]),
                Spec::Hardware => dict(&[(hw, yes())]),
                Spec::Encoder(name) => {
                    id = CFString::from_str(name);
                    dict(&[(id_key, &id)])
                }
            };
            let format_number = CFNumber::new_i64(i64::from(format));
            // SAFETY: framework-provided constant string.
            let source = dict(&[(unsafe { kCVPixelBufferPixelFormatTypeKey }, &format_number)]);
            let (tx, rx) = mpsc::channel();
            let tx = Box::new(tx);
            let mut raw: *mut VTCompressionSession = ptr::null_mut();
            // SAFETY: valid pointers; the refcon is the boxed sender this `Session` owns.
            let status = unsafe {
                VTCompressionSession::create(
                    None,
                    w as i32,
                    h as i32,
                    codec,
                    Some(spec.as_opaque()),
                    Some(source.as_opaque()),
                    None,
                    Some(on_encoded),
                    ptr::from_ref(&*tx).cast_mut().cast::<c_void>(),
                    NonNull::from(&mut raw),
                )
            };
            let raw = NonNull::new(raw)
                .filter(|_| status == 0)
                .ok_or(("create", status))?;
            // SAFETY: +1 reference from the create call.
            let session = unsafe { CFRetained::from_raw(raw) };
            let mut this = Self {
                vt: session,
                rx,
                _tx: tx,
                ltr: 0,
                fps: 60,
            };
            let s: &CFType = &this.vt;
            // SAFETY: framework-provided constant strings.
            unsafe {
                // Best effort, as the worker sets them: not every encoder takes every key.
                set(s, kVTCompressionPropertyKey_RealTime, yes());
                set(
                    s,
                    kVTCompressionPropertyKey_AllowFrameReordering,
                    CFBoolean::new(false),
                );
                set(
                    s,
                    kVTCompressionPropertyKey_ExpectedFrameRate,
                    &CFNumber::new_i64(60),
                );
                set(
                    s,
                    kVTCompressionPropertyKey_MaxKeyFrameInterval,
                    &CFNumber::new_i64(i64::from(i32::MAX)),
                );
                this.ltr = set(s, kVTCompressionPropertyKey_EnableLTR, yes());
                let status = set(
                    s,
                    kVTCompressionPropertyKey_AverageBitRate,
                    &CFNumber::new_i64(bitrate),
                );
                if status != 0 {
                    return Err(("AverageBitRate", status));
                }
                if let Some(profile) = profile {
                    let status = set(s, kVTCompressionPropertyKey_ProfileLevel, profile);
                    if status != 0 {
                        return Err(("ProfileLevel", status));
                    }
                }
                let status = this.vt.prepare_to_encode_frames();
                if status != 0 {
                    return Err(("prepare", status));
                }
            }
            Ok(this)
        }

        /// Stamp frames at `fps` and tell the encoder so, as the worker's `set_frame_rate` does.
        fn set_fps(&mut self, fps: i32) -> i32 {
            self.fps = fps;
            let rate = CFNumber::new_i64(i64::from(fps));
            // SAFETY: framework-provided constant string.
            set(
                &self.vt,
                unsafe { kVTCompressionPropertyKey_ExpectedFrameRate },
                &rate,
            )
        }

        /// Submit one picture without waiting for it; the status.
        fn submit(&self, image: &CVPixelBuffer, index: i64) -> i32 {
            let pts = CMTime {
                value: index,
                timescale: self.fps,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            };
            // SAFETY: a valid image and session; no per-frame properties or refcon.
            unsafe {
                self.vt.encode_frame(
                    image,
                    pts,
                    kCMTimeInvalid,
                    None,
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            }
        }

        fn hardware(&self) -> Option<bool> {
            // SAFETY: framework-provided constant string.
            copy_bool(&self.vt, unsafe {
                kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder
            })
        }

        /// Encode one picture and wait for it: the submit → callback time and the sample.
        fn encode(&self, image: &CVPixelBuffer, index: i64) -> (Duration, Option<Sample>) {
            let submitted = Instant::now();
            let status = self.submit(image, index);
            if status != 0 {
                return (Duration::ZERO, None);
            }
            match self.rx.recv_timeout(Duration::from_secs(10)) {
                Ok((at, sample)) => (at.duration_since(submitted), sample),
                Err(_) => (Duration::ZERO, None),
            }
        }
    }

    fn sample_bytes(sample: &CMSampleBuffer) -> usize {
        // SAFETY: a valid sample buffer.
        unsafe { sample.total_sample_size() }
    }

    /// `(profile_idc, chroma_format_idc, bit depth)` from the stream's SPS.
    fn sps_of(format: &CMFormatDescription) -> Option<(u32, u32, u32)> {
        let mut count = 0_usize;
        let mut index = 0_usize;
        loop {
            let mut ptr: *const u8 = ptr::null();
            let mut size = 0_usize;
            // SAFETY: valid out pointers; the bytes are owned by `format`.
            let status = unsafe {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format,
                    index,
                    &raw mut ptr,
                    &raw mut size,
                    &raw mut count,
                    ptr::null_mut(),
                )
            };
            if status != 0 || ptr.is_null() {
                return None;
            }
            // SAFETY: CoreMedia returned `size` readable bytes at `ptr`.
            let nal = unsafe { std::slice::from_raw_parts(ptr, size) };
            if (nal[0] >> 1) & 0x3f == 33 {
                return parse_sps(nal);
            }
            index += 1;
            if index >= count {
                return None;
            }
        }
    }

    /// `(profile_idc, chroma_format_idc, bit depth)`: the profile from the SPS's fixed-length
    /// head (no emulation bytes can precede it), the rest from the codec's own parser.
    fn parse_sps(nal: &[u8]) -> Option<(u32, u32, u32)> {
        let profile_idc = u32::from(nal.get(3)? & 0x1f);
        let format = hevc::sample_format(nal)?;
        Some((profile_idc, format.chroma_format_idc, format.bit_depth))
    }

    unsafe extern "C-unwind" fn on_decoded(
        refcon: *mut c_void,
        _source: *mut c_void,
        status: i32,
        _flags: VTDecodeInfoFlags,
        image: *mut CVImageBuffer,
        _pts: CMTime,
        _duration: CMTime,
    ) {
        // SAFETY: the refcon is the `Sender` owned by `decode`, which outlives the session.
        let tx = unsafe { &*refcon.cast::<mpsc::Sender<Result<Pixels, i32>>>() };
        let image = NonNull::new(image).filter(|_| status == 0);
        // SAFETY: the callback borrows a valid image buffer; retaining it keeps it.
        let _gone = tx.send(
            image
                .map(|i| Pixels(unsafe { CFRetained::retain(i) }))
                .ok_or(status),
        );
    }

    struct Pixels(CFRetained<CVImageBuffer>);
    // SAFETY: the decoded buffer is not written again once handed over; CF retain is thread-safe.
    unsafe impl Send for Pixels {}

    /// Decode `samples` into `output` format; `(hardware in use, last picture)`, or the
    /// failing status.
    fn decode(
        samples: &[Sample],
        output: u32,
        require_hardware: bool,
    ) -> Result<(Option<bool>, Pixels), (&'static str, i32)> {
        let (hardware, mut tail) = decode_tail(samples, output, require_hardware, 1)?;
        tail.pop().map(|p| (hardware, p)).ok_or(("no picture", 0))
    }

    /// Whether the hardware decoder was in use, and the last pictures it gave back.
    type Tail = (Option<bool>, Vec<Pixels>);

    /// What failed, and the status it failed with.
    type Failure = (&'static str, i32);

    /// Decode `samples` into `output` format; `(hardware in use, the last `keep` pictures)`, or
    /// the failing status.
    fn decode_tail(
        samples: &[Sample],
        output: u32,
        require_hardware: bool,
        keep: usize,
    ) -> Result<Tail, (&'static str, i32)> {
        let mut tail: std::collections::VecDeque<Pixels> =
            std::collections::VecDeque::with_capacity(keep + 1);
        let samples: Vec<&Sample> = samples.iter().collect();
        let (hardware, failed) = decode_each(&samples, output, require_hardware, |picture| {
            let Ok(p) = picture else { return false };
            tail.push_back(p);
            if tail.len() > keep {
                tail.pop_front();
            }
            true
        })?;
        match failed {
            Some(failure) => Err(failure),
            None => Ok((hardware, tail.into())),
        }
    }

    /// Decode `samples` in order on one session and hand `each` every picture, or the status a
    /// sample failed with; `each` returns whether to go on. `(hardware in use, the failure that
    /// stopped it)`, or the status the session could not be made with.
    fn decode_each(
        samples: &[&Sample],
        output: u32,
        require_hardware: bool,
        mut each: impl FnMut(Result<Pixels, i32>) -> bool,
    ) -> Result<(Option<bool>, Option<Failure>), Failure> {
        let first = samples.first().ok_or(("no samples", 0))?;
        // SAFETY: a valid sample buffer.
        let format = unsafe { first.0.format_description() }.ok_or(("format", 0))?;
        let (tx, rx) = mpsc::channel::<Result<Pixels, i32>>();
        let record = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_decoded),
            decompressionOutputRefCon: ptr::from_ref(&tx).cast_mut().cast::<c_void>(),
        };
        let format_number = CFNumber::new_i64(i64::from(output));
        // SAFETY: framework-provided constant strings.
        let attrs = dict(&[(unsafe { kCVPixelBufferPixelFormatTypeKey }, &format_number)]);
        let spec = dict(&[(
            // SAFETY: framework-provided constant string.
            unsafe { kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder },
            CFBoolean::new(require_hardware),
        )]);
        let mut raw: *mut VTDecompressionSession = ptr::null_mut();
        // SAFETY: valid pointers; the refcon is `tx`, which outlives the session.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                Some(spec.as_opaque()),
                Some(attrs.as_opaque()),
                &raw const record,
                NonNull::from(&mut raw),
            )
        };
        let raw = NonNull::new(raw)
            .filter(|_| status == 0)
            .ok_or(("create", status))?;
        // SAFETY: +1 reference from the create call.
        let session = unsafe { CFRetained::from_raw(raw) };
        // SAFETY: framework-provided constant string.
        let hardware = copy_bool(&session, unsafe {
            kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder
        });
        let mut failed = None;
        for sample in samples {
            // SAFETY: a valid sample and session; synchronous decode, no refcon.
            let status = unsafe {
                session.decode_frame(
                    &sample.0,
                    VTDecodeFrameFlags(0),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            let (picture, why) = if status == 0 {
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(Ok(p)) => (Ok(p), ""),
                    Ok(Err(s)) => (Err(s), "output"),
                    Err(_) => {
                        failed = Some(("timeout", 0));
                        break;
                    }
                }
            } else {
                (Err(status), "decode")
            };
            let status = picture.as_ref().err().copied();
            if !each(picture) {
                failed = status.map(|s| (why, s));
                break;
            }
        }
        // SAFETY: invalidation stops callbacks before `tx` is dropped.
        unsafe { session.invalidate() }
        Ok((hardware, failed))
    }

    /// Nanoseconds as milliseconds.
    fn ms(ns: u64) -> f64 {
        ns as f64 / 1e6
    }

    // ---- The probe -----------------------------------------------------------------------------

    fn low_latency_spec() -> CFRetained<Dict> {
        // SAFETY: framework-provided constant strings.
        let (ll, hw) = unsafe {
            (
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
            )
        };
        dict(&[(ll, yes()), (hw, yes())])
    }

    fn hardware_spec() -> CFRetained<Dict> {
        // SAFETY: framework-provided constant string.
        let hw = unsafe { kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder };
        dict(&[(hw, yes())])
    }

    /// The profile named `suffix` from an encoder's own supported-value list: the 4:4:4
    /// profiles are advertised there but the SDK exports no constant for them.
    fn advertised(list: &[CFRetained<CFString>], name: &str) -> Option<CFRetained<CFString>> {
        list.iter().find(|p| p.to_string() == name).cloned()
    }

    /// The first picture's SPS `(profile_idc, chroma_format_idc, bits, bits)` from a session.
    fn emitted(session: &Session, image: &CVPixelBuffer) -> Option<(u32, u32, u32)> {
        let (_, sample) = session.encode(image, 0);
        // SAFETY: a valid sample buffer.
        sample
            .and_then(|s| unsafe { s.0.format_description() })
            .and_then(|f| sps_of(&f))
    }

    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn chroma_444_capabilities() {
        // 1. What the framework lists.
        // SAFETY: plain query.
        let hw_decode = unsafe { VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) };
        eprintln!("MEASURE vt hevc_hw_decode={hw_decode}");
        let encoders = encoder_list();
        for (id, hw) in &encoders {
            eprintln!("MEASURE vt encoder id={id} hardware={hw}");
        }
        // SAFETY: framework-provided constant strings.
        let (ll, id_key) = unsafe {
            (
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                kVTVideoEncoderSpecification_EncoderID,
            )
        };
        let mut specs: Vec<(String, CFRetained<Dict>)> = vec![
            ("low-latency+hw".to_owned(), low_latency_spec()),
            ("hw".to_owned(), hardware_spec()),
        ];
        for (id, _) in &encoders {
            let name = CFString::from_str(id);
            specs.push((format!("id={id}"), dict(&[(id_key, &name)])));
            specs.push((
                format!("id={id}+low-latency"),
                dict(&[(id_key, &name), (ll, yes())]),
            ));
        }
        let mut advertised_profiles: Vec<CFRetained<CFString>> = Vec::new();
        for (name, spec) in &specs {
            let (status, id, profiles, keys) = supported_for(1920, 1080, spec);
            let names: Vec<String> = profiles.iter().map(ToString::to_string).collect();
            eprintln!(
                "MEASURE vt supported spec={name} status={status} encoder={id} profiles={names:?}"
            );
            let wanted = [
                "EnableLTR",
                "InputPixelFormat",
                "Quality",
                "ConstantQualityFactor",
            ];
            let has: Vec<&str> = wanted
                .iter()
                .copied()
                .filter(|k| keys.iter().any(|key| key == k))
                .collect();
            eprintln!("MEASURE vt keys spec={name} of {wanted:?} lists {has:?}");
            for p in profiles {
                if !advertised_profiles
                    .iter()
                    .any(|q| q.to_string() == p.to_string())
                {
                    advertised_profiles.push(p);
                }
            }
        }

        // 2. What a session actually emits for each profile the encoders advertise, fed 4:2:0,
        //    4:2:2 and 4:4:4 pictures.
        let formats = [
            kCVPixelFormatType_32BGRA,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_422YpCbCr10BiPlanarFullRange,
            kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
        ];
        let mut session_specs = vec![Spec::LowLatencyHardware, Spec::Hardware];
        for (id, hw) in &encoders {
            if !hw {
                session_specs.push(Spec::Encoder(Box::leak(id.clone().into_boxed_str())));
            }
        }
        let (w, h) = (1920, 1080);
        let source = picture(w, h, 0);
        let mut beyond_420 = Vec::new();
        for spec in &session_specs {
            let profiles = std::iter::once(None).chain(advertised_profiles.iter().map(Some));
            for profile in profiles {
                let pname = profile.map_or_else(|| "none".to_owned(), ToString::to_string);
                for &format in &formats {
                    let tag = format!("spec={spec:?} profile={pname} input={}", fourcc(format));
                    let session = match Session::new(
                        w,
                        h,
                        *spec,
                        profile.map(|p| &**p),
                        format,
                        20_000_000,
                    ) {
                        Ok(s) => s,
                        Err((call, status)) => {
                            eprintln!("MEASURE vt session {tag} refused {call} status={status}");
                            continue;
                        }
                    };
                    let image = fill(&source, format);
                    let (_, sample) = session.encode(&image, 0);
                    let sps = sample.as_ref().and_then(|s| {
                        // SAFETY: a valid sample buffer.
                        unsafe { s.0.format_description() }.and_then(|f| sps_of(&f))
                    });
                    eprintln!(
                        "MEASURE vt session {tag} hardware={:?} ltr_status={} \
                         sps(profile_idc,chroma_format_idc,bits)={sps:?}",
                        session.hardware(),
                        session.ltr,
                    );
                    if let (Some((_, chroma, ..)), Some(sample)) = (sps, sample)
                        && chroma > 1
                        && !beyond_420
                            .iter()
                            .any(|(_, f, c, _): &(String, u32, u32, Sample)| {
                                *f == format && *c == chroma
                            })
                    {
                        beyond_420.push((tag, format, chroma, sample));
                    }
                }
            }
        }

        // 3. Whether the decoder takes what came out beyond 4:2:0, in hardware.
        for (tag, format, chroma, sample) in &beyond_420 {
            let output = if *format == kCVPixelFormatType_32BGRA {
                kCVPixelFormatType_444YpCbCr8BiPlanarFullRange
            } else {
                *format
            };
            match decode(std::slice::from_ref(sample), output, true) {
                Ok((hardware, pixels)) => eprintln!(
                    "MEASURE vt decode {tag} chroma_format_idc={chroma} require_hw=true \
                     hardware={hardware:?} output={}",
                    fourcc(CVPixelBufferGetPixelFormatType(&pixels.0))
                ),
                Err((call, status)) => eprintln!(
                    "MEASURE vt decode {tag} chroma_format_idc={chroma} require_hw=true \
                     failed {call} status={status}"
                ),
            }
        }
    }

    /// One stream configuration the cost measurement compares.
    struct Mode {
        name: &'static str,
        spec: Spec,
        profile: Option<CFRetained<CFString>>,
        format: u32,
    }

    impl Mode {
        /// What the decoder is asked for: the source's own format, or 4:4:4 for BGRA.
        fn decoded(&self) -> u32 {
            if self.format == kCVPixelFormatType_32BGRA {
                kCVPixelFormatType_444YpCbCr8BiPlanarFullRange
            } else {
                self.format
            }
        }
    }

    fn modes() -> Vec<Mode> {
        let (_, _, ll_profiles, _) = supported_for(1920, 1080, &low_latency_spec());
        let (_, _, hw_profiles, _) = supported_for(1920, 1080, &hardware_spec());
        // SAFETY: framework-provided constant string.
        let main: CFRetained<CFString> =
            CFRetained::from(unsafe { kVTProfileLevel_HEVC_Main_AutoLevel });
        let mut modes = vec![
            Mode {
                name: "low-latency Main 4:2:0 (shipped)",
                spec: Spec::LowLatencyHardware,
                profile: Some(main.clone()),
                format: kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            },
            Mode {
                name: "hw Main 4:2:0",
                spec: Spec::Hardware,
                profile: Some(main),
                format: kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            },
            Mode {
                name: "hw Main444 4:4:4",
                spec: Spec::Hardware,
                profile: advertised(&hw_profiles, "HEVC_Main444_AutoLevel"),
                format: kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
            },
        ];
        if let Some(profile) = advertised(&ll_profiles, "HEVC_Main444_AutoLevel") {
            modes.push(Mode {
                name: "low-latency Main444 4:4:4",
                spec: Spec::LowLatencyHardware,
                profile: Some(profile.clone()),
                format: kCVPixelFormatType_444YpCbCr8BiPlanarFullRange,
            });
            modes.push(Mode {
                name: "low-latency Main444 fed BGRA",
                spec: Spec::LowLatencyHardware,
                profile: Some(profile),
                format: kCVPixelFormatType_32BGRA,
            });
        }
        if let Some(profile) = advertised(&ll_profiles, "HEVC_Main44410_AutoLevel") {
            modes.push(Mode {
                name: "low-latency Main44410 fed xf44",
                spec: Spec::LowLatencyHardware,
                profile: Some(profile.clone()),
                format: kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
            });
            modes.push(Mode {
                name: "low-latency Main44410 fed BGRA",
                spec: Spec::LowLatencyHardware,
                profile: Some(profile),
                format: kCVPixelFormatType_32BGRA,
            });
        }
        modes
    }

    /// Encode time per frame at 1080p and 5K, one frame in flight like the worker, over a
    /// scrolling text picture.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn chroma_444_encode_time() {
        const FRAMES: usize = 180;
        for (w, h, bitrate) in [
            (1920_usize, 1080_usize, 16_000_000_i64),
            (5120, 2880, 60_000_000),
        ] {
            let pictures: Vec<Picture> = (0..6).map(|i| picture(w, h, i * 16)).collect();
            for mode in modes() {
                let Ok(session) = Session::new(
                    w,
                    h,
                    mode.spec,
                    mode.profile.as_deref(),
                    mode.format,
                    bitrate,
                ) else {
                    eprintln!("MEASURE encode {w}x{h} mode={:?} refused", mode.name);
                    continue;
                };
                let images: Vec<_> = pictures.iter().map(|p| fill(p, mode.format)).collect();
                let chroma = emitted(&session, &images[0]).map(|s| s.1);
                let mut times = Vec::with_capacity(FRAMES);
                let mut bytes = 0_usize;
                for i in 0..FRAMES {
                    let (took, sample) = session.encode(&images[i % images.len()], i as i64 + 1);
                    if let Some(sample) = sample {
                        times.push(took);
                        if i >= 30 {
                            bytes += sample_bytes(&sample.0);
                        }
                    }
                }
                let spread = Spread::of_durations(&times).unwrap_or_default();
                let mbps = (bytes * 8) as f64 / ((FRAMES - 30) as f64 / 60.0) / 1e6;
                eprintln!(
                    "MEASURE encode {w}x{h} mode={:?} chroma_format_idc={chroma:?} frames={} \
                     p50={:.2}ms p95={:.2}ms max={:.2}ms target={}Mbit/s actual={mbps:.1}Mbit/s",
                    mode.name,
                    times.len(),
                    ms(spread.p50),
                    ms(spread.p95),
                    ms(spread.max),
                    bitrate / 1_000_000,
                );
            }
        }
    }

    /// Bits against quality on the text picture at 1080p: each mode at several target rates,
    /// the actual rate it spent, and the PSNR of what the hardware decoder gives back against
    /// the 4:4:4 source.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn chroma_444_rate_quality() {
        const FRAMES: usize = 90;
        let (w, h) = (1920, 1080);
        let pictures: Vec<Picture> = (0..6).map(|i| picture(w, h, i * 16)).collect();
        for mode in modes() {
            let images: Vec<_> = pictures.iter().map(|p| fill(p, mode.format)).collect();
            // The picture as the source format carries it, before any encoding.
            let (sy, sc) = if mode.format == kCVPixelFormatType_32BGRA {
                (f64::NAN, f64::NAN)
            } else {
                psnr(&pictures[0], &images[0])
            };
            eprintln!(
                "MEASURE quality mode={:?} source_only psnr_y={sy:.2} psnr_c={sc:.2}",
                mode.name
            );
            for mbps in [4_i64, 8, 16, 32] {
                let Ok(session) = Session::new(
                    w,
                    h,
                    mode.spec,
                    mode.profile.as_deref(),
                    mode.format,
                    mbps * 1_000_000,
                ) else {
                    eprintln!("MEASURE quality mode={:?} refused", mode.name);
                    break;
                };
                let mut samples = Vec::with_capacity(FRAMES);
                let mut bytes = 0_usize;
                for i in 0..FRAMES {
                    let (_, sample) = session.encode(&images[i % images.len()], i as i64);
                    if let Some(sample) = sample {
                        if i >= 30 {
                            bytes += sample_bytes(&sample.0);
                        }
                        samples.push(sample);
                    }
                }
                let actual = (bytes * 8) as f64 / ((FRAMES - 30) as f64 / 60.0) / 1e6;
                let last = &pictures[(FRAMES - 1) % pictures.len()];
                match decode(&samples, mode.decoded(), true) {
                    Ok((hardware, pixels)) => {
                        let (py, pc) = psnr(last, &pixels.0);
                        eprintln!(
                            "MEASURE quality mode={:?} target={mbps}Mbit/s actual={actual:.2}Mbit/s \
                             psnr_y={py:.2}dB psnr_c={pc:.2}dB hw_decode={hardware:?}",
                            mode.name
                        );
                    }
                    Err((call, status)) => eprintln!(
                        "MEASURE quality mode={:?} target={mbps}Mbit/s decode failed {call} {status}",
                        mode.name
                    ),
                }
            }
        }
    }
    /// The one-minute load average, as `uptime` prints it.
    fn load_average() -> String {
        std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|text| text.split_whitespace().nth(1).map(str::to_owned))
            .unwrap_or_default()
    }

    /// The shipped encoder fed in real time at 60 and at 120 frames a second: the text picture
    /// scrolling at 960 pixel rows a second in both (16 a frame at 60, 8 at 120), at several
    /// target rates. For each: submit → callback per frame with the encoder pipelined as the
    /// worker runs it (the next picture goes in on its beat whether or not the last came back),
    /// frames it dropped, the rate it spent, and the mean luma PSNR of the last pictures the
    /// hardware decoder gives back against their sources.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn frame_rate_120_against_60() {
        const SECONDS: u32 = 3;
        const SCROLL_PER_S: usize = 960;
        const SCORED: usize = 10;
        let sizes: &[(usize, usize, &[i64])] = &[
            (1920, 1080, &[4, 8, 12, 16, 32]),
            (2560, 1440, &[16, 32]),
            (3024, 1964, &[8, 16, 32, 60]),
            (3840, 2160, &[32]),
        ];
        for &(w, h, rates) in sizes {
            for fps in [60_i32, 120] {
                let step = SCROLL_PER_S / fps as usize;
                // One text line (16 rows) of distinct pictures, cycled.
                let pictures: Vec<Picture> =
                    (0..(96 / step)).map(|i| picture(w, h, i * step)).collect();
                let images: Vec<_> = pictures
                    .iter()
                    .map(|p| fill(p, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange))
                    .collect();
                for &mbps in rates {
                    let Ok(mut session) = Session::new(
                        w,
                        h,
                        Spec::LowLatencyHardware,
                        None,
                        kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                        mbps * 1_000_000,
                    ) else {
                        eprintln!("MEASURE fps {w}x{h} fps={fps} {mbps}Mbit/s refused");
                        continue;
                    };
                    let rate_status = session.set_fps(fps);
                    let frames = (fps as u32 * SECONDS) as usize;
                    let period = Duration::from_secs(1) / fps as u32;
                    let mut submitted = Vec::with_capacity(frames);
                    let mut back: Vec<(Instant, Option<Sample>)> = Vec::with_capacity(frames);
                    let load = load_average();
                    let start = Instant::now();
                    for i in 0..frames {
                        let due = start + period * i as u32;
                        if let Some(wait) = due.checked_duration_since(Instant::now()) {
                            std::thread::sleep(wait);
                        }
                        submitted.push(Instant::now());
                        let status = session.submit(&images[i % images.len()], i as i64);
                        assert_eq!(status, 0, "submit");
                        while let Ok(got) = session.rx.try_recv() {
                            back.push(got);
                        }
                    }
                    while back.len() < frames {
                        match session.rx.recv_timeout(Duration::from_secs(5)) {
                            Ok(got) => back.push(got),
                            Err(_) => break,
                        }
                    }
                    let times: Vec<Duration> = back
                        .iter()
                        .zip(&submitted)
                        .map(|((at, _), sent)| at.saturating_duration_since(*sent))
                        .collect();
                    let dropped = back.iter().filter(|(_, s)| s.is_none()).count();
                    // The first second settles the rate controller.
                    let settled = fps as usize;
                    let bytes: usize = back
                        .iter()
                        .skip(settled)
                        .filter_map(|(_, s)| s.as_ref().map(|s| sample_bytes(&s.0)))
                        .sum();
                    let spent = (bytes * 8) as f64 / f64::from(SECONDS - 1) / 1e6;
                    let per_frame = bytes as f64 / (frames - settled) as f64 / 1024.0;
                    // Which picture each sample the encoder kept was made from.
                    let (sources, samples): (Vec<usize>, Vec<Sample>) = back
                        .into_iter()
                        .enumerate()
                        .filter_map(|(i, (_, s))| s.map(|s| (i % pictures.len(), s)))
                        .unzip();
                    let psnr_y = match decode_tail(
                        &samples,
                        kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                        true,
                        SCORED,
                    ) {
                        Ok((_, tail)) if !tail.is_empty() => {
                            let first = sources.len() - tail.len();
                            let sum: f64 = tail
                                .iter()
                                .zip(&sources[first..])
                                .map(|(p, &source)| psnr(&pictures[source], &p.0).0)
                                .sum();
                            sum / tail.len() as f64
                        }
                        _ => f64::NAN,
                    };
                    let spread = Spread::of_durations(&times).unwrap_or_default();
                    eprintln!(
                        "MEASURE fps {w}x{h} fps={fps} target={mbps}Mbit/s load={load} \
                         expected_rate_status={rate_status} encode p50={:.2}ms p95={:.2}ms \
                         max={:.2}ms dropped={dropped}/{frames} spent={spent:.2}Mbit/s \
                         per_frame={per_frame:.1}KiB psnr_y={psnr_y:.2}dB",
                        ms(spread.p50),
                        ms(spread.p95),
                        ms(spread.max),
                    );
                }
            }
        }
    }

    // ---- The worker's session: frame size, presets, temporal layers --------------------------

    impl Session {
        /// A session set up as `onicode_desktop_codec::Encoder` sets up the worker's: low-latency
        /// hardware HEVC Main 4:2:0 fed `420f`, with every property `Encoder::configure` sets
        /// (the optional ones as best effort, as there) and `fps` expected.
        fn worker(w: usize, h: usize, bitrate: i64, fps: i32) -> Result<Self, (&'static str, i32)> {
            Self::worker_on(Spec::LowLatencyHardware, w, h, bitrate, fps)
        }

        /// [`Session::worker`] on the encoder `spec` picks.
        fn worker_on(
            spec: Spec,
            w: usize,
            h: usize,
            bitrate: i64,
            fps: i32,
        ) -> Result<Self, (&'static str, i32)> {
            // SAFETY: framework-provided constant string.
            let main = unsafe { kVTProfileLevel_HEVC_Main_AutoLevel };
            let source = (main, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
            Self::worker_as(spec, kCMVideoCodecType_HEVC, source, (w, h), bitrate, fps)
        }

        /// [`Session::worker`] on the encoder `spec` picks, for `codec` in `profile` fed
        /// `format`.
        fn worker_as(
            spec: Spec,
            codec: u32,
            (profile, format): (&CFString, u32),
            (w, h): (usize, usize),
            bitrate: i64,
            fps: i32,
        ) -> Result<Self, (&'static str, i32)> {
            let mut session = Self::of(codec, (w, h), spec, Some(profile), format, bitrate)?;
            session.set_fps(fps);
            let limits = CFArray::from_retained_objects(&[
                CFNumber::new_i64(bitrate * 5 / 32),
                CFNumber::new_f64(1.0),
            ]);
            let s: &CFType = &session.vt;
            // SAFETY: framework-provided constant strings.
            unsafe {
                set(
                    s,
                    kVTCompressionPropertyKey_AllowOpenGOP,
                    CFBoolean::new(false),
                );
                set(
                    s,
                    kVTCompressionPropertyKey_MaxFrameDelayCount,
                    &CFNumber::new_i64(0),
                );
                set(
                    s,
                    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality,
                    yes(),
                );
                set(
                    s,
                    kVTCompressionPropertyKey_MaximumRealTimeFrameRate,
                    &CFNumber::new_i64(120),
                );
                set(
                    s,
                    kVTCompressionPropertyKey_YCbCrMatrix,
                    kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                );
                set(s, kVTCompressionPropertyKey_DataRateLimits, &limits);
            }
            Ok(session)
        }
    }

    /// A property of a session, +1, or the status it failed with.
    fn copy_property(session: &CFType, key: &CFString) -> Result<CFRetained<CFType>, i32> {
        let mut out: *const CFType = ptr::null();
        // SAFETY: VTSession.h: the value comes back +1 through a `CFTypeRef *` out pointer.
        let status = unsafe {
            VTSessionCopyProperty(
                as_session(session),
                key,
                None,
                (&raw mut out).cast::<c_void>(),
            )
        };
        let raw = NonNull::new(out.cast_mut())
            .filter(|_| status == 0)
            .ok_or(status)?;
        // SAFETY: +1 reference from the copy call (Copy rule).
        Ok(unsafe { CFRetained::from_raw(raw) })
    }

    /// A value as a CFString-keyed dictionary, when it is one.
    fn as_dict(value: &CFType) -> Option<&Dict> {
        let d = value.downcast_ref::<CFDictionary>()?;
        // SAFETY: every dictionary read here is keyed by CFString (VTCompressionProperties.h,
        // CMSampleBuffer.h); values are only read.
        Some(unsafe { d.cast_unchecked() })
    }

    /// `key = value` pairs of a dictionary, sorted by key.
    fn pairs(d: &Dict) -> Vec<(String, String)> {
        let (keys, values) = d.to_vecs();
        let mut out: Vec<(String, String)> = keys
            .iter()
            .zip(&values)
            .map(|(k, v)| (k.to_string(), describe(v)))
            .collect();
        out.sort();
        out
    }

    /// A short rendering of a property value: numbers and booleans as they are, strings bare.
    fn describe(value: &CFType) -> String {
        if let Some(b) = value.downcast_ref::<CFBoolean>() {
            return b.as_bool().to_string();
        }
        if let Some(n) = value.downcast_ref::<CFNumber>() {
            return n
                .as_f64()
                .map_or_else(|| format!("{n:?}"), |f| f.to_string());
        }
        if let Some(d) = as_dict(value) {
            let inner: Vec<String> = pairs(d)
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            return format!("{{{}}}", inner.join(", "));
        }
        text(value)
    }

    /// Twelve pictures of the text scrolled 8 rows apart: a beat at 120 takes them in turn, one
    /// at 60 every other, so both scroll 960 rows a second.
    fn scrolling(w: usize, h: usize) -> (Vec<Picture>, Vec<CFRetained<CVPixelBuffer>>) {
        let pictures: Vec<Picture> = (0..12).map(|i| picture(w, h, i * 8)).collect();
        let images = pictures
            .iter()
            .map(|p| fill(p, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange))
            .collect();
        (pictures, images)
    }

    /// What frames submitted on a real-time beat gave back.
    struct Beat {
        /// Submit → callback per frame after the settling ones.
        times: Vec<Duration>,
        /// Frames the encoder dropped, of all submitted.
        dropped: usize,
        /// Bytes of the frames after the settling ones.
        bytes: usize,
        /// The samples it kept, each with the index of the picture it was made from.
        samples: Vec<(usize, Sample)>,
        /// Frames submitted a second: below the beat's rate when a submit blocks for longer
        /// than a period, as VideoToolbox's does when it encodes inside the call.
        submitted_fps: f64,
    }

    /// Submit `frames` pictures at the session's rate, each on its beat whether or not the last
    /// came back, as the capture callback does. The first `settle` frames are left out of the
    /// times and the bytes (the keyframe and the rate controller's start).
    fn on_beat(
        session: &Session,
        images: &[CFRetained<CVPixelBuffer>],
        frames: usize,
        settle: usize,
    ) -> Beat {
        on_beat_from(session, images, 0, frames, settle)
    }

    /// [`on_beat`] with presentation stamps counted from `first`, so a session can be fed on
    /// the beat more than once and its stamps still only go forward.
    fn on_beat_from(
        session: &Session,
        images: &[CFRetained<CVPixelBuffer>],
        first: usize,
        frames: usize,
        settle: usize,
    ) -> Beat {
        let stride = (120 / session.fps.max(1)) as usize;
        let period = Duration::from_secs(1) / session.fps as u32;
        let mut submitted = Vec::with_capacity(frames);
        let mut back: Vec<(Instant, Option<Sample>)> = Vec::with_capacity(frames);
        let start = Instant::now();
        for i in 0..frames {
            let due = start + period * i as u32;
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            submitted.push(Instant::now());
            let status = session.submit(&images[(i * stride) % images.len()], (first + i) as i64);
            assert_eq!(status, 0, "submit");
            while let Ok(got) = session.rx.try_recv() {
                back.push(got);
            }
        }
        let submitted_fps = frames as f64 / start.elapsed().as_secs_f64();
        while back.len() < frames {
            match session.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(got) => back.push(got),
                Err(_) => break,
            }
        }
        let times = back
            .iter()
            .zip(&submitted)
            .skip(settle)
            .map(|((at, _), sent)| at.saturating_duration_since(*sent))
            .collect();
        let dropped = back.iter().filter(|(_, s)| s.is_none()).count();
        let bytes = back
            .iter()
            .skip(settle)
            .filter_map(|(_, s)| s.as_ref().map(|s| sample_bytes(&s.0)))
            .sum();
        let samples = back
            .into_iter()
            .enumerate()
            .filter_map(|(i, (_, s))| s.map(|s| ((i * stride) % images.len(), s)))
            .collect();
        Beat {
            times,
            dropped,
            bytes,
            samples,
            submitted_fps,
        }
    }

    /// Submit → callback with one frame in flight, over `frames` pictures after `settle`.
    fn one_at_a_time(
        session: &Session,
        images: &[CFRetained<CVPixelBuffer>],
        frames: usize,
        settle: usize,
    ) -> Spread {
        let times: Vec<Duration> = (0..frames + settle)
            .filter_map(|i| {
                let (took, sample) = session.encode(&images[i % images.len()], 10_000 + i as i64);
                sample.map(|_| took)
            })
            .skip(settle)
            .collect();
        Spread::of_durations(&times).unwrap_or_default()
    }

    /// The multiple the size sweep pads each side to as the worker does (`ONICODE_PROBE_ALIGN=16`):
    /// the session codes the padded size, the picture at its top-left and black around it.
    /// 1 (the default) codes each size as it is.
    fn probe_align() -> usize {
        std::env::var("ONICODE_PROBE_ALIGN")
            .ok()
            .and_then(|a| a.parse().ok())
            .unwrap_or(1)
            .max(1)
    }

    /// The sizes the size sweep times, `ONICODE_PROBE_SIZES=3024x1964,3024x1968` to pick some.
    fn probe_sizes(default: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let Ok(list) = std::env::var("ONICODE_PROBE_SIZES") else {
            return default.to_vec();
        };
        list.split(',')
            .filter_map(|s| s.split_once('x'))
            .filter_map(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
            .collect()
    }

    /// Encode time against frame size, on the worker's session: one frame in flight, then on a
    /// real-time beat at 60 and at 120. 3024 × 1964 (a `MacBook` Pro's native size) came back
    /// 77–83 ms late where 3840 × 2160 took 22 (MEASUREMENTS "120 fps against 60"); the sweep
    /// moves one side at a time around it to find what the encoder minds.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn encode_time_by_size() {
        const SECONDS: usize = 3;
        let sizes = probe_sizes(&[
            (1920, 1080),
            (2560, 1440),
            (3840, 2160),
            (3024, 1964),
            (3024, 1962),
            (3024, 1960),
            (3024, 1968),
            (3024, 1952),
            (3008, 1964),
            (3020, 1968),
            (3456, 2234),
            (3456, 2240),
            (2880, 1800),
            (1728, 1118),
            (1500, 946),
            (1282, 802),
            (2000, 1234),
        ]);
        let align = probe_align();
        for (w, h) in sizes {
            let (cw, ch) = (w.next_multiple_of(align), h.next_multiple_of(align));
            let pictures: Vec<Picture> = (0..12).map(|i| picture(w, h, i * 8)).collect();
            let images: Vec<_> = pictures
                .iter()
                .map(|p| {
                    fill(
                        &padded(p, cw, ch),
                        kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                    )
                })
                .collect();
            for fps in [60_i32, 120] {
                let Ok(session) = Session::worker(cw, ch, 32_000_000, fps) else {
                    eprintln!("MEASURE size {w}x{h} fps={fps} refused");
                    continue;
                };
                let serial = one_at_a_time(&session, &images, 30, 5);
                let frames = fps as usize * SECONDS;
                let beat = on_beat(&session, &images, frames, fps as usize);
                let spread = Spread::of_durations(&beat.times).unwrap_or_default();
                let spent = (beat.bytes * 8) as f64 / (SECONDS - 1) as f64 / 1e6;
                eprintln!(
                    "MEASURE size {w}x{h} coded={cw}x{ch} w%16={} h%16={} fps={fps} load={} \
                     hardware={:?} one_in_flight p50={:.2}ms p95={:.2}ms | on_beat p50={:.2}ms \
                     p95={:.2}ms max={:.2}ms dropped={}/{frames} submitted={:.1}fps \
                     spent={spent:.2}Mbit/s",
                    w % 16,
                    h % 16,
                    load_average(),
                    session.hardware(),
                    ms(serial.p50),
                    ms(serial.p95),
                    ms(spread.p50),
                    ms(spread.p95),
                    ms(spread.max),
                    beat.dropped,
                    beat.submitted_fps,
                );
            }
        }
    }

    /// Mean luma PSNR of the last decoded pictures of `samples` against their sources.
    fn tail_psnr(samples: Vec<(usize, Sample)>, pictures: &[Picture]) -> f64 {
        tail_psnr_as(
            samples,
            pictures,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        )
    }

    /// [`tail_psnr`] decoded into `output`.
    fn tail_psnr_as(samples: Vec<(usize, Sample)>, pictures: &[Picture], output: u32) -> f64 {
        let (sources, samples): (Vec<usize>, Vec<Sample>) = samples.into_iter().unzip();
        match decode_tail(&samples, output, true, 10) {
            Ok((_, tail)) if !tail.is_empty() => {
                let first = sources.len() - tail.len();
                let sum: f64 = tail
                    .iter()
                    .zip(&sources[first..])
                    .map(|(p, &source)| psnr(&pictures[source], &p.0).0)
                    .sum();
                sum / tail.len() as f64
            }
            _ => f64::NAN,
        }
    }

    /// macOS 26's compression presets: what each session offers (`SupportedPresetDictionaries`),
    /// what `VideoConferencing` sets, and what it does to the worker's encode time, rate and
    /// picture at 1080p, 4K and 5K. Also lists every key the low-latency encoder supports, to
    /// see whether a latency key has appeared in the list or the headers.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn compression_presets() {
        for (name, spec) in [
            ("low-latency+hw", low_latency_spec()),
            ("hw", hardware_spec()),
        ] {
            let (status, id, _, keys) = supported_for(1920, 1080, &spec);
            eprintln!(
                "MEASURE preset supported spec={name} status={status} encoder={id} keys={keys:?}"
            );
        }
        // SAFETY: framework-provided constant strings.
        let (presets_key, conferencing) = unsafe {
            (
                kVTCompressionPropertyKey_SupportedPresetDictionaries,
                kVTCompressionPreset_VideoConferencing,
            )
        };
        let mut conferencing_settings: Option<CFRetained<CFType>> = None;
        for spec in [Spec::LowLatencyHardware, Spec::Hardware] {
            let session = Session::new(
                1920,
                1080,
                spec,
                None,
                kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                16_000_000,
            )
            .expect("session");
            match copy_property(&session.vt, presets_key) {
                Err(status) => eprintln!("MEASURE preset spec={spec:?} none status={status}"),
                Ok(presets) => {
                    let presets = as_dict(&presets).expect("a dictionary of presets");
                    for (name, settings) in pairs(presets) {
                        eprintln!("MEASURE preset spec={spec:?} {name} = {settings}");
                    }
                    if matches!(spec, Spec::LowLatencyHardware) {
                        conferencing_settings = presets.get(conferencing);
                    }
                }
            }
        }
        let Some(settings) = conferencing_settings else {
            onicode_desktop_testkit::live::skip(
                "the low-latency encoder offers no VideoConferencing preset",
            );
            return;
        };
        let settings = as_dict(&settings).expect("preset settings are a dictionary");
        let (keys, values) = settings.to_vecs();
        for (w, h, bitrate) in [
            (1920_usize, 1080_usize, 16_000_000_i64),
            (3840, 2160, 40_000_000),
            (5120, 2880, 60_000_000),
        ] {
            let (pictures, images) = scrolling(w, h);
            for with_preset in [false, true] {
                let session = Session::worker(w, h, bitrate, 60).expect("session");
                if with_preset {
                    let statuses: Vec<String> = keys
                        .iter()
                        .zip(&values)
                        .map(|(k, v)| format!("{k}={}", set(&session.vt, k, v)))
                        .collect();
                    eprintln!("MEASURE preset applied {w}x{h} statuses {statuses:?}");
                }
                // The beat first: its samples start at the keyframe, which the decoder needs.
                let beat = on_beat(&session, &images, 180, 60);
                let serial = one_at_a_time(&session, &images, 120, 10);
                let spread = Spread::of_durations(&beat.times).unwrap_or_default();
                let spent = (beat.bytes * 8) as f64 / 2.0 / 1e6;
                let dropped = beat.dropped;
                let psnr_y = tail_psnr(beat.samples, &pictures);
                eprintln!(
                    "MEASURE preset {w}x{h} config={} load={} one_in_flight p50={:.2}ms \
                     p95={:.2}ms max={:.2}ms | on_beat_60 p50={:.2}ms p95={:.2}ms max={:.2}ms \
                     dropped={dropped}/180 spent={spent:.2}Mbit/s psnr_y={psnr_y:.2}dB",
                    if with_preset {
                        "worker+VideoConferencing"
                    } else {
                        "worker"
                    },
                    load_average(),
                    ms(serial.p50),
                    ms(serial.p95),
                    ms(serial.max),
                    ms(spread.p50),
                    ms(spread.p95),
                    ms(spread.max),
                );
            }
        }
    }

    /// `(nal_unit_type, TemporalId)` of every NAL unit in a sample (four-byte lengths).
    fn nal_headers(sample: &CMSampleBuffer) -> Vec<(u8, u8)> {
        // SAFETY: a valid sample buffer.
        let Some(block) = (unsafe { sample.data_buffer() }) else {
            return Vec::new();
        };
        // SAFETY: a valid block buffer.
        let len = unsafe { block.data_length() };
        let mut bytes = vec![0_u8; len];
        // SAFETY: CoreMedia rule: copies exactly `len` bytes into a buffer that holds them.
        let status = unsafe {
            block.copy_data_bytes(0, len, NonNull::new(bytes.as_mut_ptr()).unwrap().cast())
        };
        assert_eq!(status, 0, "CMBlockBufferCopyDataBytes");
        let mut out = Vec::new();
        let mut at = 0;
        while at + 6 <= bytes.len() {
            let n = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let (b0, b1) = (bytes[at + 4], bytes[at + 5]);
            out.push(((b0 >> 1) & 0x3f, (b1 & 7).saturating_sub(1)));
            at += 4 + n;
        }
        out
    }

    /// The sample's first attachment dictionary, rendered: `IsDependedOnByOthers`,
    /// `DependsOnOthers`, `NotSync`, the HEVC temporal level, and every key it holds.
    fn sample_marks(
        sample: &CMSampleBuffer,
    ) -> (Option<bool>, Option<bool>, bool, Option<i64>, Vec<String>) {
        // SAFETY: valid sample buffer; `false` never allocates.
        let Some(array) = (unsafe { sample.sample_attachments_array(false) }) else {
            return (None, None, false, None, Vec::new());
        };
        // SAFETY: CMSampleBuffer.h: an array of CFDictionaries keyed by CFString.
        let array: CFRetained<CFArray<Dict>> = unsafe { CFRetained::cast_unchecked(array) };
        let Some(d) = array.get(0) else {
            return (None, None, false, None, Vec::new());
        };
        let flag = |key: &CFString| {
            d.get(key)
                .and_then(|v| v.downcast::<CFBoolean>().ok())
                .map(|b| b.as_bool())
        };
        // SAFETY: framework-provided constant strings.
        let (depended, depends, not_sync, level_info, level_key) = unsafe {
            (
                kCMSampleAttachmentKey_IsDependedOnByOthers,
                kCMSampleAttachmentKey_DependsOnOthers,
                kCMSampleAttachmentKey_NotSync,
                kCMSampleAttachmentKey_HEVCTemporalLevelInfo,
                kCMHEVCTemporalLevelInfoKey_TemporalLevel,
            )
        };
        let level = d.get(level_info).and_then(|v| {
            as_dict(&v)
                .and_then(|info| info.get(level_key))
                .and_then(|n| n.downcast::<CFNumber>().ok())
                .and_then(|n| n.as_i64())
        });
        let all = pairs(&d)
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        (
            flag(depended),
            flag(depends),
            flag(not_sync).unwrap_or(false),
            level,
            all,
        )
    }

    /// Whether the worker's encoder writes frames nothing refers to: each sample's NAL types and
    /// `TemporalId`s and the attachments that say so, on the worker's session as it is and with
    /// `BaseLayerFrameRateFraction` 0.5 asked for. A frame nothing refers to is one whose loss
    /// costs no refresh.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn temporal_layers() {
        const FRAMES: usize = 120;
        let (w, h) = (1920, 1080);
        let (_, images) = scrolling(w, h);
        // SAFETY: framework-provided constant string.
        let fraction_key = unsafe { kVTCompressionPropertyKey_BaseLayerFrameRateFraction };
        for fraction in [None, Some(0.5_f64)] {
            let session = Session::worker(w, h, 16_000_000, 60).expect("session");
            let status = fraction.map(|f| set(&session.vt, fraction_key, &CFNumber::new_f64(f)));
            let read_back = copy_property(&session.vt, fraction_key).map(|v| describe(&v));
            let beat = on_beat(&session, &images, FRAMES, 0);
            let spread = Spread::of_durations(&beat.times[FRAMES / 2..]).unwrap_or_default();
            let mut classes: std::collections::BTreeMap<String, (usize, usize)> =
                std::collections::BTreeMap::new();
            for (i, (_, sample)) in beat.samples.iter().enumerate() {
                let nals = nal_headers(&sample.0);
                let (depended, depends, not_sync, level, all) = sample_marks(&sample.0);
                let bytes = sample_bytes(&sample.0);
                let class = format!(
                    "nal={:?} depended_on={depended:?} depends={depends:?} not_sync={not_sync} \
                     temporal_level={level:?}",
                    nals.iter().filter(|(t, _)| *t < 32).collect::<Vec<_>>()
                );
                if i < 8 {
                    eprintln!(
                        "MEASURE temporal fraction={fraction:?} frame={i} bytes={bytes} {class} \
                         attachments={all:?}"
                    );
                }
                let entry = classes.entry(class).or_default();
                entry.0 += 1;
                entry.1 += bytes;
            }
            eprintln!(
                "MEASURE temporal fraction={fraction:?} set_status={status:?} read_back={read_back:?} \
                 on_beat p50={:.2}ms p95={:.2}ms dropped={}/{FRAMES}",
                ms(spread.p50),
                ms(spread.p95),
                beat.dropped
            );
            for (class, (count, bytes)) in classes {
                eprintln!(
                    "MEASURE temporal fraction={fraction:?} {count} frames, {} B mean: {class}",
                    bytes / count.max(1)
                );
            }
        }
    }

    // ---- Stream sides padded to 16: the conformance window (probe P1) ------------------------

    /// `p` drawn at the top-left of a `w × h` picture, the rest black: what ScreenCaptureKit
    /// renders into a padded surface under a `destinationRect` and a black background.
    fn padded(p: &Picture, w: usize, h: usize) -> Picture {
        let black = ycbcr([0, 0, 0]);
        let mut out = Picture {
            w,
            h,
            y: vec![black[0]; w * h],
            cb: vec![black[1]; w * h],
            cr: vec![black[2]; w * h],
            rgb: vec![[0, 0, 0]; w * h],
        };
        for row in 0..p.h {
            let (from, to) = (row * p.w, row * w);
            out.y[to..to + p.w].copy_from_slice(&p.y[from..from + p.w]);
            out.cb[to..to + p.w].copy_from_slice(&p.cb[from..from + p.w]);
            out.cr[to..to + p.w].copy_from_slice(&p.cr[from..from + p.w]);
            out.rgb[to..to + p.w].copy_from_slice(&p.rgb[from..from + p.w]);
        }
        out
    }

    /// Mean absolute luma difference, 0–255, between the decoded picture's last row and last
    /// column and the source's: a band of padding that showed would be the black against the
    /// picture's own edge.
    fn edge_error(p: &Picture, image: &CVPixelBuffer) -> f64 {
        let format = CVPixelBufferGetPixelFormatType(image);
        let (_, _, bits) = layout(format);
        let mut sum = 0.0_f64;
        let mut n = 0_usize;
        with_planes(image, true, |plane, base, stride| {
            if plane != 0 {
                return;
            }
            let read = |x: usize, y: usize| -> f64 {
                let width = if bits == 8 { 1 } else { 2 };
                // SAFETY: the plane is locked and `(x, y)` is inside the picture.
                let cell = unsafe { base.add(y * stride + x * width) };
                let mut b = [0_u8; 2];
                // SAFETY: `width` readable bytes of the locked plane.
                unsafe { ptr::copy_nonoverlapping(cell, b.as_mut_ptr(), width) }
                if bits == 8 {
                    f64::from(b[0])
                } else {
                    f64::from(u16::from_le_bytes(b) >> 6) / 4.0
                }
            };
            let edge = (0..p.w)
                .map(|x| (x, p.h - 1))
                .chain((0..p.h).map(|y| (p.w - 1, y)));
            for (x, y) in edge {
                sum += (read(x, y) - f64::from(p.y[y * p.w + x])).abs();
                n += 1;
            }
        });
        sum / n.max(1) as f64
    }

    /// The first SPS NAL unit of an access unit.
    fn sps_in(data: &[u8]) -> Option<Vec<u8>> {
        onicode_desktop_codec::nal::units(data)
            .find(|nal| hevc::nal_type(nal) == Some(hevc::SPS))
            .map(<[u8]>::to_vec)
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _written: std::fmt::Result = write!(out, "{b:02x}");
            out
        })
    }

    /// What one path of the probe gave: the keyframe's SPS as the encoder wrote it, the last
    /// decoded picture's size, its PSNR `(luma, chroma)` and edge error against the source.
    struct Path {
        sps: Vec<u8>,
        size: (usize, usize),
        psnr: (f64, f64),
        edge: f64,
    }

    /// Encode `pictures` on the worker's own session at `coded`, each drawn at the top-left
    /// with black around it, crop the stream to the pictures' size when `coded` is larger, and
    /// decode it on the client's decoder.
    fn through(
        pictures: &[Picture],
        coded: (usize, usize),
        chroma: onicode_desktop_codec::Chroma,
    ) -> Path {
        use onicode_desktop_codec::{Decoder, Encoder, EncoderConfig, FrameOptions};
        use onicode_desktop_media::wire::VideoCodec;

        let (w, h) = (pictures[0].w, pictures[0].h);
        let format = onicode_desktop_codec::pixel_format(chroma);
        let (tx, rx) = mpsc::channel();
        let encoder = Encoder::new(
            EncoderConfig {
                width: coded.0 as u32,
                height: coded.1 as u32,
                codec: VideoCodec::Hevc,
                fps: 60,
                bitrate_bps: 32_000_000,
                chroma,
            },
            move |packet| {
                let _gone = tx.send(packet);
            },
        )
        .expect("the worker's session");
        let images: Vec<_> = pictures
            .iter()
            .map(|p| fill(&padded(p, coded.0, coded.1), format))
            .collect();
        let mut packets = Vec::new();
        for (i, image) in images.iter().enumerate() {
            let options = FrameOptions {
                force_keyframe: i == 0,
                ..FrameOptions::default()
            };
            encoder
                .encode(image, (i as u64 + 1) * 16_667, &options)
                .expect("encode");
            packets.push(rx.recv_timeout(Duration::from_secs(10)).expect("a packet"));
        }
        let sps = sps_in(&packets[0].data).expect("an SPS in front of the keyframe");
        if coded != (w, h) {
            onicode_desktop_codec::conformance::crop_access_unit(
                &mut packets[0].data,
                (w as u32, h as u32),
            )
            .expect("the window fits");
        }
        let (dtx, drx) = mpsc::channel();
        let mut decoder = Decoder::new(VideoCodec::Hevc, move |frame| {
            let _gone = dtx.send(frame);
        });
        let mut last = None;
        for packet in &packets {
            decoder
                .decode(&packet.data.clone().into(), packet.pts_us)
                .expect("decode");
            last = Some(
                drx.recv_timeout(Duration::from_secs(10))
                    .expect("a picture"),
            );
        }
        let last = last.expect("a picture");
        let source = &pictures[pictures.len() - 1];
        Path {
            sps,
            size: (last.image.width(), last.image.height()),
            psnr: psnr(source, last.image.as_cv()),
            edge: edge_error(source, last.image.as_cv()),
        }
    }

    /// Probe P1 (`docs/decisions/video.md`, "Stream sides padded to 16"): a picture coded at a
    /// padded size with the SPS's conformance window rewritten decodes to the true size, with
    /// the same SPS the encoder writes for that size itself and the same quality, 4:2:0 and
    /// 4:4:4. Then whether `CleanAperture` on a session writes a window of its own.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn conformance_window() {
        use onicode_desktop_codec::Chroma;

        for ((w, h), chroma) in [
            ((3024_usize, 1964_usize), Chroma::Subsampled),
            ((3456, 2234), Chroma::Full),
        ] {
            let coded = (w.next_multiple_of(16), h.next_multiple_of(16));
            let pictures: Vec<Picture> = (0..8).map(|i| picture(w, h, i * 8)).collect();
            let native = through(&pictures, (w, h), chroma);
            let padded = through(&pictures, coded, chroma);
            let cropped =
                onicode_desktop_codec::conformance::crop_sps(&padded.sps, (w as u32, h as u32))
                    .expect("the window fits");
            eprintln!(
                "MEASURE p1 {w}x{h} {chroma:?} coded={}x{} native_sps={} padded_sps={} \
                 cropped_sps={} cropped==native:{} shown(native)={:?} shown(cropped)={:?}",
                coded.0,
                coded.1,
                hex(&native.sps),
                hex(&padded.sps),
                hex(&cropped),
                cropped == native.sps,
                hevc::shown_size(&native.sps),
                hevc::shown_size(&cropped),
            );
            for (name, path) in [("native", &native), ("padded+window", &padded)] {
                eprintln!(
                    "MEASURE p1 {w}x{h} {chroma:?} {name}: decoded={}x{} psnr_y={:.3}dB \
                     psnr_c={:.3}dB edge_error={:.2}",
                    path.size.0, path.size.1, path.psnr.0, path.psnr.1, path.edge
                );
            }
        }

        // 5. CleanAperture on a padded session: does the encoder write a window from it?
        let (w, h) = (3024_usize, 1968_usize);
        let session = Session::worker(w, h, 32_000_000, 60).expect("session");
        // SAFETY: framework-provided constant strings.
        let aperture = unsafe {
            dict(&[
                (
                    kCVImageBufferCleanApertureWidthKey,
                    &CFNumber::new_i64(3024),
                ),
                (
                    kCVImageBufferCleanApertureHeightKey,
                    &CFNumber::new_i64(1964),
                ),
                (
                    kCVImageBufferCleanApertureHorizontalOffsetKey,
                    &CFNumber::new_i64(0),
                ),
                (
                    kCVImageBufferCleanApertureVerticalOffsetKey,
                    &CFNumber::new_i64(-2),
                ),
            ])
        };
        // SAFETY: framework-provided constant string.
        let status = set(
            &session.vt,
            unsafe { kVTCompressionPropertyKey_CleanAperture },
            &aperture,
        );
        let image = fill(
            &picture(w, h, 0),
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        );
        let (_, sample) = session.encode(&image, 0);
        // SAFETY: a valid sample buffer.
        let format = sample.and_then(|s| unsafe { s.0.format_description() });
        let sps = format.as_deref().and_then(sps_nal);
        eprintln!(
            "MEASURE p1 clean_aperture set_status={status} sps={} shown={:?}",
            sps.as_deref().map(hex).unwrap_or_default(),
            sps.as_deref().and_then(hevc::shown_size),
        );
    }

    /// What a submit costs the thread that makes it, and how many source pictures the worker's
    /// session holds, while it encodes on a real-time beat. A capture callback submits on the
    /// capture's own queue, so a submit that blocks holds the next capture back; and a capture's
    /// pool must have every picture the encoder holds and one more to render into. Each
    /// submitted picture is a fresh one of a pool of 24, and before each submit the pictures
    /// whose retain count is above the pool's own are counted.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn submit_blocking_and_pictures_held() {
        for (w, h, cw, ch) in [
            (3024, 1964, 3024, 1964),
            (3024, 1964, 3024, 1968),
            (1920, 1080, 1920, 1088),
            (1920, 1080, 1920, 1080),
        ] {
            for fps in [60_i32, 120] {
                let session = Session::worker(cw, ch, 32_000_000, fps).expect("session");
                let pictures: Vec<Picture> = (0..24).map(|i| picture(w, h, i * 8)).collect();
                let images: Vec<_> = pictures
                    .iter()
                    .map(|p| {
                        fill(
                            &padded(p, cw, ch),
                            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                        )
                    })
                    .collect();
                let period = Duration::from_secs(1) / fps as u32;
                let start = Instant::now();
                let mut held = Vec::new();
                let mut blocked = Vec::new();
                let frames = fps as usize * 3;
                for i in 0..frames {
                    let due = start + period * i as u32;
                    if let Some(wait) = due.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                    held.push(images.iter().filter(|b| b.retain_count() > 1).count());
                    let submitting = Instant::now();
                    assert_eq!(
                        session.submit(&images[i % images.len()], i as i64),
                        0,
                        "submit"
                    );
                    blocked.push(submitting.elapsed());
                    while session.rx.try_recv().is_ok() {}
                }
                let submit = Spread::of_durations(&blocked[fps as usize..]).unwrap_or_default();
                let settled = &mut held[fps as usize..];
                settled.sort_unstable();
                eprintln!(
                    "MEASURE held {w}x{h} coded={cw}x{ch} fps={fps} load={}: submit call p50={:.2}ms p95={:.2}ms max={:.2}ms; pictures held before a submit p50={} p95={} max={}",
                    load_average(),
                    ms(submit.p50),
                    ms(submit.p95),
                    ms(submit.max),
                    settled[settled.len() / 2],
                    settled[settled.len() * 95 / 100],
                    settled[settled.len() - 1],
                );
            }
        }
    }

    /// User clients of the M2 scaler (`AppleM2ScalerCSCDriver`) this process has open, as
    /// `ioreg` lists them.
    fn scaler_clients() -> usize {
        let out = std::process::Command::new("/usr/sbin/ioreg")
            .args(["-r", "-c", "AppleM2ScalerCSCDriver", "-l", "-w0"])
            .output()
            .expect("ioreg");
        let ours = format!("pid {},", std::process::id());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.contains("IOUserClientCreator") && l.contains(&ours))
            .count()
    }

    /// Whether a session reaches for the M2 scaler: one fed pictures off 16 copies each into a
    /// padded buffer of its own, and a hosted runner's virtual Mac, which has no scaler
    /// (`IOServiceMatching failed for: AppleM2ScalerParavirtDriver`), was slow to the first
    /// keyframe of such a stream. Five frames each at the sizes the quality-change test codes,
    /// off 16 and padded; the scaler's user clients are counted after them.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn the_scaler_is_opened_only_off_16() {
        for (w, h) in [
            (3024, 1964),
            (3024, 1968),
            (1512, 982),
            (1520, 992),
            (756, 492),
            (768, 496),
        ] {
            let before = scaler_clients();
            let session = Session::worker(w, h, 20_000_000, 60).expect("session");
            let image = buffer(w, h, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange);
            let start = Instant::now();
            for i in 0..5_i64 {
                assert_eq!(session.submit(&image, i), 0, "submit");
            }
            for _ in 0..5 {
                let _frame = session
                    .rx
                    .recv_timeout(Duration::from_secs(5))
                    .expect("a frame");
            }
            eprintln!(
                "MEASURE scaler {w}x{h}: user clients before {before}, after five frames {}; {:.1} ms",
                scaler_clients(),
                start.elapsed().as_secs_f64() * 1e3,
            );
        }
    }

    /// The SPS NAL unit of a format description.
    fn sps_nal(format: &CMFormatDescription) -> Option<Vec<u8>> {
        let mut count = 0_usize;
        let mut index = 0_usize;
        loop {
            let mut ptr: *const u8 = ptr::null();
            let mut size = 0_usize;
            // SAFETY: valid out pointers; the bytes are owned by `format`.
            let status = unsafe {
                CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(
                    format,
                    index,
                    &raw mut ptr,
                    &raw mut size,
                    &raw mut count,
                    ptr::null_mut(),
                )
            };
            if status != 0 || ptr.is_null() {
                return None;
            }
            // SAFETY: CoreMedia returned `size` readable bytes at `ptr`.
            let nal = unsafe { std::slice::from_raw_parts(ptr, size) };
            if hevc::nal_type(nal) == Some(hevc::SPS) {
                return Some(nal.to_vec());
            }
            index += 1;
            if index >= count {
                return None;
            }
        }
    }

    // ---- Temporal layers: what a receiver may skip (probe P5) --------------------------------

    impl Session {
        /// Submit one picture with per-frame `options`; the status.
        fn submit_with(&self, image: &CVPixelBuffer, index: i64, options: Option<&Dict>) -> i32 {
            let pts = CMTime {
                value: index,
                timescale: self.fps,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            };
            // SAFETY: a valid image, session and options dictionary for the call; no refcon.
            unsafe {
                self.vt.encode_frame(
                    image,
                    pts,
                    kCMTimeInvalid,
                    options.map(CFDictionary::as_opaque),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            }
        }

        /// Encode one picture with `options` and wait for it.
        fn encode_with(
            &self,
            image: &CVPixelBuffer,
            index: i64,
            options: Option<&Dict>,
        ) -> (Duration, Option<Sample>) {
            let submitted = Instant::now();
            if self.submit_with(image, index, options) != 0 {
                return (Duration::ZERO, None);
            }
            match self.rx.recv_timeout(Duration::from_secs(10)) {
                Ok((at, sample)) => (at.duration_since(submitted), sample),
                Err(_) => (Duration::ZERO, None),
            }
        }
    }

    /// The `TemporalId` of a sample's first slice.
    fn temporal_id(sample: &CMSampleBuffer) -> Option<u8> {
        nal_headers(sample)
            .into_iter()
            .find(|(t, _)| *t < 32)
            .map(|(_, tid)| tid)
    }

    /// The long-term-reference token a sample asks the receiver to acknowledge.
    fn ltr_token(sample: &CMSampleBuffer) -> Option<i64> {
        // SAFETY: valid sample buffer; `false` never allocates.
        let array = unsafe { sample.sample_attachments_array(false) }?;
        // SAFETY: CMSampleBuffer.h: an array of CFDictionaries keyed by CFString.
        let array: CFRetained<CFArray<Dict>> = unsafe { CFRetained::cast_unchecked(array) };
        let d = array.get(0)?;
        // SAFETY: framework-provided constant string.
        let key = unsafe { kVTSampleAttachmentKey_RequireLTRAcknowledgementToken };
        d.get(key)
            .and_then(|v| v.downcast::<CFNumber>().ok())
            .and_then(|n| n.as_i64())
    }

    /// A hash of a decoded 4:2:0 picture's visible samples, both planes.
    fn picture_hash(image: &CVPixelBuffer) -> u64 {
        let (w, h) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
        let mut acc = 0xcbf2_9ce4_8422_2325_u64;
        with_planes(image, true, |plane, base, stride| {
            let rows = if plane == 0 { h } else { h / 2 };
            for y in 0..rows {
                // SAFETY: the plane is locked, so row `y < rows` starts inside it.
                let start = unsafe { base.add(y * stride) };
                // SAFETY: NV12 rows are `w <= stride` readable bytes in both planes.
                let row = unsafe { std::slice::from_raw_parts(start, w) };
                for &b in row {
                    acc = (acc ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
                }
            }
        });
        acc
    }

    /// Decode `samples` in order on one hardware session, going on past failures: per sample,
    /// the picture's hash or the status it failed with.
    fn decode_hashes(samples: &[&Sample]) -> Vec<Result<u64, i32>> {
        let mut out = Vec::with_capacity(samples.len());
        let decoded = decode_each(
            samples,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            true,
            |picture| {
                out.push(picture.map(|p| picture_hash(&p.0)));
                true
            },
        );
        if let Err(e) = decoded {
            panic!("decoder session: {e:?}");
        }
        out
    }

    /// One frame of the layered stream and what it says of itself.
    struct Marked {
        sample: Sample,
        tid: Option<u8>,
        depended: Option<bool>,
        token: Option<i64>,
        refresh: bool,
    }

    /// Frame options: a keyframe, an LTR refresh, the tokens acknowledged since the last frame.
    fn frame_options(keyframe: bool, refresh: bool, acked: &[i64]) -> Option<CFRetained<Dict>> {
        let tokens: Vec<CFRetained<CFNumber>> =
            acked.iter().map(|&t| CFNumber::new_i64(t)).collect();
        let tokens = CFArray::from_retained_objects(&tokens);
        // SAFETY: framework-provided constant strings.
        let (key, refresh_key, acked_key) = unsafe {
            (
                kVTEncodeFrameOptionKey_ForceKeyFrame,
                kVTEncodeFrameOptionKey_ForceLTRRefresh,
                kVTEncodeFrameOptionKey_AcknowledgedLTRTokens,
            )
        };
        let mut pairs: Vec<(&CFString, &CFType)> = Vec::new();
        if keyframe {
            pairs.push((key, yes()));
        }
        if refresh {
            pairs.push((refresh_key, yes()));
        }
        if !acked.is_empty() {
            pairs.push((acked_key, &tokens));
        }
        (!pairs.is_empty()).then(|| dict(&pairs))
    }

    /// A letter per frame: `K` keyframe, `B` base layer, `L` layer 1, then `r` for a refresh,
    /// `t` for a frame that offers an LTR token and `d` for one marked depended on.
    fn pattern(frames: &[Marked]) -> String {
        frames
            .iter()
            .map(|f| {
                let head = match f.tid {
                    _ if f.depended.is_none() && f.token.is_some() => "K",
                    Some(0) => "B",
                    Some(_) => "L",
                    None => "?",
                };
                let mut s = head.to_owned();
                if f.refresh {
                    s.push('r');
                }
                if f.token.is_some() {
                    s.push('t');
                }
                if f.tid.is_some_and(|t| t > 0) && f.depended != Some(false) {
                    s.push('d');
                }
                s
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Decode the frames `keep` selects and compare each picture with the whole stream's
    /// picture of the same frame: `(decoded, failed, differing)`.
    fn decode_without(
        frames: &[Marked],
        full: &[Result<u64, i32>],
        keep: impl Fn(usize, &Marked) -> bool,
    ) -> (usize, usize, usize) {
        let kept: Vec<usize> = (0..frames.len()).filter(|&i| keep(i, &frames[i])).collect();
        let samples: Vec<&Sample> = kept.iter().map(|&i| &frames[i].sample).collect();
        let got = decode_hashes(&samples);
        let mut out = (0, 0, 0);
        for (&i, picture) in kept.iter().zip(&got) {
            match (picture, &full[i]) {
                (Ok(a), Ok(b)) if a == b => out.0 += 1,
                (Ok(_), _) => {
                    out.0 += 1;
                    out.2 += 1;
                }
                (Err(_), _) => out.1 += 1,
            }
        }
        out
    }

    /// Probe P5 (`docs/decisions/video.md`, temporal layers): on the worker's session with
    /// `BaseLayerFrameRateFraction` 0.5, which frames nothing refers to, whether a decoder given
    /// the stream without some or all of them decodes every other frame to the very picture
    /// the whole stream gives, with LTR tokens acknowledged and a refresh in the middle; whether
    /// layers can be turned on and off on a live session; and what they cost in encode time,
    /// bytes and picture at 1080p and 3024 × 1968, with each `BaseLayerBitRateFraction` Apple
    /// suggests.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn temporal_layers_skip_and_toggle() {
        type Keep<'a> = Box<dyn Fn(usize, &Marked) -> bool + 'a>;
        // SAFETY: framework-provided constant strings.
        let (fraction_key, bit_key) = unsafe {
            (
                kVTCompressionPropertyKey_BaseLayerFrameRateFraction,
                kVTCompressionPropertyKey_BaseLayerBitRateFraction,
            )
        };
        let (w, h) = (1920, 1088);
        let (_, images) = scrolling(w, h);

        let only_cost = std::env::var_os("ONICODE_PROBE_LAYERS_COST_ONLY").is_some();
        if !only_cost {
            // 1. What may be skipped, with LTR acknowledged and a refresh at frame 45.
            let session = Session::worker(w, h, 16_000_000, 60).expect("session");
            let status = set(&session.vt, fraction_key, &CFNumber::new_f64(0.5));
            let mut acked: Vec<i64> = Vec::new();
            let mut frames: Vec<Marked> = Vec::new();
            for i in 0..90_usize {
                let refresh = i == 45;
                let options = frame_options(i == 0, refresh, &acked);
                acked.clear();
                let (_, sample) =
                    session.encode_with(&images[i % images.len()], i as i64, options.as_deref());
                let sample = sample.expect("a frame");
                let (depended, ..) = sample_marks(&sample.0);
                let token = ltr_token(&sample.0);
                acked.extend(token);
                frames.push(Marked {
                    tid: temporal_id(&sample.0),
                    depended,
                    token,
                    refresh,
                    sample,
                });
            }
            eprintln!(
                "MEASURE layers set_status={status} pattern: {}",
                pattern(&frames)
            );
            let all: Vec<&Sample> = frames.iter().map(|f| &f.sample).collect();
            let full = decode_hashes(&all);
            let full_failed = full.iter().filter(|r| r.is_err()).count();
            eprintln!(
                "MEASURE layers whole stream: {} frames, {full_failed} failed",
                full.len()
            );
            let layer1 = |f: &Marked| f.tid.is_some_and(|t| t > 0);
            let control = (20..frames.len())
                .find(|&i| !layer1(&frames[i]))
                .unwrap_or(20);
            let cases: [(&str, Keep<'_>); 4] = [
                ("every layer-1 frame dropped", Box::new(|_, f| !layer1(f))),
                (
                    "every other layer-1 frame dropped",
                    Box::new(|i, f| !(layer1(f) && i % 4 == 1)),
                ),
                (
                    "the layer-1 frames around the refresh dropped",
                    Box::new(|i, f| !(layer1(f) && (43..=48).contains(&i))),
                ),
                (
                    "one base frame dropped (control)",
                    Box::new(move |i, _| i != control),
                ),
            ];
            for (name, keep) in cases {
                let (decoded, failed, differing) = decode_without(&frames, &full, keep);
                eprintln!(
                    "MEASURE layers {name}: decoded={decoded} failed={failed} differing_from_whole={differing}"
                );
            }

            // 1b. A refresh after a lost base frame: the client decodes frames 0–29, loses base
            // frame 30 and acknowledges nothing from there until the refresh it asks for, which the
            // worker makes at frame 36. From the refresh on, every frame must decode to the whole
            // stream's picture with frames 30–35 never given to the decoder.
            for (lost, refresh_at, label) in [
                (30_usize, 36_usize, "base"),
                (30, 37, "base"),
                (31, 0, "layer-1"),
            ] {
                let session = Session::worker(w, h, 16_000_000, 60).expect("session");
                set(&session.vt, fraction_key, &CFNumber::new_f64(0.5));
                let mut acked: Vec<i64> = Vec::new();
                let mut frames: Vec<Marked> = Vec::new();
                let layer1_lost = label == "layer-1";
                for i in 0..90_usize {
                    let refresh = i == refresh_at && !layer1_lost;
                    let options = frame_options(i == 0, refresh, &acked);
                    acked.clear();
                    let (_, sample) = session.encode_with(
                        &images[i % images.len()],
                        i as i64,
                        options.as_deref(),
                    );
                    let sample = sample.expect("a frame");
                    let (depended, ..) = sample_marks(&sample.0);
                    let token = ltr_token(&sample.0);
                    // The client acknowledges only what it decoded: nothing between the loss and
                    // the refresh when a base frame went, only the lost frame
                    // itself when it was layer 1.
                    let decoded = if layer1_lost {
                        i != lost
                    } else {
                        i < lost || i >= refresh_at
                    };
                    if decoded {
                        acked.extend(token);
                    }
                    frames.push(Marked {
                        tid: temporal_id(&sample.0),
                        depended,
                        token,
                        refresh,
                        sample,
                    });
                }
                let all: Vec<&Sample> = frames.iter().map(|f| &f.sample).collect();
                let full = decode_hashes(&all);
                let tid_lost = frames[lost].tid;
                let (decoded, failed, differing) = decode_without(&frames, &full, |i, _| {
                    if layer1_lost {
                        i != lost
                    } else {
                        i < lost || i >= refresh_at
                    }
                });
                eprintln!(
                    "MEASURE layers recovery, {label} frame {lost} lost (TemporalId {tid_lost:?}): \
                     decoded={decoded} failed={failed} differing_from_whole={differing}; pattern from 26: {}",
                    pattern(&frames[26..44])
                );
            }

            // 2. On and off on a live session.
            let session = Session::worker(w, h, 16_000_000, 60).expect("session");
            let mut index = 0_i64;
            for (label, fraction) in [
                ("unset", None),
                ("0.5", Some(0.5)),
                ("1.0", Some(1.0)),
                ("0.5 again", Some(0.5)),
            ] {
                let status =
                    fraction.map(|f| set(&session.vt, fraction_key, &CFNumber::new_f64(f)));
                let read_back = copy_property(&session.vt, fraction_key).map(|v| describe(&v));
                let mut segment = Vec::new();
                for _ in 0..16 {
                    let options = frame_options(index == 0, false, &[]);
                    let (_, sample) = session.encode_with(
                        &images[index as usize % images.len()],
                        index,
                        options.as_deref(),
                    );
                    index += 1;
                    let sample = sample.expect("a frame");
                    let (depended, ..) = sample_marks(&sample.0);
                    segment.push(Marked {
                        tid: temporal_id(&sample.0),
                        depended,
                        token: ltr_token(&sample.0),
                        refresh: false,
                        sample,
                    });
                }
                eprintln!(
                    "MEASURE layers live {label}: set_status={status:?} read_back={read_back:?} pattern: {}",
                    pattern(&segment)
                );
            }
        }

        // 3. The cost, on a real-time beat: at a rate the picture does not reach, where layers
        // cost bits, and at one it does, where they cost picture.
        for ((w, h), bitrate) in [
            ((1920_usize, 1088_usize), 16_000_000_i64),
            ((1920, 1088), 3_000_000),
            ((3024, 1968), 32_000_000),
            ((3024, 1968), 6_000_000),
        ] {
            let pictures: Vec<Picture> = (0..12).map(|i| picture(w, h, i * 8)).collect();
            let images: Vec<_> = pictures
                .iter()
                .map(|p| fill(p, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange))
                .collect();
            for (label, layers, base_bits) in [
                ("none", None, None),
                ("0.5", Some(0.5), None),
                ("0.5, base bits 0.7", Some(0.5), Some(0.7)),
                ("0.5, base bits 0.8", Some(0.5), Some(0.8)),
                ("0.67, base bits 0.8", Some(0.67), Some(0.8)),
                ("0.75, base bits 0.8", Some(0.75), Some(0.8)),
            ] {
                let session = Session::worker(w, h, bitrate, 60).expect("session");
                if let Some(fraction) = layers {
                    set(&session.vt, fraction_key, &CFNumber::new_f64(fraction));
                }
                if let Some(bits) = base_bits {
                    set(&session.vt, bit_key, &CFNumber::new_f64(bits));
                }
                let beat = on_beat(&session, &images, 240, 60);
                let spread = Spread::of_durations(&beat.times).unwrap_or_default();
                let (mut base, mut upper) = ((0_usize, 0_usize), (0_usize, 0_usize));
                for (_, s) in beat.samples.iter().skip(60) {
                    let bytes = sample_bytes(&s.0);
                    let slot = if temporal_id(&s.0).is_some_and(|t| t > 0) {
                        &mut upper
                    } else {
                        &mut base
                    };
                    slot.0 += 1;
                    slot.1 += bytes;
                }
                let spent = (beat.bytes * 8) as f64 / 3.0 / 1e6;
                let psnr_y = tail_psnr(beat.samples, &pictures);
                eprintln!(
                    "MEASURE layers cost {w}x{h} {label} load={}: on_beat p50={:.2}ms p95={:.2}ms dropped={} \
                     base {} x {} B, layer-1 {} x {} B, spent={spent:.2}Mbit/s psnr_y={psnr_y:.2}dB",
                    load_average(),
                    ms(spread.p50),
                    ms(spread.p95),
                    beat.dropped,
                    base.0,
                    base.1 / base.0.max(1),
                    upper.0,
                    upper.1 / upper.0.max(1),
                );
            }
        }
    }

    // ---- A still picture refined (probe P6) ---------------------------------------------------

    /// The stamp of the `k`th refinement (from 1) after a last moving frame stamped `last`, as
    /// the worker stamps them at a 60 Hz rung: sent two periods apart, each stamped a period
    /// before it was sent (`onicode_desktop_media::Refine::stamp`).
    fn refinement_stamp(last: u64, k: u64) -> u64 {
        last + k * 2 * 16_667 - 16_667
    }

    /// Where the worker's policy stops (`onicode_desktop_media::Refine::wanted`, copied: the probe
    /// does not link the media crate): the number of refinements it sends after the fresh frame's
    /// error `fresh`, the refinements' errors being `mse`. A frame the encoder dropped reports
    /// none, as on the worker, and the policy goes on blind for a few frames.
    fn refinements_sent(fresh: Option<f64>, mse: &[Option<f64>]) -> usize {
        const BLIND: usize = 4;
        // Hundredths of a dB, as `gain_centi_db`.
        let gain = |before: f64, after: f64| 1000.0 * (before / after).log10();
        let mut record = [fresh, None, None];
        for (sent, m) in mse.iter().enumerate() {
            let wanted = match record {
                [Some(newest), ..] if newest <= 0.0 => false,
                [Some(newest), _, Some(two_back)] => gain(two_back, newest) >= 20.0,
                [Some(newest), Some(one_back), None] => gain(one_back, newest) >= 10.0,
                [Some(_), None, _] => true,
                [None, ..] => sent < BLIND,
            };
            if !wanted {
                return sent;
            }
            record = [*m, record[0], record[1]];
        }
        mse.len()
    }

    /// Probe P6 (`docs/decisions/video.md`, still-picture refinement): the text scrolls for half
    /// a second on the worker's session, then stops, and the last picture is handed to the
    /// encoder again with the stamps the worker gives refinements ([`refinement_stamp`]). Each
    /// frame after the stop: its bytes against the rate's per-frame budget, how long the encoder
    /// took, the error the encoder reports, and the luma and chroma PSNR of the decoded picture
    /// against the source; and where the worker's policy stops. 4:2:0 at 4, 8 and 16 Mbit/s,
    /// 4:4:4 at 8, 16 and 32; `ONICODE_PROBE_SIZES` picks the sizes (1920 × 1088).
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn still_picture_refinement() {
        use onicode_desktop_codec::{Chroma, Decoder, Encoder, EncoderConfig, FrameOptions};
        use onicode_desktop_media::wire::VideoCodec;

        const MOVING: usize = 30;
        const STILL: usize = 12;
        for (w, h) in probe_sizes(&[(1920, 1088)]) {
            let pictures: Vec<Picture> = (0..MOVING).map(|i| picture(w, h, i * 16)).collect();
            for (chroma, rates) in [
                (Chroma::Subsampled, [4_000_000_u32, 8_000_000, 16_000_000]),
                (Chroma::Full, [8_000_000, 16_000_000, 32_000_000]),
            ] {
                let format = onicode_desktop_codec::pixel_format(chroma);
                let images: Vec<_> = pictures.iter().map(|p| fill(p, format)).collect();
                for rate in rates {
                    let (tx, rx) = mpsc::channel();
                    let encoder = Encoder::new(
                        EncoderConfig {
                            width: w as u32,
                            height: h as u32,
                            codec: VideoCodec::Hevc,
                            fps: 60,
                            bitrate_bps: rate,
                            chroma,
                        },
                        move |packet| {
                            let _gone = tx.send(packet);
                        },
                    )
                    .expect("the worker's session");
                    // A frame the encoder dropped comes back as nothing: the picture before it
                    // stands, and its bytes are none.
                    let mut packets: Vec<Option<onicode_desktop_codec::EncodedPacket>> = Vec::new();
                    let mut took = Vec::new();
                    for i in 0..MOVING + STILL {
                        let image = &images[i.min(MOVING - 1)];
                        let options = FrameOptions {
                            force_keyframe: i == 0,
                            ..FrameOptions::default()
                        };
                        let start = Instant::now();
                        let last = MOVING as u64 * 16_667;
                        let pts = if i < MOVING {
                            (i as u64 + 1) * 16_667
                        } else {
                            refinement_stamp(last, (i + 1 - MOVING) as u64)
                        };
                        encoder.encode(image, pts, &options).expect("encode");
                        encoder.flush().expect("flush");
                        took.push(start.elapsed());
                        packets.push(rx.try_recv().ok());
                    }
                    let dropped = packets.iter().filter(|p| p.is_none()).count();
                    let (dtx, drx) = mpsc::channel();
                    let mut decoder = Decoder::new(VideoCodec::Hevc, move |frame| {
                        let _gone = dtx.send(frame);
                    });
                    let mut scores = Vec::new();
                    let mut shown = (0.0, 0.0);
                    for (i, packet) in packets.iter().enumerate() {
                        if let Some(packet) = packet {
                            decoder
                                .decode(&packet.data.clone().into(), packet.pts_us)
                                .expect("decode");
                            let decoded = drx
                                .recv_timeout(Duration::from_secs(10))
                                .expect("a picture");
                            shown = psnr(&pictures[i.min(MOVING - 1)], decoded.image.as_cv());
                        }
                        scores.push(shown);
                    }
                    let bytes = |i: usize| packets[i].as_ref().map_or(0, |p| p.data.len());
                    let budget = rate as usize / 8 / 60;
                    let moving_took = Spread::of_durations(&took[5..MOVING]).unwrap_or_default();
                    let still_took = Spread::of_durations(&took[MOVING..]).unwrap_or_default();
                    let last = MOVING - 1;
                    let best = scores[MOVING..]
                        .iter()
                        .map(|s| s.0)
                        .fold(f64::MIN, f64::max);
                    let crisp = scores[MOVING..]
                        .iter()
                        .position(|s| s.0 >= best - 0.3)
                        .map_or(STILL, |k| k + 1);
                    let over_budget = (MOVING..MOVING + STILL)
                        .filter(|&i| bytes(i) > budget)
                        .count();
                    let series: Vec<Option<f64>> = packets[MOVING..]
                        .iter()
                        .map(|p| p.as_ref().and_then(|p| p.mse).map(|m| m.luma))
                        .collect();
                    let fresh = packets[last].as_ref().and_then(|p| p.mse).map(|m| m.luma);
                    let stops = refinements_sent(fresh, &series);
                    let spent: usize = (MOVING..MOVING + stops).map(bytes).sum();
                    eprintln!(
                        "MEASURE refine {w}x{h} {chroma:?} {} Mbit/s load={}: moving p50 {:.2} ms, \
                         refinement p50 {:.2} ms; stop at luma {:.2} dB chroma {:.2} dB ({} B); \
                         after 1/2/4/8 refinement frames luma {:.2}/{:.2}/{:.2}/{:.2} dB, chroma \
                         {:.2}/{:.2}/{:.2}/{:.2} dB; within 0.3 dB of the best ({best:.2}) after \
                         {crisp}; {over_budget} of {STILL} over the {budget} B budget; \
                         {dropped} dropped; the policy sends {stops} ({spent} B) and stops at \
                         luma {:.2} dB",
                        rate / 1_000_000,
                        load_average(),
                        ms(moving_took.p50),
                        ms(still_took.p50),
                        scores[last].0,
                        scores[last].1,
                        bytes(last),
                        scores[MOVING].0,
                        scores[MOVING + 1].0,
                        scores[MOVING + 3].0,
                        scores[MOVING + 7].0,
                        scores[MOVING].1,
                        scores[MOVING + 1].1,
                        scores[MOVING + 3].1,
                        scores[MOVING + 7].1,
                        scores[(MOVING + stops).max(last + 1) - 1].0,
                    );
                    let series: Vec<String> = (last..MOVING + STILL)
                        .map(|i| {
                            format!(
                                "{}:{:.1}/{:.1}dB {}B mse_psnr={}",
                                i as isize - last as isize,
                                scores[i].0,
                                scores[i].1,
                                bytes(i),
                                packets[i].as_ref().and_then(|p| p.mse).map_or_else(
                                    || "-".to_owned(),
                                    |m| format!("{:.1}", m.luma_psnr())
                                ),
                            )
                        })
                        .collect();
                    eprintln!(
                        "MEASURE refine series {chroma:?} {}M: {}",
                        rate / 1_000_000,
                        series.join(" ")
                    );
                }
            }
        }
    }

    // ---- Stripes across the two media engines (probe P3) ------------------------------------

    /// `(first row, rows)` of each of `n` horizontal stripes of a picture `h` rows tall: each the
    /// nearest multiple of 64 rows (the HEVC CTU) to an even share, the last taking what is left.
    fn stripe_rows(h: usize, n: usize) -> Vec<(usize, usize)> {
        let share = ((h / n + 32) / 64 * 64).max(64);
        let mut out = Vec::with_capacity(n);
        let mut top = 0;
        for k in 0..n {
            let rows = if k + 1 == n { h - top } else { share };
            out.push((top, rows));
            top += rows;
        }
        out
    }

    /// The rows each stripe codes: its own, and `overlap` more on each side that meets another
    /// stripe, so a scroll across a seam still finds its reference inside the stripe. The client
    /// shows only the stripe's own rows.
    fn coded_rows(stripes: &[(usize, usize)], h: usize, overlap: usize) -> Vec<(usize, usize)> {
        stripes
            .iter()
            .map(|&(top, rows)| {
                let first = top.saturating_sub(overlap);
                let end = (top + rows + overlap).min(h);
                (first, end - first)
            })
            .collect()
    }

    /// Rows `top..top + rows` of `p`, as a picture of their own.
    fn rows_of(p: &Picture, top: usize, rows: usize) -> Picture {
        let range = top * p.w..(top + rows) * p.w;
        Picture {
            w: p.w,
            h: rows,
            y: p.y[range.clone()].to_vec(),
            cb: p.cb[range.clone()].to_vec(),
            cr: p.cr[range.clone()].to_vec(),
            rgb: p.rgb[range].to_vec(),
        }
    }

    /// What one stripe's session did in a striped run.
    struct StripeRun {
        /// `UsingHardwareAcceleratedVideoEncoder` as the session reads it.
        hardware: String,
        /// `RecommendedParallelizationLimit` as the session reads it.
        parallel: String,
        /// Submit → callback per frame with one frame in flight, every stripe submitted at once.
        serial: Vec<Option<(Duration, Instant)>>,
        /// The frames coded one at a time, which the beat's frames refer back to.
        lead: Vec<Sample>,
        /// When the beat's first frame was due, the same for every stripe.
        start: Instant,
        /// When each frame on the beat went in.
        submitted: Vec<Instant>,
        /// Each frame on the beat as it came back.
        back: Vec<(Instant, Option<Sample>)>,
    }

    /// Everything the stripe threads share: the barrier they submit together on, the instant
    /// the beat starts, and whether any of them could not open its session.
    struct Together {
        barrier: std::sync::Barrier,
        start: std::sync::OnceLock<Instant>,
        refused: std::sync::atomic::AtomicBool,
    }

    /// One stripe's session, fed rows `top..top + rows` of `pictures`: `serial` frames one at a
    /// time, then `frames` on the beat, all in step with the other stripes' threads.
    fn stripe_run(
        (w, h): (usize, usize),
        (top, rows, shown): (usize, usize, usize),
        (bitrate, fps): (i64, i32),
        pictures: &[Picture],
        (serial, frames): (usize, usize),
        together: &Together,
    ) -> Result<StripeRun, Failure> {
        use std::sync::atomic::Ordering;
        // The stripe's share of the target is its share of the picture it shows: the rows past
        // a seam it codes too are the other stripe's to pay for.
        let share = bitrate * shown as i64 / h as i64;
        let spec = if std::env::var("ONICODE_PROBE_ENCODER").as_deref() == Ok("plain") {
            Spec::Hardware
        } else {
            Spec::LowLatencyHardware
        };
        let session = Session::worker_on(spec, w, rows, share, fps).and_then(|session| {
            let Some(expect) = std::env::var("ONICODE_PROBE_EXPECT")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
            else {
                return Ok(session);
            };
            // SAFETY: framework-provided constant string.
            let key = unsafe { kVTCompressionPropertyKey_ExpectedFrameRate };
            match set(&session.vt, key, &CFNumber::new_i64(expect)) {
                0 => Ok(session),
                status => Err(("ExpectedFrameRate", status)),
            }
        });
        if session.is_err() {
            together.refused.store(true, Ordering::SeqCst);
        }
        let images: Vec<_> = pictures
            .iter()
            .map(|p| {
                fill(
                    &rows_of(p, top, rows),
                    kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
                )
            })
            .collect();
        together.barrier.wait();
        let session = session?;
        if together.refused.load(Ordering::SeqCst) {
            return Err(("another stripe refused", 0));
        }
        // SAFETY: framework-provided constant string.
        let parallel = copy_property(&session.vt, unsafe {
            kVTCompressionPropertyKey_RecommendedParallelizationLimit
        })
        .map_or_else(|status| format!("status {status}"), |v| describe(&v));
        let mut lead = Vec::with_capacity(serial);
        let serial = (0..serial)
            .map(|i| {
                together.barrier.wait();
                let submitted = Instant::now();
                let (took, sample) = session.encode(&images[i % images.len()], i as i64);
                sample.map(|sample| {
                    lead.push(sample);
                    (took, submitted + took)
                })
            })
            .collect();
        let stride = (120 / session.fps.max(1)) as usize;
        let period = Duration::from_secs(1) / session.fps as u32;
        together.barrier.wait();
        let start = *together
            .start
            .get_or_init(|| Instant::now() + Duration::from_millis(20));
        let mut submitted = Vec::with_capacity(frames);
        let mut back = Vec::with_capacity(frames);
        for i in 0..frames {
            let due = start + period * i as u32;
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            submitted.push(Instant::now());
            let status = session.submit(&images[(i * stride) % images.len()], 1_000 + i as i64);
            if status != 0 {
                return Err(("submit", status));
            }
            while let Ok(got) = session.rx.try_recv() {
                back.push(got);
            }
        }
        while back.len() < frames {
            match session.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(got) => back.push(got),
                Err(_) => break,
            }
        }
        // SAFETY: framework-provided constant string.
        let hardware = copy_property(&session.vt, unsafe {
            kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder
        })
        .map_or_else(|status| format!("status {status}"), |v| describe(&v));
        Ok(StripeRun {
            hardware,
            parallel,
            serial,
            lead,
            start,
            submitted,
            back,
        })
    }

    /// The last `keep` decoded pictures of `back` on the beat, each with its source's index.
    fn decoded_tail(
        run: &StripeRun,
        stride: usize,
        sources: usize,
        keep: usize,
    ) -> Vec<(usize, Pixels)> {
        let (indices, samples): (Vec<usize>, Vec<&Sample>) = run
            .lead
            .iter()
            .map(|s| (usize::MAX, s))
            .chain(
                run.back
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (_, s))| s.as_ref().map(|s| ((i * stride) % sources, s))),
            )
            .unzip();
        let mut all = Vec::new();
        let decoded = decode_each(
            &samples,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            true,
            |picture| picture.map(|p| all.push(p)).is_ok(),
        );
        if !matches!(decoded, Ok((_, None))) || all.len() != indices.len() {
            return Vec::new();
        }
        let first = all.len().saturating_sub(keep).max(run.lead.len());
        indices.into_iter().zip(all).skip(first).collect()
    }

    /// Two and four low-latency sessions on horizontal stripes of one picture against one
    /// session on the whole (design §3.3): every stripe submitted at the same instant, one frame
    /// in flight and then on a 60 and a 120 beat, a frame counting when its last stripe comes
    /// back. Stripes pay off only if the sessions run on the chip's two encode engines at once;
    /// if they take turns on one, the later stripe comes back when the whole picture would have.
    /// Also prints each session's `RecommendedParallelizationLimit`, the rate the stripes spend
    /// together, and the luma PSNR of the whole picture and of the 8 rows each side of a seam,
    /// striped against whole. `ONICODE_PROBE_SIZES` picks sizes, `ONICODE_PROBE_STRIPES=2,4`
    /// counts, and `ONICODE_PROBE_ENCODER=plain` runs the hardware encoder without low-latency rate
    /// control (the one that codes a 5K frame in half the time), set up otherwise the same.
    /// `ONICODE_PROBE_EXPECT=120` tells every session that frame rate whatever the beat, and
    /// `ONICODE_PROBE_OVERLAP=64` codes that many rows past each seam ([`coded_rows`]).
    /// `ONICODE_PROBE_RATE` sets the whole picture's target in bits a second.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn stripes_across_engines() {
        const SECONDS: usize = 4;
        const SERIAL: usize = 40;
        const KEEP: usize = 8;
        const BAND: usize = 8;
        let sizes = probe_sizes(&[(1920, 1088), (3024, 1968), (3840, 2160), (5120, 2880)]);
        let overlap: usize = std::env::var("ONICODE_PROBE_OVERLAP")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let counts: Vec<usize> = std::env::var("ONICODE_PROBE_STRIPES").ok().map_or_else(
            || vec![1, 2, 4],
            |list| {
                list.split(',')
                    .filter_map(|n| n.trim().parse().ok())
                    .collect()
            },
        );
        for (w, h) in sizes {
            let pictures: Vec<Picture> = (0..12).map(|i| picture(w, h, i * 8)).collect();
            let bitrate: i64 = std::env::var("ONICODE_PROBE_RATE")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(if w * h > 4_000_000 {
                    40_000_000
                } else {
                    32_000_000
                });
            for fps in [60_i32, 120] {
                let mut whole: Option<Vec<(usize, Pixels)>> = None;
                for &n in &counts {
                    let stripes = stripe_rows(h, n);
                    let coded = coded_rows(&stripes, h, overlap);
                    let together = Together {
                        barrier: std::sync::Barrier::new(n),
                        start: std::sync::OnceLock::new(),
                        refused: std::sync::atomic::AtomicBool::new(false),
                    };
                    let frames = fps as usize * SECONDS;
                    let runs: Vec<Result<StripeRun, Failure>> = std::thread::scope(|scope| {
                        let handles: Vec<_> = coded
                            .iter()
                            .zip(&stripes)
                            .map(|(&(top, rows), &(_, shown))| {
                                let stripe = (top, rows, shown);
                                let (pictures, together) = (&pictures, &together);
                                scope.spawn(move || {
                                    stripe_run(
                                        (w, h),
                                        stripe,
                                        (bitrate, fps),
                                        pictures,
                                        (SERIAL, frames),
                                        together,
                                    )
                                })
                            })
                            .collect();
                        handles
                            .into_iter()
                            .map(|h| h.join().unwrap_or(Err(("panicked", 0))))
                            .collect()
                    });
                    let runs = match runs.into_iter().collect::<Result<Vec<_>, _>>() {
                        Ok(runs) => runs,
                        Err(e) => {
                            eprintln!("MEASURE stripes {w}x{h} n={n} fps={fps} refused {e:?}");
                            continue;
                        }
                    };
                    let settle = fps as usize;
                    let each: Vec<Vec<(Duration, Instant)>> = (5..SERIAL)
                        .filter_map(|i| {
                            runs.iter().map(|r| r.serial[i]).collect::<Option<Vec<_>>>()
                        })
                        .collect();
                    let serial: Vec<Duration> = each
                        .iter()
                        .filter_map(|frame| frame.iter().map(|(took, _)| *took).max())
                        .collect();
                    // First → last stripe back with one frame in flight, all submitted together:
                    // near 0 side by side, near a stripe's encode when they take turns.
                    let serial_skew: Vec<Duration> = each
                        .iter()
                        .filter_map(|frame| {
                            let last = frame.iter().map(|(_, at)| *at).max()?;
                            let first = frame.iter().map(|(_, at)| *at).min()?;
                            Some(last.saturating_duration_since(first))
                        })
                        .collect();
                    let serial_skew = Spread::of_durations(&serial_skew).unwrap_or_default();
                    let period = Duration::from_secs(1) / fps as u32;
                    let start = runs.first().map(|r| r.start);
                    let (mut late, mut took, mut skew, mut dropped) =
                        (Vec::new(), Vec::new(), Vec::new(), 0);
                    for i in settle..frames {
                        let back: Option<Vec<Instant>> = runs
                            .iter()
                            .map(|r| r.back.get(i).filter(|b| b.1.is_some()).map(|b| b.0))
                            .collect();
                        let (Some(back), Some(start)) = (back, start) else {
                            dropped += 1;
                            continue;
                        };
                        let (Some(&last), Some(&first)) = (back.iter().max(), back.iter().min())
                        else {
                            continue;
                        };
                        late.push(last.saturating_duration_since(start + period * i as u32));
                        let own = runs
                            .iter()
                            .zip(&back)
                            .map(|(r, at)| at.saturating_duration_since(r.submitted[i]))
                            .max()
                            .unwrap_or_default();
                        took.push(own);
                        skew.push(last.saturating_duration_since(first));
                    }
                    let bytes: usize = runs
                        .iter()
                        .flat_map(|r| r.back.iter().skip(settle))
                        .filter_map(|(_, s)| s.as_ref().map(|s| sample_bytes(&s.0)))
                        .sum();
                    let spent = (bytes * 8) as f64 / (SECONDS - 1) as f64 / 1e6;
                    let submitted_fps = runs
                        .iter()
                        .map(|r| {
                            let span = r.submitted.last().zip(r.submitted.first());
                            let secs = span.map_or(0.0, |(l, f)| (*l - *f).as_secs_f64());
                            (r.submitted.len().saturating_sub(1)) as f64 / secs.max(1e-9)
                        })
                        .fold(f64::INFINITY, f64::min);
                    let stride = (120 / fps) as usize;
                    let tails: Vec<Vec<(usize, Pixels)>> = runs
                        .iter()
                        .map(|r| decoded_tail(r, stride, pictures.len(), KEEP))
                        .collect();
                    let psnr_whole = if tails.iter().any(Vec::is_empty) {
                        f64::NAN
                    } else {
                        let weighted: f64 = tails
                            .iter()
                            .zip(stripes.iter().zip(&coded))
                            .map(|(tail, (&(top, rows), &(first, _)))| {
                                let mean = tail
                                    .iter()
                                    .map(|(src, p)| {
                                        let own = rows_of(&pictures[*src], top, rows);
                                        psnr_at(&own, &p.0, top - first).0
                                    })
                                    .sum::<f64>()
                                    / tail.len() as f64;
                                mean * rows as f64
                            })
                            .sum();
                        weighted / h as f64
                    };
                    // The rows each side of every seam: striped, and the same rows of the whole.
                    let seams: Vec<String> = stripes
                        .windows(2)
                        .enumerate()
                        .map(|(k, pair)| {
                            let [(top, rows), (next, _)] = [pair[0], pair[1]];
                            let (upper_first, lower_first) = (coded[k].0, coded[k + 1].0);
                            let (upper, lower) = (&tails[k], &tails[k + 1]);
                            let striped = upper
                                .iter()
                                .zip(lower)
                                .map(|((su, pu), (sl, pl))| {
                                    let above = rows_of(&pictures[*su], next - BAND, BAND);
                                    let below = rows_of(&pictures[*sl], next, BAND);
                                    f64::midpoint(
                                        psnr_at(&above, &pu.0, next - BAND - upper_first).0,
                                        psnr_at(&below, &pl.0, next - lower_first).0,
                                    )
                                })
                                .sum::<f64>()
                                / upper.len().min(lower.len()).max(1) as f64;
                            let one = whole.as_ref().map_or(f64::NAN, |tail| {
                                tail.iter()
                                    .map(|(src, p)| {
                                        let band = rows_of(&pictures[*src], next - BAND, 2 * BAND);
                                        psnr_at(&band, &p.0, next - BAND).0
                                    })
                                    .sum::<f64>()
                                    / tail.len().max(1) as f64
                            });
                            debug_assert_eq!(top + rows, next);
                            format!("{next}:{striped:.2}/{one:.2}dB")
                        })
                        .collect();
                    if std::env::var_os("ONICODE_PROBE_TRACE").is_some()
                        && let Some(start) = start
                    {
                        for i in (0..frames).step_by(fps as usize / 4) {
                            let at = |t: Instant| {
                                ms(t.saturating_duration_since(start).as_nanos() as u64)
                            };
                            let backs: Vec<String> = runs
                                .iter()
                                .map(|r| {
                                    format!(
                                        "in {:.1} back {:.1}",
                                        at(r.submitted[i]),
                                        r.back.get(i).map_or(f64::NAN, |b| at(b.0))
                                    )
                                })
                                .collect();
                            eprintln!(
                                "TRACE {i} due {:.1}: {}",
                                ms((period * i as u32).as_nanos() as u64),
                                backs.join(", ")
                            );
                        }
                    }
                    let serial = Spread::of_durations(&serial).unwrap_or_default();
                    let late = Spread::of_durations(&late).unwrap_or_default();
                    let took = Spread::of_durations(&took).unwrap_or_default();
                    let skew = Spread::of_durations(&skew).unwrap_or_default();
                    let rows: Vec<usize> = coded.iter().map(|s| s.1).collect();
                    let hardware: Vec<&str> = runs.iter().map(|r| r.hardware.as_str()).collect();
                    eprintln!(
                        "MEASURE stripes {w}x{h} n={n} overlap={overlap} rows={rows:?} fps={fps} load={} \
                         hardware={hardware:?} parallel_limit={} | one_in_flight p50={:.2}ms \
                         p95={:.2}ms skew p50={:.2}ms p95={:.2}ms | on_beat late p50={:.2}ms p95={:.2}ms max={:.2}ms \
                         took p50={:.2}ms skew p50={:.2}ms p95={:.2}ms dropped={dropped}/{} \
                         submitted={submitted_fps:.1}fps spent={spent:.2}Mbit/s \
                         psnr_y={psnr_whole:.2}dB seams(striped/whole)={}",
                        load_average(),
                        runs.first().map_or("", |r| r.parallel.as_str()),
                        ms(serial.p50),
                        ms(serial.p95),
                        ms(serial_skew.p50),
                        ms(serial_skew.p95),
                        ms(late.p50),
                        ms(late.p95),
                        ms(late.max),
                        ms(took.p50),
                        ms(skew.p50),
                        ms(skew.p95),
                        frames - settle,
                        seams.join(" "),
                    );
                    if n == 1 {
                        whole = tails.into_iter().next();
                    }
                }
            }
        }
    }

    // ---- Layers by codec, and what switching them off leaves behind (review of P5) ----------

    /// Whether a sample says nothing later refers to it (`IsDependedOnByOthers` false).
    fn discardable(sample: &CMSampleBuffer) -> bool {
        sample_marks(sample).0 == Some(false)
    }

    /// The worker's three sessions, each fed its own source format: HEVC Main 4:2:0, HEVC Main
    /// 4:4:4 10-bit and H.264 High.
    fn codecs() -> Vec<(&'static str, u32, CFRetained<CFString>, u32)> {
        let (_, _, ll_profiles, _) = supported_for(1920, 1080, &low_latency_spec());
        // SAFETY: framework-provided constant strings.
        let (main, high) = unsafe {
            (
                kVTProfileLevel_HEVC_Main_AutoLevel,
                kVTProfileLevel_H264_High_AutoLevel,
            )
        };
        let mut out = vec![(
            "HEVC 4:2:0",
            kCMVideoCodecType_HEVC,
            CFRetained::from(main),
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        )];
        if let Some(full) = advertised(&ll_profiles, "HEVC_Main44410_AutoLevel") {
            out.push((
                "HEVC 4:4:4 10-bit",
                kCMVideoCodecType_HEVC,
                full,
                kCVPixelFormatType_444YpCbCr10BiPlanarFullRange,
            ));
        }
        out.push((
            "H.264 4:2:0",
            kCMVideoCodecType_H264,
            CFRetained::from(high),
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        ));
        out
    }

    /// Temporal layers on each of the worker's sessions, switched on and off on one live
    /// session: what each phase spends, drops and marks, and the luma PSNR at its end. The
    /// phases: as opened; `BaseLayerFrameRateFraction` 0.5 with `BaseLayerBitRateFraction` 0.8;
    /// the frame fraction back to 1.0 with the bit fraction left at 0.8 (how the first cut
    /// switched them off); both back to 1.0; and on again.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn temporal_layers_by_codec() {
        const PHASE: usize = 150;
        const SETTLE: usize = 30;
        // SAFETY: framework-provided constant strings.
        let (fraction_key, bits_key) = unsafe {
            (
                kVTCompressionPropertyKey_BaseLayerFrameRateFraction,
                kVTCompressionPropertyKey_BaseLayerBitRateFraction,
            )
        };
        let (w, h) = (1920, 1088);
        let pictures: Vec<Picture> = (0..12).map(|i| picture(w, h, i * 8)).collect();
        for (label, codec, profile, format) in codecs() {
            let images: Vec<_> = pictures.iter().map(|p| fill(p, format)).collect();
            for rate in [16_000_000_i64, 4_000_000] {
                let session = match Session::worker_as(
                    Spec::LowLatencyHardware,
                    codec,
                    (&profile, format),
                    (w, h),
                    rate,
                    60,
                ) {
                    Ok(session) => session,
                    Err(e) => {
                        eprintln!("MEASURE layers codec {label} {rate}: refused {e:?}");
                        continue;
                    }
                };
                let phases: [(&str, &[(&CFString, f64)]); 5] = [
                    ("as opened", &[]),
                    ("0.5 + 0.8", &[(bits_key, 0.8), (fraction_key, 0.5)]),
                    ("off, bits left 0.8", &[(fraction_key, 1.0)]),
                    ("off, bits 1.0", &[(bits_key, 1.0)]),
                    ("on again", &[(bits_key, 0.8), (fraction_key, 0.5)]),
                ];
                let label = format!("codec {label} {} Mbit/s", rate / 1_000_000);
                in_phases(
                    &session,
                    (&images, &pictures),
                    format,
                    &label,
                    &phases,
                    (PHASE, SETTLE),
                );
            }
        }
    }

    /// Feed `session` on a 60 beat through `phases`, each `len` frames with the properties it
    /// names set first; per phase, from its `settle`th frame: what came back, what was
    /// dropped and marked discardable, the mean base and layer-1 frame, the rate spent, and
    /// the luma PSNR of its last frames decoded from the stream's start. Every sample, in
    /// order, with its picture's index.
    fn in_phases(
        session: &Session,
        (images, pictures): (&[CFRetained<CVPixelBuffer>], &[Picture]),
        format: u32,
        label: &str,
        phases: &[(&str, &[(&CFString, f64)])],
        (len, settle): (usize, usize),
    ) -> Vec<(usize, Sample)> {
        let lengths: Vec<Phase<'_>> = phases
            .iter()
            .map(|&(phase, sets)| (phase, sets, len))
            .collect();
        in_phases_of(session, (images, pictures), format, label, &lengths, settle)
    }

    /// A phase of [`in_phases_of`]: its name, the properties set before it, and its frames.
    type Phase<'a> = (&'a str, &'a [(&'a CFString, f64)], usize);

    /// [`in_phases`] with a length of its own for each phase; `settle` is capped at half a
    /// phase.
    fn in_phases_of(
        session: &Session,
        (images, pictures): (&[CFRetained<CVPixelBuffer>], &[Picture]),
        format: u32,
        label: &str,
        phases: &[Phase<'_>],
        settle: usize,
    ) -> Vec<(usize, Sample)> {
        let mut all: Vec<(usize, Sample)> = Vec::new();
        let mut first = 0;
        for &(phase, sets, len) in phases {
            let settle = settle.min(len / 2);
            let statuses: Vec<i32> = sets
                .iter()
                .map(|(key, value)| set(&session.vt, key, &CFNumber::new_f64(*value)))
                .collect();
            let beat = on_beat_from(session, images, first, len, settle);
            first += len;
            let (mut base, mut upper) = ((0_usize, 0_usize), (0_usize, 0_usize));
            for (_, s) in beat.samples.iter().skip(settle) {
                let slot = if discardable(&s.0) {
                    &mut upper
                } else {
                    &mut base
                };
                slot.0 += 1;
                slot.1 += sample_bytes(&s.0);
            }
            let returned = beat.samples.len();
            let spent = (beat.bytes * 8) as f64 / ((len - settle) as f64 / 60.0) / 1e6;
            all.extend(beat.samples);
            let (sources, samples): (Vec<usize>, Vec<Sample>) =
                std::mem::take(&mut all).into_iter().unzip();
            let psnr_y = match decode_tail(&samples, format, true, 10) {
                Ok((_, tail)) if !tail.is_empty() => {
                    let first = sources.len() - tail.len();
                    tail.iter()
                        .zip(&sources[first..])
                        .map(|(p, &source)| psnr(&pictures[source], &p.0).0)
                        .sum::<f64>()
                        / tail.len() as f64
                }
                Ok(_) => f64::NAN,
                Err(e) => {
                    eprintln!("  decode failed {e:?}");
                    f64::NAN
                }
            };
            all = sources.into_iter().zip(samples).collect();
            let spread = Spread::of_durations(&beat.times).unwrap_or_default();
            eprintln!(
                "MEASURE layers {label} [{phase}] load={} set={statuses:?}: returned \
                 {returned}/{len} dropped {} base {} x {} B, layer-1 {} x {} B, \
                 spent={spent:.2}Mbit/s encode p50={:.2}ms psnr_y={psnr_y:.2}dB",
                load_average(),
                beat.dropped,
                base.0,
                base.1 / base.0.max(1),
                upper.0,
                upper.1 / upper.0.max(1),
                ms(spread.p50),
            );
        }
        all
    }

    /// What layers cost where they ship: switched on and off on a live session that has run a
    /// while, in long phases, against a session opened with them on. The first cut of this
    /// measurement compared fresh sessions only.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn temporal_layers_live_switch() {
        const PHASE: usize = 600;
        const SETTLE: usize = 60;
        // SAFETY: framework-provided constant strings.
        let (fraction_key, bits_key) = unsafe {
            (
                kVTCompressionPropertyKey_BaseLayerFrameRateFraction,
                kVTCompressionPropertyKey_BaseLayerBitRateFraction,
            )
        };
        let on: &[(&CFString, f64)] = &[(bits_key, 0.8), (fraction_key, 0.5)];
        let off: &[(&CFString, f64)] = &[(bits_key, 1.0), (fraction_key, 1.0)];
        let format = kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
        let sizes: Vec<(usize, usize)> = probe_sizes(&[(1920, 1088), (3024, 1968)]);
        for (w, h) in sizes {
            let rates = if w * h > 4_000_000 {
                [32_000_000_i64, 6_000_000]
            } else {
                [16_000_000, 3_000_000]
            };
            let pictures: Vec<Picture> = (0..12).map(|i| picture(w, h, i * 8)).collect();
            let images: Vec<_> = pictures.iter().map(|p| fill(p, format)).collect();
            for rate in rates {
                let label = format!("live {w}x{h} {} Mbit/s", rate / 1_000_000);
                let session = Session::worker(w, h, rate, 60).expect("session");
                let phases: [(&str, &[(&CFString, f64)]); 5] = [
                    ("warm", &[]),
                    ("on", on),
                    ("off", off),
                    ("on", on),
                    ("off", off),
                ];
                let all = in_phases(
                    &session,
                    (&images, &pictures),
                    format,
                    &label,
                    &phases,
                    (PHASE, SETTLE),
                );
                // The frames a live toggle marks can go: the rest decode to the same pictures.
                let samples: Vec<&Sample> = all.iter().map(|(_, s)| s).collect();
                let full = decode_hashes(&samples);
                let kept: Vec<usize> = (0..samples.len())
                    .filter(|&i| !discardable(&samples[i].0))
                    .collect();
                let got = decode_hashes(&kept.iter().map(|&i| samples[i]).collect::<Vec<_>>());
                let failed = got.iter().filter(|g| g.is_err()).count();
                let differ = kept
                    .iter()
                    .zip(&got)
                    .filter(|(i, g)| matches!((g, &full[**i]), (Ok(a), Ok(b)) if a != b))
                    .count();
                eprintln!(
                    "MEASURE layers {label} without the {} marked frames: {} decoded, {failed} \
                     failed, {differ} differ from the whole stream",
                    samples.len() - kept.len(),
                    got.len() - failed,
                );
                let fresh = Session::worker(w, h, rate, 60).expect("session");
                let label = format!("opened layered {w}x{h} {} Mbit/s", rate / 1_000_000);
                let phases: [(&str, &[(&CFString, f64)]); 2] =
                    [("on from the start", on), ("on", &[])];
                in_phases(
                    &fresh,
                    (&images, &pictures),
                    format,
                    &label,
                    &phases,
                    (PHASE, SETTLE),
                );
            }
        }
    }

    /// Whether the sample carries the encoder's error (`kVTSampleAttachmentKey_QualityMetrics`).
    fn has_quality_metrics(sample: &CMSampleBuffer) -> bool {
        // SAFETY: framework-provided constant string.
        let key = unsafe { kVTSampleAttachmentKey_QualityMetrics };
        sample_marks(sample)
            .4
            .iter()
            .any(|entry| entry.starts_with(&key.to_string()))
    }

    /// What `CalculateMeanSquaredError` costs the worker's session: encode time one frame at a
    /// time and on a 60 beat with it off and on, alternated twice, at 1080p and 3024 × 1968;
    /// and whether a live session takes it on and off between frames.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn mean_squared_error_report() {
        // SAFETY: framework-provided constant string.
        let key = unsafe { kVTCompressionPropertyKey_CalculateMeanSquaredError };
        for (w, h) in probe_sizes(&[(1920, 1088), (3024, 1968)]) {
            let (_, images) = scrolling(w, h);
            for round in 0..2 {
                for on in [false, true] {
                    let session = Session::worker(w, h, 16_000_000, 60).expect("session");
                    let status = if on { set(&session.vt, key, yes()) } else { 0 };
                    let serial = one_at_a_time(&session, &images, 60, 10);
                    let beat = on_beat_from(&session, &images, 1_000, 240, 60);
                    let spread = Spread::of_durations(&beat.times).unwrap_or_default();
                    let metrics = beat
                        .samples
                        .iter()
                        .filter(|(_, s)| has_quality_metrics(&s.0))
                        .count();
                    eprintln!(
                        "MEASURE mse {w}x{h} round {round} mse={on} status={status} load={}: \
                         one in flight p50={:.2}ms p95={:.2}ms | on beat p50={:.2}ms \
                         p95={:.2}ms | with metrics {metrics}/{}",
                        load_average(),
                        ms(serial.p50),
                        ms(serial.p95),
                        ms(spread.p50),
                        ms(spread.p95),
                        beat.samples.len(),
                    );
                }
            }
            let session = Session::worker(w, h, 16_000_000, 60).expect("session");
            let mut seen = Vec::new();
            for (step, on) in [(0, false), (1, true), (2, false), (3, true)] {
                let status = set(&session.vt, key, CFBoolean::new(on));
                let beat = on_beat_from(&session, &images, step * 60, 60, 5);
                let metrics = beat
                    .samples
                    .iter()
                    .filter(|(_, s)| has_quality_metrics(&s.0))
                    .count();
                seen.push(format!(
                    "{on}(status {status}): {metrics}/{}",
                    beat.samples.len()
                ));
            }
            eprintln!("MEASURE mse {w}x{h} toggled live: {}", seen.join(", "));
        }
    }

    /// How long a session must run before layers switched on behave as on a session that has
    /// run a while: on after 0 (before the keyframe), 1, 10, 60 and 300 frames, then 600 frames
    /// measured, at a rate with room and one that binds.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn temporal_layers_when_switched_on() {
        // SAFETY: framework-provided constant strings.
        let (fraction_key, bits_key) = unsafe {
            (
                kVTCompressionPropertyKey_BaseLayerFrameRateFraction,
                kVTCompressionPropertyKey_BaseLayerBitRateFraction,
            )
        };
        let on: &[(&CFString, f64)] = &[(bits_key, 0.8), (fraction_key, 0.5)];
        let format = kCVPixelFormatType_420YpCbCr8BiPlanarFullRange;
        let (w, h) = (1920, 1088);
        let pictures: Vec<Picture> = (0..12).map(|i| picture(w, h, i * 8)).collect();
        let images: Vec<_> = pictures.iter().map(|p| fill(p, format)).collect();
        for rate in [16_000_000_i64, 3_000_000] {
            for after in [0_usize, 1, 10, 60, 300] {
                let session = Session::worker(w, h, rate, 60).expect("session");
                let label = format!("on after {after} {w}x{h} {} Mbit/s", rate / 1_000_000);
                let mut phases: Vec<Phase<'_>> = Vec::new();
                if after > 0 {
                    phases.push(("before", &[], after));
                }
                phases.push(("on", on, 600));
                in_phases_of(&session, (&images, &pictures), format, &label, &phases, 60);
            }
        }
    }

    /// What a refinement does to the change right after it (review of P6): the text scrolls,
    /// stops, and scrolls on 1 ms after the stop's second refinement. The change's bytes and
    /// luma PSNR, and the next four frames', with no refinement before it (a 100 ms pause),
    /// with refinements stamped when they were sent (the first cut: the change comes 1 ms of
    /// stamps after one), and stamped as the worker now stamps them. 4:2:0 and 4:4:4 at 8
    /// Mbit/s, 1920 × 1088.
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn a_change_after_a_refinement() {
        use onicode_desktop_codec::{Chroma, Decoder, Encoder, EncoderConfig, FrameOptions};
        use onicode_desktop_media::wire::VideoCodec;

        const MOVING: usize = 30;
        const AFTER: usize = 5;
        let (w, h) = (1920, 1088);
        let pictures: Vec<Picture> = (0..MOVING + AFTER).map(|i| picture(w, h, i * 16)).collect();
        for chroma in [Chroma::Subsampled, Chroma::Full] {
            let format = onicode_desktop_codec::pixel_format(chroma);
            let images: Vec<_> = pictures.iter().map(|p| fill(p, format)).collect();
            let last = MOVING as u64 * 16_667;
            // (label, refinement stamps, the change's stamp)
            let cases: [(&str, Vec<u64>, u64); 3] = [
                ("no refinement", Vec::new(), last + 100_000),
                (
                    "stamped when sent",
                    vec![last + 33_334, last + 66_668],
                    last + 67_668,
                ),
                (
                    "stamped a period early",
                    vec![refinement_stamp(last, 1), refinement_stamp(last, 2)],
                    last + 67_668,
                ),
            ];
            for (label, refinements, change) in cases {
                let (tx, rx) = mpsc::channel();
                let encoder = Encoder::new(
                    EncoderConfig {
                        width: w as u32,
                        height: h as u32,
                        codec: VideoCodec::Hevc,
                        fps: 60,
                        bitrate_bps: 8_000_000,
                        chroma,
                    },
                    move |packet| {
                        let _gone = tx.send(packet);
                    },
                )
                .expect("the worker's session");
                // (picture, stamp) in the order they go in.
                let mut plan: Vec<(usize, u64)> =
                    (0..MOVING).map(|i| (i, (i as u64 + 1) * 16_667)).collect();
                plan.extend(refinements.iter().map(|&pts| (MOVING - 1, pts)));
                plan.extend((0..AFTER).map(|k| (MOVING + k, change + k as u64 * 16_667)));
                let mut packets = Vec::new();
                for (k, &(i, pts)) in plan.iter().enumerate() {
                    let options = FrameOptions {
                        force_keyframe: k == 0,
                        ..FrameOptions::default()
                    };
                    encoder.encode(&images[i], pts, &options).expect("encode");
                    encoder.flush().expect("flush");
                    packets.push((i, rx.try_recv().ok()));
                }
                let (dtx, drx) = mpsc::channel();
                let mut decoder = Decoder::new(VideoCodec::Hevc, move |frame| {
                    let _gone = dtx.send(frame);
                });
                let mut scored = Vec::new();
                for (i, packet) in &packets {
                    let Some(packet) = packet else {
                        scored.push((0, f64::NAN));
                        continue;
                    };
                    decoder
                        .decode(&packet.data.clone().into(), packet.pts_us)
                        .expect("decode");
                    let decoded = drx
                        .recv_timeout(Duration::from_secs(10))
                        .expect("a picture");
                    scored.push((
                        packet.data.len(),
                        psnr(&pictures[*i], decoded.image.as_cv()).0,
                    ));
                }
                let tail = &scored[scored.len() - AFTER..];
                let rest = &tail[1..];
                eprintln!(
                    "MEASURE change after refinement {chroma:?} 8 Mbit/s [{label}] load={}: the \
                     change {} B at {:.2} dB; the next {} frames {:.0} B at {:.2} dB",
                    load_average(),
                    tail[0].0,
                    tail[0].1,
                    rest.len(),
                    rest.iter().map(|s| s.0 as f64).sum::<f64>() / rest.len() as f64,
                    rest.iter().map(|s| s.1).sum::<f64>() / rest.len() as f64,
                );
            }
        }
    }

    /// One stream of [`concurrent_sessions`]: what its session gave back.
    #[derive(Default)]
    struct Concurrent {
        /// Submit → the sink, per frame after the settling ones.
        encode: Vec<Duration>,
        /// Time inside `VTCompressionSessionEncodeFrame`, the part the caller's thread pays.
        submit: Vec<Duration>,
        /// Due on the beat → the sink.
        late: Vec<Duration>,
        /// Pictures submitted and packets that came back, after the settling ones.
        submitted: usize,
        encoded: usize,
        /// Frames skipped because the focused stream had one in the encoder
        /// (`ONICODE_PROBE_YIELD`).
        yielded: usize,
        /// Frames the session dropped.
        dropped: usize,
        /// The luma PSNR the encoder measured on each frame after the settling ones.
        psnr: Vec<f64>,
        /// Beats whose capture a newer one replaced in the mailbox before the thread took it.
        superseded: usize,
        bytes: usize,
    }

    /// `n` of the worker's own sessions (`onicode_desktop_codec::Encoder`), each fed a scrolling
    /// text picture on its own thread at its own beat for `seconds`, the way the worker runs one
    /// encode thread per stream. Stream 0 is the focused one: at `fps`, the others at
    /// `background_fps`. `aligned` puts every stream's beat on the same instants, as windows on
    /// one display are captured on its refresh; otherwise the beats are spread evenly across a
    /// period. The rest is in [`Knobs`].
    fn concurrent_run(
        (w, h): (usize, usize),
        n: usize,
        seconds: f64,
        (fps, background_fps): (u32, u32),
        knobs: Knobs,
    ) -> (Vec<Concurrent>, f64) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        use onicode_desktop_codec::{Chroma, Encoder, EncoderConfig, FrameOptions};
        use onicode_desktop_media::wire::VideoCodec;

        const SETTLE: usize = 10;
        let Knobs {
            declared,
            aligned,
            yielding,
        } = knobs;
        // `ONICODE_PROBE_PLACE_OWN=1`: sessions made for one frame a second less than their beat,
        // below 60, place themselves at their own rate as every session did before 2026-09-30
        // (`onicode_desktop_codec` `placement_fps`); the declare knobs act at once only on those.
        let place_own = std::env::var("ONICODE_PROBE_PLACE_OWN").is_ok_and(|p| p == "1");
        let (_, images) = scrolling(w, h);
        let images = Arc::new(
            images
                .into_iter()
                .map(onicode_desktop_codec::PixelBuffer::from_retained)
                .collect::<Vec<_>>(),
        );
        // The focused stream's frames in the encoder: submitted, not yet back.
        let focus_in_flight = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(n + 1));
        let bitrate = std::env::var("ONICODE_PROBE_RATE")
            .ok()
            .and_then(|r| r.parse().ok())
            .unwrap_or(if w * h > 1920 * 1088 {
                40_000_000
            } else {
                16_000_000
            });
        let start_at = Arc::new(parking_lot::Mutex::new(None::<Instant>));
        // `ONICODE_PROBE_OPEN=sequential`: stream k opens its session only once stream k - 1 has,
        // as streams a client opens one after another do.
        let sequential = std::env::var("ONICODE_PROBE_OPEN").is_ok_and(|o| o == "sequential");
        let opened = Arc::new(AtomicUsize::new(0));
        #[expect(
            clippy::needless_collect,
            reason = "every stream's thread starts before any is joined"
        )]
        let threads: Vec<_> = (0..n)
            .map(|k| {
                let opened = Arc::clone(&opened);
                let images = Arc::clone(&images);
                let focus = Arc::clone(&focus_in_flight);
                let barrier = Arc::clone(&barrier);
                let start_at = Arc::clone(&start_at);
                std::thread::spawn(move || {
                    let rate = if k == 0 { fps } else { background_fps };
                    let period = Duration::from_secs(1) / rate;
                    let (tx, rx) = mpsc::channel::<(Instant, u64, usize, Option<f64>)>();
                    let focus_back = (k == 0).then(|| Arc::clone(&focus));
                    while sequential && opened.load(Ordering::Acquire) < k {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    let encoder = Encoder::new(
                        EncoderConfig {
                            width: w as u32,
                            height: h as u32,
                            codec: VideoCodec::Hevc,
                            fps: rate as u16 - u16::from(place_own),
                            bitrate_bps: bitrate,
                            chroma: Chroma::Subsampled,
                        },
                        move |packet| {
                            if let Some(focus) = &focus_back {
                                focus.fetch_sub(1, Ordering::AcqRel);
                            }
                            let _gone = tx.send((
                                Instant::now(),
                                packet.pts_us,
                                packet.data.len(),
                                packet.mse.map(|m| m.luma_psnr()),
                            ));
                        },
                    )
                    .expect("the worker's session");
                    let declare = if k == 0 { declared.0 } else { declared.1 };
                    // `ONICODE_PROBE_EXPECT_AT`: told on that beat instead, to see whether a
                    // running session moves; with `ONICODE_PROBE_EXPECT_THEN` as well, told
                    // `declare` at the start and that rate on the beat, to see whether it stays.
                    let then: Option<u16> = std::env::var("ONICODE_PROBE_EXPECT_THEN")
                        .ok()
                        .and_then(|t| t.parse().ok());
                    let declare_at: Option<usize> = std::env::var("ONICODE_PROBE_EXPECT_AT")
                        .ok()
                        .and_then(|a| a.parse().ok());
                    if let Some(declared) =
                        declare.filter(|_| declare_at.is_none() || then.is_some())
                    {
                        encoder.set_frame_rate(declared).expect("ExpectedFrameRate");
                    }
                    opened.fetch_add(1, Ordering::AcqRel);
                    // Twice: every session is open, then the first beat is set.
                    barrier.wait();
                    barrier.wait();
                    let start = start_at
                        .lock()
                        .expect("the start is set between the barriers");
                    let phase = if aligned {
                        Duration::ZERO
                    } else {
                        Duration::from_secs(1) / fps * k as u32 / n as u32
                    };
                    let frames = (seconds * f64::from(rate)) as usize;
                    let mut out = Concurrent::default();
                    // pts → (due, submitted)
                    let mut sent = std::collections::HashMap::new();
                    // The worker's one-frame mailbox: a capture that lands while the thread is
                    // inside a submit replaces the one waiting, so a thread that comes back late
                    // encodes the newest beat at once and the beats it missed are superseded.
                    let first = start + phase;
                    let mut last: Option<usize> = None;
                    let mut told = false;
                    loop {
                        let now = Instant::now();
                        let current = now
                            .checked_duration_since(first)
                            .map(|since| (since.as_nanos() / period.as_nanos()) as usize);
                        let i = match (current, last) {
                            (Some(c), Some(l)) if c > l => c,
                            (Some(c), None) => c,
                            _ => {
                                let next = last.map_or(0, |l| l + 1);
                                let due = first + period * next as u32;

                                std::thread::sleep(due.saturating_duration_since(now));
                                continue;
                            }
                        };
                        if i >= frames {
                            break;
                        }
                        if i >= SETTLE {
                            out.superseded += i - last.map_or(i, |l| l + 1).min(i);
                        }
                        last = Some(i);
                        let due = first + period * i as u32;
                        if let Some(declared) = then
                            .or(declare)
                            .filter(|_| declare_at.is_some_and(|at| i >= at))
                            && !told
                        {
                            encoder.set_frame_rate(declared).expect("ExpectedFrameRate");
                            told = true;
                        }
                        let busy = || k != 0 && i > 0 && focus.load(Ordering::Acquire) > 0;
                        match yielding {
                            Yield::Skip if busy() => {
                                if i >= SETTLE {
                                    out.yielded += 1;
                                }
                                continue;
                            }
                            Yield::No | Yield::Skip => {}
                            Yield::Wait => {
                                let gave_up = Instant::now() + period;
                                let mut waited = false;
                                while busy() && Instant::now() < gave_up {
                                    waited = true;

                                    std::thread::sleep(Duration::from_micros(100));
                                }
                                if waited && i >= SETTLE {
                                    out.yielded += 1;
                                }
                            }
                        }
                        let pts = (i as u64 + 1) * 1_000_000 / u64::from(rate);
                        let options = FrameOptions {
                            force_keyframe: i == 0,
                            ..FrameOptions::default()
                        };
                        if k == 0 {
                            focus.fetch_add(1, Ordering::AcqRel);
                        }
                        let submitted = Instant::now();
                        encoder
                            .encode(images[(i + 5 * k) % images.len()].as_cv(), pts, &options)
                            .expect("submit");
                        let took = submitted.elapsed();
                        if i >= SETTLE {
                            out.submit.push(took);
                            out.submitted += 1;
                            sent.insert(pts, (due, submitted));
                        }
                    }
                    encoder.flush().expect("flush");
                    out.dropped = usize::try_from(encoder.frames_dropped()).unwrap_or(usize::MAX);
                    // A frame the session dropped never reaches the sink; it leaves the count.
                    if k == 0 {
                        focus.store(0, Ordering::Release);
                    }
                    while let Ok((at, pts, bytes, psnr)) = rx.try_recv() {
                        let Some((due, submitted)) = sent.get(&pts) else {
                            continue;
                        };
                        out.encode.push(at.saturating_duration_since(*submitted));
                        out.late.push(at.saturating_duration_since(*due));
                        out.encoded += 1;
                        out.bytes += bytes;
                        out.psnr.extend(psnr.filter(|p| p.is_finite()));
                    }
                    out
                })
            })
            .collect();
        // Every session is open before the first beat, so none pays another's creation.
        barrier.wait();
        *start_at.lock() = Some(Instant::now() + Duration::from_millis(20));
        let before = onicode_desktop_testkit::process::own().map_or(0, |u| u.cycles);
        let began = Instant::now();
        barrier.wait();
        let results: Vec<Concurrent> = threads
            .into_iter()
            .map(|t| t.join().expect("a stream thread"))
            .collect();
        let cycles = onicode_desktop_testkit::process::own().map_or(0, |u| u.cycles) - before;
        // Gigacycles a second of this process, every thread (VideoToolbox's included).
        let load = cycles as f64 / 1e9 / began.elapsed().as_secs_f64();
        (results, load)
    }

    fn spread_ms(samples: &[Duration]) -> String {
        Spread::of_durations(samples).map_or_else(
            || "-".to_owned(),
            |s| format!("{:.2}/{:.2}/{:.2}", ms(s.p50), ms(s.p95), ms(s.max)),
        )
    }

    /// How [`concurrent_run`] runs its sessions.
    #[derive(Clone, Copy, Debug)]
    struct Knobs {
        /// The `ExpectedFrameRate` the focused and the other sessions are told, when not their own
        /// rate.
        declared: (Option<u16>, Option<u16>),
        /// Every stream's beat on the same instants.
        aligned: bool,
        /// What a background stream does while the focused one has a frame in the encoder.
        yielding: Yield,
    }

    /// What a background stream of [`concurrent_run`] does while the focused one has a frame in
    /// the encoder.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Yield {
        /// Nothing: every stream submits on its beat.
        No,
        /// Skips that beat.
        Skip,
        /// Waits for the focused frame to come back, a period at most, then submits.
        Wait,
    }

    /// Several streams sharing the media engines (idea #14; MEASUREMENTS "several streams on
    /// the encode engines"): 1, 2, 4 and 8 of the worker's sessions at once, each on its own
    /// thread and beat, at 1080p and at 4K. For the focused stream (stream 0) and for all of
    /// them together: submit → packet p50/p95/max, the time inside the submit call, due →
    /// packet, frames encoded a second against those asked for, and the process's gigacycles a
    /// second (VideoToolbox's threads in this process included).
    ///
    /// Knobs: `ONICODE_PROBE_SIZES` (`1920x1088,3840x2160`), `ONICODE_PROBE_STREAMS` (`1,2,4,8`),
    /// `ONICODE_PROBE_SECONDS` (4), `ONICODE_PROBE_PHASE` (`aligned` or `spread`, default
    /// aligned), `ONICODE_PROBE_BACKGROUND_FPS` (the other streams' rate, default 60),
    /// `ONICODE_PROBE_PLACE_OWN=1` (each session placed at its own rate, as before 2026-09-30),
    /// `ONICODE_PROBE_EXPECT` (the `ExpectedFrameRate` every session is told, at once only with
    /// `PLACE_OWN`), `ONICODE_PROBE_FOCUS_EXPECT` (the focused one's, default the same),
    /// `ONICODE_PROBE_EXPECT_AT` and `_THEN` (when it is told, and what after),
    /// `ONICODE_PROBE_OPEN` (`sequential`), `ONICODE_PROBE_RATE`, `ONICODE_PROBE_FPS` and
    /// `ONICODE_PROBE_YIELD` (`skip` or `wait`: what a background stream does while the focused one
    /// has a frame in the encoder).
    #[test]
    #[ignore = "a measurement; copy the binary off the Lacie volume and run with --ignored --nocapture"]
    fn concurrent_sessions() {
        let knob = |name: &str| std::env::var(name).ok();
        let rate = |name: &str| knob(name).and_then(|s| s.parse::<u16>().ok());
        let sizes = probe_sizes(&[(1920, 1088), (3840, 2160)]);
        let counts: Vec<usize> = knob("ONICODE_PROBE_STREAMS").map_or_else(
            || vec![1, 2, 4, 8],
            |s| s.split(',').filter_map(|n| n.trim().parse().ok()).collect(),
        );
        let seconds: f64 = knob("ONICODE_PROBE_SECONDS")
            .and_then(|s| s.parse().ok())
            .unwrap_or(4.0);
        let aligned = knob("ONICODE_PROBE_PHASE").is_none_or(|p| p != "spread");
        let focus_fps = u32::from(rate("ONICODE_PROBE_FPS").unwrap_or(60));
        let background_fps = u32::from(rate("ONICODE_PROBE_BACKGROUND_FPS").unwrap_or(60));
        let declared = (
            rate("ONICODE_PROBE_FOCUS_EXPECT").or_else(|| rate("ONICODE_PROBE_EXPECT")),
            rate("ONICODE_PROBE_EXPECT"),
        );
        let yielding = match knob("ONICODE_PROBE_YIELD").as_deref() {
            Some("skip") => Yield::Skip,
            Some("wait") => Yield::Wait,
            _ => Yield::No,
        };
        for &(w, h) in &sizes {
            for &n in &counts {
                let knobs = Knobs {
                    declared,
                    aligned,
                    yielding,
                };
                let (streams, load) =
                    concurrent_run((w, h), n, seconds, (focus_fps, background_fps), knobs);
                let all = |f: fn(&Concurrent) -> &Vec<Duration>| -> Vec<Duration> {
                    streams.iter().flat_map(|s| f(s).iter().copied()).collect()
                };
                // The beats after the settling ones, at each stream's rate.
                let measured = |fps: u32| seconds - 10.0 / f64::from(fps);
                let focus = &streams[0];
                let rest = &streams[1..];
                let rest_fps = rest.iter().map(|s| s.encoded).sum::<usize>() as f64
                    / rest.len().max(1) as f64
                    / measured(background_fps);
                let mbps = focus.bytes as f64 * 8.0 / measured(focus_fps) / 1e6
                    + rest.iter().map(|s| s.bytes).sum::<usize>() as f64 * 8.0
                        / measured(background_fps)
                        / 1e6;
                eprintln!(
                    "MEASURE concurrent {w}x{h} n={n} {} declared {:?}/{:?}{} others at {background_fps} load={}: \
                     focused at {focus_fps}: encode {} ms, late {} ms, {:.1} fps, {:.2} Mbit/s, {:.2} dB | all encode {} ms, submit {} \
                     ms, late {} ms | others {:.1} fps each | superseded {}, yielded {}, dropped \
                     {} | {mbps:.1} Mbit/s | process {load:.2} Gcycles/s",
                    if aligned { "aligned" } else { "spread" },
                    declared.0,
                    declared.1,
                    if yielding == Yield::No {
                        String::new()
                    } else {
                        format!(" {yielding:?}")
                    },
                    load_average(),
                    spread_ms(&focus.encode),
                    spread_ms(&focus.late),
                    focus.encoded as f64 / measured(focus_fps),
                    focus.bytes as f64 * 8.0 / measured(focus_fps) / 1e6,
                    focus.psnr.iter().sum::<f64>() / focus.psnr.len().max(1) as f64,
                    spread_ms(&all(|s| &s.encode)),
                    spread_ms(&all(|s| &s.submit)),
                    spread_ms(&all(|s| &s.late)),
                    rest_fps,
                    streams.iter().map(|s| s.superseded).sum::<usize>(),
                    rest.iter().map(|s| s.yielded).sum::<usize>(),
                    streams.iter().map(|s| s.dropped).sum::<usize>(),
                );
            }
        }
    }
}
