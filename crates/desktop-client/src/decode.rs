//! The picture decoder seam: VideoToolbox on Apple platforms, a fake under test.
//!
//! A stream's lanes are generic over [`VideoDecode`], so the whole receive path (routing,
//! reassembly, the decode threads, stitching, presenting) runs in the tests with a decoder that
//! makes pictures of its own, and on a Mac or an iPhone with `onicode_desktop_codec::Decoder`.

use bytes::Bytes;
use onicode_desktop_media::wire::VideoCodec;

/// A decoded picture, as the stream hands it on.
pub trait Image: Clone + Send + Sync + std::fmt::Debug + 'static {
    /// Width in pixels.
    fn width(&self) -> usize;
    /// Height in pixels.
    fn height(&self) -> usize;
}

/// A decoded picture and the presentation timestamp it was submitted with.
#[derive(Clone, Debug)]
pub struct DecodedFrame<P = Picture> {
    /// The picture: on Apple platforms an IOSurface-backed, Metal-compatible `CVPixelBuffer`.
    pub image: P,
    /// The presentation timestamp given to the decoder.
    pub pts_us: u64,
}

/// What a decoder made of one submitted frame: its picture, or why there is none.
#[derive(Clone, Debug)]
pub enum Outcome<P> {
    /// The picture.
    Picture(DecodedFrame<P>),
    /// No picture.
    Failed {
        /// The frame's presentation timestamp.
        pts_us: u64,
        /// The platform's status; zero when it dropped the frame without naming an error.
        status: i32,
        /// The session is gone: only a keyframe decodes again.
        session_lost: bool,
    },
}

/// Where a decoder leaves its outcomes, on its own thread.
pub type OutcomeSink<P> = Box<dyn Fn(Outcome<P>) + Send + Sync>;

/// A frame refused as it was submitted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Refused {
    /// The platform's status when the refusal was the platform's; `None` for one of the
    /// decoder's own (no parameter sets yet, a malformed unit).
    pub status: Option<i32>,
}

/// One lane's hardware decoder.
pub trait VideoDecode: Send + 'static {
    /// The pictures it makes.
    type Image: Image;

    /// It decodes on the submitting thread, at once, and never blocks (a test's fake): the stream
    /// task submits to it in place with no decode thread, so on a paused tokio clock a picture
    /// comes out at the virtual instant its frame completed. Off for a platform decoder, whose
    /// submission can block its caller.
    const INLINE: bool = false;

    /// A decoder for `codec` that hands every submitted frame's outcome to `sink`.
    fn start(codec: VideoCodec, sink: OutcomeSink<Self::Image>) -> Self;

    /// Submit one length-prefixed access unit. May block inside the platform: it runs on the
    /// lane's decode thread, never on the runtime.
    ///
    /// # Errors
    ///
    /// The frame was refused as it was submitted.
    fn decode(&mut self, access_unit: &Bytes, pts_us: u64) -> Result<(), Refused>;

    /// Whether a keyframe has configured a session.
    fn ready(&self) -> bool;
}

/// The picture this platform decodes to.
pub type Picture = onicode_desktop_codec::PixelBuffer;

/// The decoder this platform has.
pub type PlatformDecoder = VideoToolbox;

impl Image for onicode_desktop_codec::PixelBuffer {
    fn width(&self) -> usize {
        Self::width(self)
    }

    fn height(&self) -> usize {
        Self::height(self)
    }
}

/// VideoToolbox, through `onicode_desktop_codec::Decoder`.
pub struct VideoToolbox(onicode_desktop_codec::Decoder);

impl VideoDecode for VideoToolbox {
    type Image = onicode_desktop_codec::PixelBuffer;

    fn start(codec: VideoCodec, sink: OutcomeSink<Self::Image>) -> Self {
        Self(onicode_desktop_codec::Decoder::with_outcomes(
            codec,
            move |outcome| {
                sink(match outcome {
                    Ok(frame) => Outcome::Picture(DecodedFrame {
                        image: frame.image,
                        pts_us: frame.pts_us,
                    }),
                    Err(failure) => Outcome::Failed {
                        pts_us: failure.pts_us,
                        status: failure.status,
                        session_lost: failure.session_lost(),
                    },
                });
            },
        ))
    }

    fn decode(&mut self, access_unit: &Bytes, pts_us: u64) -> Result<(), Refused> {
        self.0.decode(access_unit, pts_us).map_err(|e| Refused {
            status: match e {
                onicode_desktop_codec::CodecError::Os { status, .. } => Some(status),
                _ => None,
            },
        })
    }

