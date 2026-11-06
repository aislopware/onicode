//! Hardware encode → decode round trip on the machine's VideoToolbox (macOS only).

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests {
    #![expect(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        reason = "test fixture arithmetic on small, bounded values"
    )]

    use std::ptr::{self, NonNull};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
        CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
        CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    };
    use onicode_desktop_codec::nal::{self, hevc};
    use onicode_desktop_codec::{Decoder, Encoder, EncoderConfig, FrameOptions};
    use onicode_desktop_media::wire::VideoCodec;

    const W: usize = 640;
    const H: usize = 360;

    /// An NV12 frame with a moving gradient so consecutive frames differ.
    fn frame(index: usize) -> CFRetained<CVPixelBuffer> {
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: the out-pointer is valid; no attributes dictionary is passed.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                W,
                H,
                kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, 0, "CVPixelBufferCreate");
        // SAFETY: `CVPixelBufferCreate` returned a +1 reference.
        let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };
        // SAFETY: the buffer is valid and not locked yet.
        let locked =
            unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(locked, 0);
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            let rows = if plane == 0 { H } else { H / 2 };
            for y in 0..rows {
                for x in 0..W {
                    let v = if plane == 0 {
                        ((x + y + index * 7) % 220 + 16) as u8
                    } else {
                        128
                    };
                    // SAFETY: the plane is locked, `y < rows` and `x < W <= stride`, so the
                    // offset is inside the plane's mapped bytes.
                    let cell = unsafe { base.add(y * stride + x) };
                    // SAFETY: `cell` points at one writable byte of the locked plane.
                    unsafe { cell.write(v) }
                }
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0);
        buffer
    }

    #[test]
    fn hevc_encode_then_decode() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (ptx, prx) = mpsc::channel();
        let encoder = Encoder::new(
            EncoderConfig {
                width: W as u32,
                height: H as u32,
                codec: VideoCodec::Hevc,
                fps: 60,
                bitrate_bps: 4_000_000,
                chroma: onicode_desktop_codec::Chroma::Subsampled,
            },
            move |packet| {
                let _sent = ptx.send((Instant::now(), packet));
            },
        )
        .expect("hardware HEVC encoder");
        let frames = 30;
        let mut submitted = Vec::with_capacity(frames);
        for i in 0..frames {
            let opts = FrameOptions {
                force_keyframe: i == 0,
                ..FrameOptions::default()
            };
            submitted.push(Instant::now());
            encoder
                .encode(&frame(i), (i as u64) * 16_667, &opts)
                .expect("encode");
        }
        encoder.flush().expect("flush");
        let mut packets = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while packets.len() < frames {
            let left = deadline.saturating_duration_since(Instant::now());
            match prx.recv_timeout(left) {
                Ok(p) => packets.push(p),
                Err(_) => break,
            }
        }
        assert!(packets.len() >= frames - 1, "got {} packets", packets.len());
        let first = &packets[0].1;
        assert!(first.keyframe, "first packet is an IDR");
        let types: Vec<u8> = nal::units(&first.data).filter_map(hevc::nal_type).collect();
        assert!(
            packets.iter().all(|(_, p)| nal::check(&p.data).is_ok()),
            "every packet is length-prefixed units, end to end"
        );
        assert!(
            types.contains(&hevc::VPS) && types.contains(&hevc::SPS),
            "{types:?}"
        );
        assert!(types.contains(&hevc::PPS), "{types:?}");
        assert!(
            types
                .iter()
                .any(|t| matches!(*t, hevc::IDR_W_RADL | hevc::IDR_N_LP)),
            "{types:?}"
        );
        assert!(
            packets[1..].iter().all(|(_, p)| !p.keyframe),
            "no unrequested keyframes"
        );
        assert!(
            packets.windows(2).all(|w| w[0].1.pts_us < w[1].1.pts_us),
            "pts monotonic"
        );
        if encoder.ltr_enabled() {
            assert!(
                packets.iter().any(|(_, p)| p.ltr_token.is_some()),
                "LTR tokens flow"
            );
        }
        let latencies: Vec<Duration> = packets
            .iter()
            .enumerate()
            .map(|(i, (at, _))| at.saturating_duration_since(submitted[i.min(frames - 1)]))
            .collect();
        let max = latencies.iter().max().copied().unwrap_or_default();
        eprintln!(
            "encode latency max {max:?}, first packet {} bytes",
            first.data.len()
        );

        let (dtx, drx) = mpsc::channel();
        let mut decoder = Decoder::new(VideoCodec::Hevc, move |f| {
            let _sent = dtx.send((f.image.width(), f.image.height(), f.pts_us));
        });
        for (_, p) in &packets {
            decoder
                .decode(&p.data.clone().into(), p.pts_us)
                .expect("decode");
        }
        let mut decoded = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while decoded.len() < packets.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            match drx.recv_timeout(left) {
                Ok(f) => decoded.push(f),
                Err(_) => break,
            }
        }
        assert!(
            decoded.len() + 2 >= packets.len(),
            "decoded {} of {}",
            decoded.len(),
            packets.len()
        );
        assert!(
            decoded.iter().all(|&(w, h, _)| (w, h) == (W, H)),
            "{decoded:?}"
        );
        assert!(decoder.ready());
    }

    /// What a forced LTR refresh actually costs once its reference is acknowledged.
    ///
    /// The worker's congestion guard asks for a refresh on every frame it drops, so on a collapsed
    /// link that is most of them (`docs/decisions/transport.md`). Whether that is cheap or ruinous
    /// turns on one thing: if VideoToolbox answers with a delta off an acknowledged long-term
    /// reference it is a small frame, and if it falls back to an IDR the guard is demanding a full
    /// keyframe from a link that cannot carry one.
    #[test]
    fn a_forced_ltr_refresh_is_a_delta_not_an_idr() {
        if !onicode_desktop_testkit::live::opted_in(onicode_desktop_testkit::live::VT_TESTS) {
            return;
        }
        let (ptx, prx) = mpsc::channel();
        let encoder = Encoder::new(
            EncoderConfig {
                width: W as u32,
                height: H as u32,
                codec: VideoCodec::Hevc,
                fps: 60,
                bitrate_bps: 4_000_000,
                chroma: onicode_desktop_codec::Chroma::Subsampled,
            },
            move |packet| {
                let _sent = ptx.send(packet);
            },
        )
        .expect("hardware HEVC encoder");
        if !encoder.ltr_enabled() {
            onicode_desktop_testkit::live::skip("no LTR on this encoder; nothing to measure");
            return;
        }

        // A run of ordinary frames, so the encoder has long-term references to offer.
        let lead = 15;
        for i in 0..lead {
            let opts = FrameOptions {
                force_keyframe: i == 0,
                ..FrameOptions::default()
            };
            encoder
                .encode(&frame(i), (i as u64) * 16_667, &opts)
                .expect("encode");
        }
        let mut packets = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while packets.len() < lead {
            let left = deadline.saturating_duration_since(Instant::now());
            match prx.recv_timeout(left) {
                Ok(p) => packets.push(p),
                Err(_) => break,
            }
        }
        assert!(packets.len() >= lead - 1, "got {} packets", packets.len());
        let idr = packets[0].data.len();
        let tokens: Vec<u64> = packets.iter().filter_map(|p| p.ltr_token).collect();
        assert!(
            !tokens.is_empty(),
            "the encoder offered no LTR tokens to acknowledge"
        );

        // Acknowledge every token the way the client does, then ask for the refresh.
        let opts = FrameOptions {
            force_keyframe: false,
            force_ltr_refresh: true,
            acked_ltr: tokens.clone(),
        };
        encoder
            .encode(&frame(lead), (lead as u64) * 16_667, &opts)
            .expect("encode");
        encoder.flush().expect("flush");
        let refresh = prx
            .recv_timeout(Duration::from_secs(10))
            .expect("the refresh frame came back from the encoder");
        eprintln!(
            "{} LTR tokens acknowledged; IDR {idr} B, refresh {} B, keyframe {}, ltr_refresh {}",
            tokens.len(),
            refresh.data.len(),
            refresh.keyframe,
            refresh.ltr_refresh
        );
        assert!(
            !refresh.keyframe,
            "a refresh off {} acknowledged LTR tokens still came back as an IDR of {} B \
             (the plain IDR was {idr} B): the worker's guard is asking a collapsed link for a full \
             keyframe on every dropped frame",
            tokens.len(),
            refresh.data.len()
        );
        assert!(
            refresh.data.len() < idr,
            "a refresh that is not an IDR should still be cheaper than one: {} B vs {idr} B",
            refresh.data.len()
        );
    }

    #[test]
    fn decoder_rejects_p_frames_before_parameter_sets() {
        let mut decoder = Decoder::new(VideoCodec::Hevc, |_f| {});
        let err = decoder
            .decode(&vec![0, 0, 0, 3, 0x02, 0x01, 0xaa].into(), 0)
            .unwrap_err();
        assert!(
            matches!(err, onicode_desktop_codec::CodecError::NoParameterSets),
            "{err}"
        );
    }
}
