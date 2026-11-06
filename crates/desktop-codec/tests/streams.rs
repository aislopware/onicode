//! What N streams from one worker cost, per stream as they are today and shared: the worker
//! encoding them all (one session per stream, two stripe sessions per stream, or one session for
//! all of them side by side), a client decoding them all (one `Decoder` per tile, fed on one
//! display's refresh), and each stream's own sound (an Opus encoder on the worker, a decoder on
//! the client). MEASUREMENTS, "streams from one worker: what they could share" and "large streams
//! on the encode engines".

#![cfg(target_os = "macos")]

#[cfg(test)]
mod tests {
    #![expect(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::cast_precision_loss,
        clippy::cast_sign_loss,
        reason = "a measurement's fixture arithmetic on small, bounded values"
    )]

    use std::mem::MaybeUninit;
    use std::ptr::{self, NonNull};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use objc2_core_foundation::{CFDictionary, CFRetained, CFString, CFType};
    use objc2_core_video::{
        CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
        CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
        CVPixelBufferUnlockBaseAddress, kCVPixelBufferIOSurfacePropertiesKey,
    };
    use onicode_desktop_codec::{Decoder, Encoder, EncoderConfig, FrameOptions};
    use onicode_desktop_media::wire::{Chroma, VideoCodec};

    /// Frames in the loop every stream plays: two seconds at 60.
    const LOOP: usize = 120;
    /// Frames of each run not counted: sessions configuring, caches filling.
    const WARM: usize = 30;

    /// `struct rusage_info_v6` (`<sys/resource.h>`): a UUID and 56 counters.
    #[repr(C)]
    struct RusageV6 {
        uuid: [u8; 16],
        counters: [u64; 56],
    }

    /// `ri_phys_footprint`, bytes.
    const PHYS_FOOTPRINT: usize = 7;
    /// `ri_energy_nj`: the energy the process's threads spent on the CPU, nanojoules.
    const ENERGY_NJ: usize = 40;
    /// `RUSAGE_INFO_V6` (`<sys/resource.h>`).
    const RUSAGE_INFO_V6: i32 = 6;

    unsafe extern "C" {
        /// `<libproc.h>`: `int proc_pid_rusage(int pid, int flavor, rusage_info_t *buffer)`.
        fn proc_pid_rusage(pid: i32, flavor: i32, buffer: *mut RusageV6) -> i32;
    }

    /// The process's CPU time so far, its CPU energy and its footprint.
    #[derive(Clone, Copy, Debug)]
    struct Usage {
        cpu: Duration,
        energy_nj: u64,
        footprint: u64,
    }

    fn usage() -> Usage {
        let mut ru = MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `getrusage(2)` writes one `struct rusage` through the valid pointer.
        let got = unsafe { libc::getrusage(libc::RUSAGE_SELF, ru.as_mut_ptr()) };
        assert_eq!(got, 0, "getrusage");
        // SAFETY: written in full by the successful call above.
        let ru = unsafe { ru.assume_init() };
        let time = |t: libc::timeval| {
            Duration::from_secs(t.tv_sec as u64) + Duration::from_micros(t.tv_usec as u64)
        };
        let pid = i32::try_from(std::process::id()).expect("a pid");
        let mut info = MaybeUninit::<RusageV6>::zeroed();
        // SAFETY: `proc_pid_rusage` (libproc.h) writes one `rusage_info_v6` for the flavor
        // `RUSAGE_INFO_V6`, whose layout `RusageV6` mirrors; the pointer is valid for it.
        let got = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V6, info.as_mut_ptr()) };
        assert_eq!(got, 0, "proc_pid_rusage");
        // SAFETY: written in full by the successful call above.
        let info = unsafe { info.assume_init() };
        Usage {
            cpu: time(ru.ru_utime) + time(ru.ru_stime),
            energy_nj: info.counters[ENERGY_NJ],
            footprint: info.counters[PHYS_FOOTPRINT],
        }
    }

    /// A page of text scrolled up by `scroll` rows in the format a `chroma` session is fed
    /// (full-range NV12, or full-range 10-bit 4:4:4): dark glyph-like marks on a light ground,
    /// 8 × 16 cells, so each frame differs from the last as a terminal's does. `IOSurface`-backed,
    /// as ScreenCaptureKit's captures are: VideoToolbox codes an aligned one inside the submit,
    /// and copies any other buffer and codes the copy off it, behind a queue.
    fn page(w: usize, h: usize, scroll: usize, chroma: Chroma) -> CFRetained<CVPixelBuffer> {
        // SAFETY: framework-provided constant string.
        let key = unsafe { kCVPixelBufferIOSurfacePropertiesKey };
        let none = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let attributes = CFDictionary::<CFString, CFType>::from_slices(&[key], &[&*none]);
        let mut raw: *mut CVPixelBuffer = ptr::null_mut();
        // SAFETY: CoreVideo's rule for `CVPixelBufferCreate`: a valid out-pointer, and an
        // attributes dictionary of `kCVPixelBuffer*` keys.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                w,
                h,
                onicode_desktop_codec::pixel_format(chroma),
                Some(attributes.as_opaque()),
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
        // Bytes a sample, and samples a row, of each plane.
        let (sample, chroma_rows, chroma_row) = match chroma {
            Chroma::Subsampled => (1, h / 2, w),
            Chroma::Full => (2, h, w * 2),
        };
        for plane in 0..2 {
            let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane).cast::<u8>();
            let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
            let (rows, samples) = if plane == 0 {
                (h, w)
            } else {
                (chroma_rows, chroma_row)
            };
            for y in 0..rows {
                // SAFETY: the plane is locked and `y < rows`, so the row's start is inside the
                // plane's mapped bytes.
                let start = unsafe { base.add(y * stride) };
                // SAFETY: the row's `samples * sample <= stride` bytes from `start` are mapped
                // and writable, and nothing else touches them while the plane is locked here.
                let row = unsafe { std::slice::from_raw_parts_mut(start, samples * sample) };
                let line = (y + scroll) / 16;
                let within = (y + scroll) % 16;
                for (x, px) in row.chunks_exact_mut(sample).enumerate() {
                    let value: u16 = if plane == 1 {
                        128
                    } else {
                        let cell = x / 8;
                        let glyph = (line * 131 + cell * 71) % 97;
                        let ink = glyph > 20
                            && (2..13).contains(&within)
                            && ((glyph >> (x % 8)) ^ (within * glyph)).is_multiple_of(3);
                        if ink { 40 } else { 235 }
                    };
                    match px {
                        [one] => *one = value as u8,
                        // 10 bits in the high bits of a little-endian 16-bit sample.
                        two => two.copy_from_slice(&((value << 2) << 6).to_le_bytes()),
                    }
                }
            }
        }
        // SAFETY: matches the lock above.
        let unlocked =
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::empty()) };
        assert_eq!(unlocked, 0);
        buffer
    }

    /// The loop every stream plays, encoded once by the worker's own encoder: a keyframe, then
    /// [`LOOP`] − 1 frames of the page scrolling 4 rows a frame.
    fn encoded_loop(w: usize, h: usize, bitrate_bps: u32) -> Vec<Bytes> {
        let (tx, rx) = mpsc::channel();
        let encoder = Encoder::new(
            EncoderConfig {
                width: w as u32,
                height: h as u32,
                codec: VideoCodec::Hevc,
                fps: 60,
                bitrate_bps,
                chroma: Chroma::Subsampled,
            },
            move |packet| {
                let _sent = tx.send(packet);
            },
        )
        .expect("an encoder");
        (0..LOOP)
            .map(|n| {
                let options = FrameOptions {
                    force_keyframe: n == 0,
                    ..FrameOptions::default()
                };
                encoder
                    .encode(
                        &page(w, h, n * 4, Chroma::Subsampled),
                        (n as u64) * 16_667,
                        &options,
                    )
                    .expect("encode");
                let packet = rx.recv_timeout(Duration::from_secs(5)).expect("a packet");
                assert_eq!(packet.keyframe, n == 0, "one keyframe, in front");
                Bytes::from(packet.data)
            })
            .collect()
    }

    fn quantiles(mut v: Vec<f64>) -> (f64, f64, f64) {
        if v.is_empty() {
            return (f64::NAN, f64::NAN, f64::NAN);
        }
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        (at(0.5), at(0.95), v[v.len() - 1])
    }

    fn sleep_until(deadline: Instant) {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }

    fn knob<T: std::str::FromStr>(name: &str, default: &str) -> Vec<T> {
        std::env::var(name)
            .unwrap_or_else(|_| default.to_owned())
            .split(',')
            .filter_map(|v| v.trim().parse().ok())
            .collect()
    }

    /// Several streams decoded at once, as a client decodes the tiles it shows from one worker:
    /// 1, 2, 4 and 8 `Decoder`s, each on its own thread, each submitting the same encoded loop
    /// on one 60 Hz beat. For stream 0 and for all streams: submit → picture p50 / p95 / max,
    /// pictures a second, and for the process the CPU cores, CPU power (`ri_energy_nj`) and
    /// footprint over the measured seconds.
    ///
    /// Knobs: `ONICODE_DECODE_SIZES` (`1920x1088,3840x2160`), `ONICODE_DECODE_STREAMS`
    /// (`1,2,4,8`), `ONICODE_DECODE_SECONDS` (4).
    #[test]
    #[ignore = "a measurement: cargo test -p onicode-desktop-codec --release --test streams -- --ignored --nocapture"]
    fn concurrent_decode() {
        let sizes: Vec<String> = knob("ONICODE_DECODE_SIZES", "1920x1088,3840x2160");
        let counts: Vec<usize> = knob("ONICODE_DECODE_STREAMS", "1,2,4,8");
        let seconds: Vec<u64> = knob("ONICODE_DECODE_SECONDS", "4");
        let seconds = seconds.first().copied().unwrap_or(4);
        let period = Duration::from_nanos(1_000_000_000 / 60);
        let frames = WARM + (seconds as usize) * 60;
        onicode_desktop_codec::warm_up().expect("the decoder warms up");
        for size in sizes {
            let (w, h) = size.split_once('x').expect("WxH");
            let (w, h): (usize, usize) = (w.parse().expect("width"), h.parse().expect("height"));
            let bitrate = if w * h > 1920 * 1088 {
                40_000_000
            } else {
                16_000_000
            };
            let packets = encoded_loop(w, h, bitrate);
            let bytes: usize = packets.iter().map(Bytes::len).sum();
            println!(
                "MEASURE concurrent decode size={w}x{h} loop={LOOP} frames, {:.2} Mbit/s at 60",
                (bytes * 8) as f64 / 2.0 / 1e6
            );
            for &n in &counts {
                let before_open = usage();
                let epoch = Instant::now() + Duration::from_millis(300);
                let (done_tx, done_rx) = mpsc::channel();
                let threads: Vec<_> = (0..n)
                    .map(|stream| {
                        let packets = packets.clone();
                        let done_tx = done_tx.clone();
                        std::thread::Builder::new()
                            .name(format!("decode-{stream}"))
                            .spawn(move || {
                                let (tx, rx) = mpsc::channel::<(u64, Instant)>();
                                let mut decoder = Decoder::new(VideoCodec::Hevc, move |frame| {
                                    let _sent = tx.send((frame.pts_us, Instant::now()));
                                });
                                let mut submitted = Vec::with_capacity(frames);
                                let mut inside = Vec::with_capacity(frames);
                                for f in 0..frames {
                                    sleep_until(epoch + period * f as u32);
                                    let at = Instant::now();
                                    decoder
                                        .decode(&packets[f % LOOP], f as u64)
                                        .expect("decode");
                                    inside.push(at.elapsed());
                                    submitted.push(at);
                                }
                                // Every picture back, or a second after the last submit.
                                let mut back = vec![None; frames];
                                let deadline = Instant::now() + Duration::from_secs(1);
                                while back.iter().any(Option::is_none) {
                                    let left = deadline.saturating_duration_since(Instant::now());
                                    let Ok((pts, at)) = rx.recv_timeout(left) else {
                                        break;
                                    };
                                    back[pts as usize] = Some(at);
                                }
                                let latency: Vec<f64> = (WARM..frames)
                                    .filter_map(|f| {
                                        Some((back[f]? - submitted[f]).as_secs_f64() * 1e3)
                                    })
                                    .collect();
                                let inside: Vec<f64> = inside[WARM..]
                                    .iter()
                                    .map(|d| d.as_secs_f64() * 1e3)
                                    .collect();
                                let _sent = done_tx.send((stream, latency, inside));
                                drop(decoder);
                            })
                            .expect("a thread")
                    })
                    .collect();
                drop(done_tx);
                sleep_until(epoch + period * WARM as u32);
                let start = usage();
                let measured_from = Instant::now();
                sleep_until(epoch + period * frames as u32);
                let end = usage();
                let wall = measured_from.elapsed();
                let mut results: Vec<_> = done_rx.iter().collect();
                for thread in threads {
                    thread.join().expect("the stream's thread");
                }
                results.sort_by_key(|(stream, ..)| *stream);
                let counted = (frames - WARM) as f64;
                let focused = &results[0];
                let (f50, f95, fmax) = quantiles(focused.1.clone());
                let all: Vec<f64> = results.iter().flat_map(|r| r.1.iter().copied()).collect();
                let (a50, a95, amax) = quantiles(all);
                let inside: Vec<f64> = results.iter().flat_map(|r| r.2.iter().copied()).collect();
                let (i50, i95, _) = quantiles(inside);
                let pictures: Vec<String> = results
                    .iter()
                    .map(|r| format!("{:.1}", r.1.len() as f64 / counted * 60.0))
                    .collect();
                let cores = end.cpu.saturating_sub(start.cpu).as_secs_f64() / wall.as_secs_f64();
                let watts = (end.energy_nj - start.energy_nj) as f64 / 1e9 / wall.as_secs_f64();
                let footprint_mb = (end.footprint as f64 - before_open.footprint as f64) / 1e6;
                println!(
                    "MEASURE concurrent decode size={w}x{h} streams={n} \
                     stream0 p50/p95/max={f50:.2}/{f95:.2}/{fmax:.2} ms \
                     all={a50:.2}/{a95:.2}/{amax:.2} ms inside_submit p50/p95={i50:.2}/{i95:.2} ms \
                     pictures_per_s=[{}] cpu_cores={cores:.3} cpu_watts={watts:.3} \
                     footprint_growth_mb={footprint_mb:.1}",
                    pictures.join(",")
                );
            }
        }
    }
    /// How [`concurrent_encode`] codes its streams.
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Mode {
        /// One session per stream, as the worker does today.
        Sessions,
        /// Two sessions per stream, each coding a horizontal stripe with 64 rows past the seam
        /// (`docs/decisions/video.md`, "Two stripes halve the encode at 3K and above").
        Stripes,
        /// One session for every stream, their pictures side by side in one grid. The grid is
        /// drawn ahead of time, so the copy a real one would need is not counted.
        Composite,
    }

    impl std::str::FromStr for Mode {
        type Err = ();

        fn from_str(s: &str) -> Result<Self, ()> {
            match s {
                "sessions" => Ok(Self::Sessions),
                "stripes" => Ok(Self::Stripes),
                "composite" => Ok(Self::Composite),
                _ => Err(()),
            }
        }
    }

    /// Distinct pictures each session cycles through: enough that no two beats in a row repeat,
    /// few enough that grids of four 5K pictures fit in memory.
    const PICTURES: usize = 12;

    /// One session of [`concurrent_encode`]: which stream it codes for, its size, the page row
    /// its picture starts at, and its bitrate.
    #[derive(Clone, Copy, Debug)]
    struct Part {
        stream: usize,
        size: (usize, usize),
        top: usize,
        bitrate: u32,
    }

    /// The sessions `n` streams of `w` × `h` are coded on in `mode`.
    fn parts(mode: Mode, (w, h): (usize, usize), n: usize, bitrate: u32) -> Vec<Part> {
        match mode {
            Mode::Sessions => (0..n)
                .map(|stream| Part {
                    stream,
                    size: (w, h),
                    top: 0,
                    bitrate,
                })
                .collect(),
            // The seam is the multiple of 64 nearest half; each stripe codes 64 rows past it and
            // gets the bitrate for the rows it shows.
            Mode::Stripes => {
                let seam = (h / 2 + 31) / 64 * 64;
                let share = |rows: usize| (u64::from(bitrate) * rows as u64 / h as u64) as u32;
                (0..n)
                    .flat_map(|stream| {
                        [
                            Part {
                                stream,
                                size: (w, seam + 64),
                                top: 0,
                                bitrate: share(seam),
                            },
                            Part {
                                stream,
                                size: (w, h - (seam - 64)),
                                top: seam - 64,
                                bitrate: share(h - seam),
                            },
                        ]
                    })
                    .collect()
            }
            Mode::Composite => {
                let cols = (1..=n).find(|c| c * c >= n).unwrap_or(1);
                let size = (w * cols, h * n.div_ceil(cols));
                vec![Part {
                    stream: 0,
                    size,
                    top: 0,
                    bitrate: bitrate * n as u32,
                }]
            }
        }
    }

    /// What one session of [`concurrent_encode`] did: when each beat's picture went in, and
    /// when its packet came back, with its bytes.
    struct Coded {
        submitted: Vec<Option<Instant>>,
        back: Vec<Option<(Instant, usize)>>,
    }

    /// The newest beat from `epoch` after `last`, once it is due.
    fn next_beat(epoch: Instant, period: Duration, last: Option<usize>) -> usize {
        loop {
            let due = Instant::now()
                .checked_duration_since(epoch)
                .map(|since| (since.as_nanos() / period.as_nanos()) as usize);
            match (due, last) {
                (Some(d), Some(l)) if d > l => return d,
                (Some(d), None) => return d,
                _ => sleep_until(epoch + period * last.map_or(0, |l| l + 1) as u32),
            }
        }
    }

    /// The sessions of one striped stream, which take each capture together: every one of them
    /// waits for the others' submits to return, then all take the same newest beat.
    type Together = std::sync::Arc<(std::sync::Barrier, std::sync::atomic::AtomicUsize)>;

    /// One session's thread of [`concurrent_encode`]: draw its pictures and say so on `ready`;
    /// once `start` says, open the session and say so on `ready` again; and from the epoch
    /// `start` sends next, submit the newest beat's picture whenever the last submit has
    /// returned, as the worker's encode thread takes the newest capture from its one-frame
    /// mailbox. A stripe waits for its stream's other stripe first (`together`).
    fn code(
        part: Part,
        chroma: Chroma,
        (period, frames): (Duration, usize),
        (ready, start): (&mpsc::Sender<Result<(), String>>, &mpsc::Receiver<Instant>),
        together: Option<&Together>,
    ) -> Result<Coded, String> {
        use std::sync::atomic::Ordering;

        let (w, h) = part.size;
        let pictures: Vec<_> = (0..PICTURES)
            .map(|p| page(w, h, part.top + p * 4, chroma))
            .collect();
        let _sent = ready.send(Ok(()));
        start.recv().map_err(|e| format!("no start: {e}"))?;
        let (tx, rx) = mpsc::channel();
        let encoder = Encoder::new(
            EncoderConfig {
                width: w as u32,
                height: h as u32,
                codec: VideoCodec::Hevc,
                fps: 60,
                bitrate_bps: part.bitrate,
                chroma,
            },
            move |packet| {
                let _sent = tx.send((packet.pts_us, Instant::now(), packet.data.len()));
            },
        );
        let encoder = match encoder {
            Ok(encoder) => encoder,
            Err(e) => {
                let _sent = ready.send(Err(e.to_string()));
                return Err(e.to_string());
            }
        };
        let _sent = ready.send(Ok(()));
        let epoch = start.recv().map_err(|e| format!("no epoch: {e}"))?;
        let mut submitted: Vec<Option<Instant>> = vec![None; frames];
        let mut last: Option<usize> = None;
        let mut failed = None;
        loop {
            let i = match together {
                Some(together) => {
                    let (barrier, beat) = &**together;
                    if barrier.wait().is_leader() {
                        beat.store(next_beat(epoch, period, last), Ordering::SeqCst);
                    }
                    barrier.wait();
                    beat.load(Ordering::SeqCst)
                }
                None => next_beat(epoch, period, last),
            };
            if i >= frames {
                break;
            }
            last = Some(i);
            if failed.is_some() {
                continue;
            }
            let options = FrameOptions {
                force_keyframe: i == 0,
                ..FrameOptions::default()
            };
            submitted[i] = Some(Instant::now());
            // Stamped on the beat, as captures are: the rate control spends by these.
            let pts = i as u64 * period.as_micros() as u64;
            if let Err(e) =
                encoder.encode(&pictures[(i + part.stream * 5) % PICTURES], pts, &options)
            {
                failed = Some(format!("submit: {e}"));
            }
        }
        if let Err(e) = encoder.flush() {
            failed.get_or_insert_with(|| format!("flush: {e}"));
        }
        if let Some(failed) = failed {
            return Err(failed);
        }
        let mut back = vec![None; frames];
        while let Ok((pts, at, bytes)) = rx.try_recv() {
            if let Some(slot) = back.get_mut((pts / period.as_micros() as u64) as usize) {
                *slot = Some((at, bytes));
            }
        }
        Ok(Coded { submitted, back })
    }

    /// What one stream of [`concurrent_encode`] got over the measured beats, from its sessions.
    #[derive(Default)]
    struct Seen {
        /// A capture's first submit → the last of its packets back, ms.
        encode: Vec<f64>,
        /// Captures whose packets all came back.
        done: usize,
        /// Measured beats not submitted to every session of the stream: a submit was still
        /// running when the beat came.
        superseded: usize,
        /// Bytes of packet.
        bytes: usize,
    }

    impl Seen {
        fn of(sessions: &[&Coded]) -> Self {
            let mut seen = Self::default();
            let frames = sessions.first().map_or(0, |s| s.submitted.len());
            for i in WARM..frames {
                let sent: Option<Vec<Instant>> = sessions.iter().map(|s| s.submitted[i]).collect();
                let Some(sent) = sent else {
                    seen.superseded += 1;
                    continue;
                };
                let came: Option<Vec<(Instant, usize)>> =
                    sessions.iter().map(|s| s.back[i]).collect();
                let Some(came) = came else { continue };
                let first = sent.iter().min().copied().unwrap_or_else(Instant::now);
                let last = came.iter().map(|&(at, _)| at).max().unwrap_or(first);
                seen.encode.push((last - first).as_secs_f64() * 1e3);
                seen.bytes += came.iter().map(|&(_, bytes)| bytes).sum::<usize>();
                seen.done += 1;
            }
            seen
        }
    }

    fn quantiles99(mut v: Vec<f64>) -> String {
        if v.is_empty() {
            return "-".to_owned();
        }
        v.sort_by(f64::total_cmp);
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        format!("{:.2}/{:.2}/{:.2}", at(0.5), at(0.95), at(0.99))
    }

    /// Several large streams coded at once, three ways (`ONICODE_ENCODE_MODES`): a session each
    /// (the worker today), two stripe sessions each, or one session for all of them side by
    /// side. Every session has a thread submitting on one 60 Hz beat, the beats of all streams
    /// on the same instants as windows on one display are captured on its refresh, and a thread
    /// that comes back from a submit late skips to the newest beat, as the worker's one-frame
    /// mailbox does. An aligned session codes inside the submit, so a capture's encode is its
    /// first submit → its last packet back.
    ///
    /// Per run: that encode p50 / p95 / p99 for stream 0 and for all, captures coded a second
    /// per stream, and for the process its CPU cores, CPU watts (`ri_energy_nj`) and footprint
    /// growth past the drawn pictures, with every session open and after the run. From
    /// `IOReport` (`onicode_desktop_testkit::soc`), less a second of the same Mac idle just before:
    /// interrupts a second of each encode engine (which engines the sessions ran on), their DRAM
    /// traffic, and the GPU's active share and watts.
    ///
    /// Knobs: `ONICODE_ENCODE_SIZES` (`3840x2160,5120x2880`), `ONICODE_ENCODE_STREAMS`
    /// (`1,2,3,4`), `ONICODE_ENCODE_MODES` (`sessions,stripes,composite`),
    /// `ONICODE_ENCODE_CHROMA` (`420` or `444`), `ONICODE_ENCODE_SECONDS` (4),
    /// `ONICODE_ENCODE_FPS` (60), `ONICODE_ENCODE_RATE` (bits a second a stream; 40 M at 4K, 60 M
    /// at 5K), `ONICODE_ENCODE_BACKGROUND` (`WxH:count`: that many more streams of that size at
    /// 16 Mbit/s, one session each, reported apart).
    #[test]
    #[ignore = "a measurement: cargo test -p onicode-desktop-codec --release --test streams -- --ignored --nocapture"]
    fn concurrent_encode() {
        use onicode_desktop_testkit::soc::Soc;

        let sizes: Vec<String> = knob("ONICODE_ENCODE_SIZES", "3840x2160,5120x2880");
        let counts: Vec<usize> = knob("ONICODE_ENCODE_STREAMS", "1,2,3,4");
        let modes: Vec<Mode> = knob("ONICODE_ENCODE_MODES", "sessions,stripes,composite");
        let chroma = match knob::<u16>("ONICODE_ENCODE_CHROMA", "420").first() {
            Some(444) => Chroma::Full,
            _ => Chroma::Subsampled,
        };
        let seconds = knob::<u64>("ONICODE_ENCODE_SECONDS", "4")
            .first()
            .copied()
            .unwrap_or(4);
        let fps = knob::<u32>("ONICODE_ENCODE_FPS", "60")
            .first()
            .copied()
            .unwrap_or(60);
        let rate: Option<u32> = knob("ONICODE_ENCODE_RATE", "").first().copied();
        let timing = (
            Duration::from_secs(1) / fps,
            WARM + seconds as usize * fps as usize,
        );
        // Streams of another size alongside, one session each: `WxH:count`.
        let background: Vec<((usize, usize), u32)> =
            knob::<String>("ONICODE_ENCODE_BACKGROUND", "")
                .first()
                .and_then(|b| {
                    let (size, count) = b.split_once(':')?;
                    let (w, h) = size.split_once('x')?;
                    let size = (w.parse().ok()?, h.parse().ok()?);
                    Some(vec![(size, 16_000_000); count.parse().ok()?])
                })
                .unwrap_or_default();
        let soc = Soc::open();
        let read = || soc.as_ref().and_then(Soc::read);
        for size in &sizes {
            let (w, h) = size.split_once('x').expect("WxH");
            let (w, h): (usize, usize) = (w.parse().expect("width"), h.parse().expect("height"));
            let bitrate = rate.unwrap_or(if w * h > 3840 * 2160 {
                60_000_000
            } else {
                40_000_000
            });
            for &mode in &modes {
                for &n in &counts {
                    // What the Mac does with none of ours coding: Parsec's encoder, others'.
                    let idle_from = read();
                    sleep_until(Instant::now() + Duration::from_secs(1));
                    let idle = read().zip(idle_from).and_then(|(to, from)| to.since(&from));
                    let mut parts = parts(mode, (w, h), n, bitrate);
                    let streams = parts.iter().map(|p| p.stream + 1).max().unwrap_or(0);
                    parts.extend(
                        background
                            .iter()
                            .enumerate()
                            .map(|(k, &(size, rate))| Part {
                                stream: streams + k,
                                size,
                                top: 0,
                                bitrate: rate,
                            }),
                    );
                    let together: Vec<Option<Together>> = (0..streams + background.len())
                        .map(|stream| {
                            let sessions = parts.iter().filter(|p| p.stream == stream).count();
                            (sessions > 1).then(|| {
                                std::sync::Arc::new((
                                    std::sync::Barrier::new(sessions),
                                    std::sync::atomic::AtomicUsize::new(0),
                                ))
                            })
                        })
                        .collect();
                    let (ready_tx, ready_rx) = mpsc::channel();
                    let mut starts = Vec::new();
                    let threads: Vec<_> = parts
                        .iter()
                        .map(|&part| {
                            let ready = ready_tx.clone();
                            let (start_tx, start) = mpsc::channel();
                            starts.push(start_tx);
                            let together = together[part.stream].clone();
                            std::thread::Builder::new()
                                .name(format!("encode-{}", part.stream))
                                .spawn(move || {
                                    code(part, chroma, timing, (&ready, &start), together.as_ref())
                                })
                                .expect("a thread")
                        })
                        .collect();
                    drop(ready_tx);
                    let all_ready = || -> Result<(), String> {
                        ready_rx
                            .iter()
                            .take(parts.len())
                            .collect::<Result<Vec<()>, String>>()?;
                        Ok(())
                    };
                    // The pictures drawn: what the sessions cost is counted from here.
                    all_ready().expect("pictures");
                    let before_open = usage();
                    for start in &starts {
                        let _sent = start.send(Instant::now());
                    }
                    if let Err(e) = all_ready() {
                        drop(starts);
                        for thread in threads {
                            let _coded = thread.join().expect("a session's thread");
                        }
                        println!(
                            "MEASURE concurrent encode {w}x{h} {chroma:?} {mode:?} streams={n}: \
                             no session: {e}"
                        );
                        continue;
                    }
                    let opened = usage();
                    let epoch = Instant::now() + Duration::from_millis(200);
                    for start in &starts {
                        let _sent = start.send(epoch);
                    }
                    sleep_until(epoch + timing.0 * WARM as u32);
                    let start = usage();
                    let soc_from = read();
                    let measured_from = Instant::now();
                    sleep_until(epoch + timing.0 * timing.1 as u32);
                    let end = usage();
                    let soc_to = read();
                    let wall = measured_from.elapsed();
                    let coded: Result<Vec<Coded>, String> = threads
                        .into_iter()
                        .map(|t| t.join().expect("a session's thread"))
                        .collect();
                    let coded = match coded {
                        Ok(coded) => coded,
                        Err(e) => {
                            println!(
                                "MEASURE concurrent encode {w}x{h} {chroma:?} {mode:?} \
                                 streams={n}: failed: {e}"
                            );
                            continue;
                        }
                    };
                    let seen: Vec<Seen> = (0..streams + background.len())
                        .map(|stream| {
                            let own: Vec<&Coded> = parts
                                .iter()
                                .zip(&coded)
                                .filter(|(part, _)| part.stream == stream)
                                .map(|(_, c)| c)
                                .collect();
                            Seen::of(&own)
                        })
                        .collect();
                    let counted = (timing.1 - WARM) as f64 * timing.0.as_secs_f64();
                    let (seen, behind) = seen.split_at(streams);
                    let all: Vec<f64> =
                        seen.iter().flat_map(|s| s.encode.iter().copied()).collect();
                    let fps_of = |seen: &[Seen]| -> String {
                        seen.iter()
                            .map(|s| format!("{:.1}", s.done as f64 / counted))
                            .collect::<Vec<_>>()
                            .join(",")
                    };
                    let per_stream = fps_of(seen);
                    let behind = if behind.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " background {}: {} ms, fps=[{}]",
                            behind.len(),
                            quantiles99(
                                behind
                                    .iter()
                                    .flat_map(|s| s.encode.iter().copied())
                                    .collect()
                            ),
                            fps_of(behind)
                        )
                    };
                    let superseded: usize = seen.iter().map(|s| s.superseded).sum();
                    let mbps =
                        seen.iter().map(|s| s.bytes).sum::<usize>() as f64 * 8.0 / counted / 1e6;
                    let cores =
                        end.cpu.saturating_sub(start.cpu).as_secs_f64() / wall.as_secs_f64();
                    let watts = (end.energy_nj - start.energy_nj) as f64 / 1e9 / wall.as_secs_f64();
                    let growth =
                        |u: &Usage| (u.footprint as f64 - before_open.footprint as f64) / 1e6;
                    let (opened_mb, end_mb) = (growth(&opened), growth(&end));
                    let engines = soc_to
                        .zip(soc_from)
                        .and_then(|(to, from)| to.since(&from))
                        .zip(idle)
                        .map_or_else(
                            || "soc -".to_owned(),
                            |(span, idle)| {
                                let r = span.less(&idle);
                                let idle_secs = idle.elapsed.as_secs_f64();
                                format!(
                                    "ave_irq/s=+{:.0},+{:.0} (idle {:.0},{:.0}) \
                                     venc_MB/s=+{:.0},+{:.0} gpu_active=+{:.3} gpu_W=+{:.3}",
                                    r.encode_interrupts[0],
                                    r.encode_interrupts[1],
                                    idle.encode_interrupts[0] as f64 / idle_secs,
                                    idle.encode_interrupts[1] as f64 / idle_secs,
                                    r.encode_bytes[0] / 1e6,
                                    r.encode_bytes[1] / 1e6,
                                    r.gpu_active,
                                    r.gpu_watts,
                                )
                            },
                        );
                    println!(
                        "MEASURE concurrent encode {w}x{h} {chroma:?} {mode:?} streams={n} \
                         stream0 p50/p95/p99={} ms all={} ms per_stream_fps=[{per_stream}] \
                         superseded={superseded}{behind} {mbps:.1} Mbit/s cpu_cores={cores:.3} \
                         cpu_W={watts:.3} footprint_mb=+{opened_mb:.1} open, +{end_mb:.1} after \
                         {engines}",
                        quantiles99(seen[0].encode.clone()),
                        quantiles99(all),
                    );
                }
            }
        }
    }

    /// Each stream's own sound, as every stream carries it today: per stream, an Opus encoder (the
    /// worker's) and an Opus decoder (the client's), on its own thread, fed 10 ms of music-like
    /// stereo every 10 ms. Encode and decode µs a packet p50 / p95 across the streams, and the
    /// process's CPU cores and CPU power over the measured seconds, for 1, 2, 4 and 8 streams.
    ///
    /// Knobs: `ONICODE_AUDIO_STREAMS` (`1,2,4,8`), `ONICODE_AUDIO_SECONDS` (4).
    #[test]
    #[ignore = "a measurement: cargo test -p onicode-desktop-codec --release --test streams -- --ignored --nocapture"]
    fn concurrent_audio() {
        use onicode_desktop_codec::audio::{OpusDecoder, OpusEncoder};

        let counts: Vec<usize> = knob("ONICODE_AUDIO_STREAMS", "1,2,4,8");
        let seconds: Vec<u64> = knob("ONICODE_AUDIO_SECONDS", "4");
        let packets = seconds.first().copied().unwrap_or(4) as usize * 100;
        let warm = 20;
        let period = Duration::from_millis(10);
        // Two detuned tones and a little noise, so the encoder has real work.
        let sound: Vec<f32> = (0..48_000 * 2)
            .map(|i| {
                let t = (i / 2) as f32 / 48_000.0;
                let noise = ((i as u32).wrapping_mul(2_654_435_761) >> 16) as f32 / 65_536.0 - 0.5;
                let tone = |hz: f32| (t * hz * std::f32::consts::TAU).sin();
                0.02_f32.mul_add(noise, 0.3_f32.mul_add(tone(440.0), 0.2 * tone(661.0)))
            })
            .collect();
        for &n in &counts {
            let epoch = Instant::now() + Duration::from_millis(200);
            let (done_tx, done_rx) = mpsc::channel();
            let threads: Vec<_> = (0..n)
                .map(|stream| {
                    let sound = sound.clone();
                    let done_tx = done_tx.clone();
                    std::thread::Builder::new()
                        .name(format!("audio-{stream}"))
                        .spawn(move || {
                            let mut encoder = OpusEncoder::new().expect("an Opus encoder");
                            let mut decoder = OpusDecoder::new().expect("an Opus decoder");
                            let mut encode = Vec::with_capacity(packets);
                            let mut decode = Vec::with_capacity(packets);
                            for p in 0..warm + packets {
                                sleep_until(epoch + period * p as u32);
                                let from = (p * 960) % (sound.len() - 960);
                                let mut out = Vec::new();
                                let at = Instant::now();
                                encoder
                                    .push(&sound[from..from + 960], |packet| {
                                        out.push(packet.to_vec());
                                    })
                                    .expect("encode");
                                let encoded = at.elapsed();
                                let at = Instant::now();
                                for packet in &out {
                                    let _pcm = decoder.decode(packet).expect("decode");
                                }
                                let decoded = at.elapsed();
                                if p >= warm {
                                    encode.push(encoded.as_secs_f64() * 1e6);
                                    decode.push(decoded.as_secs_f64() * 1e6);
                                }
                            }
                            let _sent = done_tx.send((encode, decode));
                        })
                        .expect("a thread")
                })
                .collect();
            drop(done_tx);
            sleep_until(epoch + period * warm as u32);
            let start = usage();
            let measured_from = Instant::now();
            sleep_until(epoch + period * (warm + packets) as u32);
            let end = usage();
            let wall = measured_from.elapsed();
            let results: Vec<_> = done_rx.iter().collect();
            for thread in threads {
                thread.join().expect("the stream's thread");
            }
            let (e50, e95, _) =
                quantiles(results.iter().flat_map(|r| r.0.iter().copied()).collect());
            let (d50, d95, _) =
                quantiles(results.iter().flat_map(|r| r.1.iter().copied()).collect());
            let cores = end.cpu.saturating_sub(start.cpu).as_secs_f64() / wall.as_secs_f64();
            let watts = (end.energy_nj - start.energy_nj) as f64 / 1e9 / wall.as_secs_f64();
            println!(
                "MEASURE concurrent audio streams={n} encode p50/p95={e50:.1}/{e95:.1} µs \
                 decode p50/p95={d50:.1}/{d95:.1} µs cpu_cores={cores:.4} cpu_watts={watts:.4}"
            );
        }
    }
}