    fn ready(&self) -> bool {
        self.0.ready()
    }
}

/// A decoder for the tests: it "decodes" a frame whose first byte says what to do.
#[cfg(test)]
pub(crate) mod fake {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    /// A fake picture: the bytes it was decoded from, and a size.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) struct Drawn {
        pub(crate) bytes: Bytes,
        pub(crate) width: usize,
        pub(crate) height: usize,
    }

    impl Image for Drawn {
        fn width(&self) -> usize {
            self.width
        }

        fn height(&self) -> usize {
            self.height
        }
    }

    /// The first byte of a frame the fake decodes: a keyframe configures it.
    pub(crate) const KEY: u8 = b'K';
    /// The first byte of a frame it fails in its callback.
    pub(crate) const FAIL: u8 = b'F';
    /// The first byte of a frame it refuses as it is submitted.
    pub(crate) const REFUSE: u8 = b'R';
    /// The first byte of a frame that loses its session.
    pub(crate) const LOSE: u8 = b'L';
    /// The first byte of a frame whose submission waits inside the decoder while [`gate`] is
    /// closed, as VideoToolbox held one on a hosted virtual Mac.
    pub(crate) const HOLD: u8 = b'H';

    /// Holds the submissions of [`HOLD`] frames until a test lets them go.
    pub(crate) mod gate {
        use parking_lot::{Condvar, Mutex};

        static CLOSED: Mutex<bool> = Mutex::new(false);
        static LET_GO: Condvar = Condvar::new();
        static WAITING: Mutex<u32> = Mutex::new(0);

        /// Hold every [`super::HOLD`] submission from now on.
        pub(crate) fn close() {
            *CLOSED.lock() = true;
        }

        /// Let them go.
        pub(crate) fn open() {
            *CLOSED.lock() = false;
            LET_GO.notify_all();
        }

        /// Submissions waiting inside a decoder now.
        pub(crate) fn waiting() -> u32 {
            *WAITING.lock()
        }

        pub(super) fn wait() {
            let mut closed = CLOSED.lock();
            if !*closed {
                return;
            }
            *WAITING.lock() += 1;
            while *closed {
                LET_GO.wait(&mut closed);
            }
            *WAITING.lock() -= 1;
        }
    }

    /// The fake picture's width.
    pub(crate) const WIDTH: usize = 64;
    /// Its height: more than two stripe overlaps, so a stitched picture has rows of both.
    pub(crate) const HEIGHT: usize = 256;

    /// Decodes in the submitting thread: a [`WIDTH`] × [`HEIGHT`] picture per frame.
    pub(crate) struct Fake {
        sink: OutcomeSink<Drawn>,
        ready: Arc<AtomicBool>,
    }

    impl VideoDecode for Fake {
        type Image = Drawn;

        fn start(_codec: VideoCodec, sink: OutcomeSink<Drawn>) -> Self {
            Self {
                sink,
                ready: Arc::new(AtomicBool::new(false)),
            }
        }

        fn decode(&mut self, access_unit: &Bytes, pts_us: u64) -> Result<(), Refused> {
            if access_unit.first() == Some(&HOLD) {
                gate::wait();
            }
            match access_unit.first().copied() {
                Some(KEY) => self.ready.store(true, Ordering::Relaxed),
                Some(REFUSE) => {
                    return Err(Refused {
                        status: Some(-12909),
                    });
                }
                _ if !self.ready.load(Ordering::Relaxed) => {
                    return Err(Refused { status: None });
                }
                _ => {}
            }
            let outcome = match access_unit.first().copied() {
                Some(FAIL) => Outcome::Failed {
                    pts_us,
                    status: -12909,
                    session_lost: false,
                },
                Some(LOSE) => {
                    self.ready.store(false, Ordering::Relaxed);
                    Outcome::Failed {
                        pts_us,
                        status: -12903,
                        session_lost: true,
                    }
                }
                _ => Outcome::Picture(DecodedFrame {
                    image: Drawn {
                        bytes: access_unit.clone(),
                        width: WIDTH,
                        height: HEIGHT,
                    },
                    pts_us,
                }),
            };
            (self.sink)(outcome);
            Ok(())
        }

        fn ready(&self) -> bool {
            self.ready.load(Ordering::Relaxed)
        }
    }
}
