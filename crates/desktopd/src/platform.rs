//! The platform desktopd runs on, as the seams its streams use.
//!
//! The seams are capture (which also reads the cursor and the pointer), the video and audio
//! encoders, input, the curtain's drapes, keeping the displays awake, and the display list. Each
//! of the first four is a trait of its desktop crate or of [`crate::curtain`], and
//! a [`Platform`] names one implementation of each, so every call on the frame path is a direct
//! one. [`Native`] is macOS's (`mac`); the tests' is [`crate::synthetic::Drawn`].

use onicode_desktop_capture::CaptureSource;
use onicode_desktop_codec::{AudioEncoder, VideoEncoder};
use onicode_desktop_input::InputSink;
use onicode_desktop_proto::screen::DisplayInfo;

use crate::curtain::Drapes;

pub mod mac;

/// One implementation of each seam.
pub trait Platform: Send + Sync + 'static {
    /// Pictures of a display, the cursor and the pointer.
    type Capture: CaptureSource<Image: Clone + Send + Sync>;
    /// The video encoder, coding what the capture delivers.
    type Video: VideoEncoder<Image = <Self::Capture as CaptureSource>::Image>;
    /// The sound's encoder.
    type Audio: AudioEncoder;
    /// Input into the Mac.
    type Input: InputSink;
    /// What draws the curtain over the Mac's own screens.
    type Drapes: Drapes + Default;

    /// Every call into this platform's capture, encoders and input is cheap and never waits, and
    /// its capture runs on tokio's timers: a stream makes each call on its own tasks, with no
    /// encode thread and no blocking pool, so a paused tokio clock drives the whole stream and a
    /// seed repeats it (the drawn platform). Off where a call can block, as VideoToolbox's and
    /// the window server's do.
    const ON_RUNTIME: bool = false;

    /// The displays, as the window server lists them: never through ScreenCaptureKit, whose
    /// listing prompts for Screen Recording.
    fn displays() -> Vec<DisplayInfo>;

    /// Whether display `id` is one made for a client ([`crate::sized`]): never listed as a
    /// physical display.
    fn made_for_a_client(_id: u32) -> bool {
        false
    }

    /// Keep the displays awake while a viewer is joined ([`Awake`]); dropping the answer lets
    /// go. `None` where the platform holds nothing.
    fn keep_awake() -> Option<Box<dyn Send + Sync>> {
        None
    }
}

/// The displays kept awake while any viewer is joined: the platform's hold is taken with the
/// first and let go with the last.
#[derive(Default)]
pub struct Awake {
    joined: u32,
    held: Option<Box<dyn Send + Sync>>,
}

impl std::fmt::Debug for Awake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Awake")
            .field("joined", &self.joined)
            .field("held", &self.held.is_some())
            .finish()
    }
}

impl Awake {
    /// A viewer joined: the first takes the hold `hold` makes.
    pub fn join(&mut self, hold: impl FnOnce() -> Option<Box<dyn Send + Sync>>) {
        self.joined = self.joined.saturating_add(1);
        if self.joined == 1 {
            self.held = hold();
        }
    }

    /// A joined viewer left: the last lets go.
    pub fn leave(&mut self) {
        self.joined = self.joined.saturating_sub(1);
        if self.joined == 0 {
            self.held = None;
        }
    }

    /// Viewers joined now.
    #[must_use]
    pub const fn joined(&self) -> u32 {
        self.joined
    }
}

/// The platform this build streams from.
pub type Native = mac::MacOs;

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::Awake;

    /// A hold that counts itself while it lives.
    struct Held(Arc<AtomicU32>);

    impl Drop for Held {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// The displays are held once while anyone is joined, and let go with the last to leave.
    #[test]
    fn the_displays_stay_awake_while_anyone_is_joined() {
        let holds = Arc::new(AtomicU32::new(0));
        let hold = || {
            let holds = Arc::clone(&holds);
            move || {
                holds.fetch_add(1, Ordering::Relaxed);
                Some(Box::new(Held(holds)) as Box<dyn Send + Sync>)
            }
        };
        let mut awake = Awake::default();
        awake.join(hold());
        awake.join(hold());
        assert_eq!(holds.load(Ordering::Relaxed), 1, "held once");
        awake.leave();
        assert_eq!(holds.load(Ordering::Relaxed), 1, "one still joined");
        awake.leave();
        assert_eq!(holds.load(Ordering::Relaxed), 0, "let go with the last");
        awake.leave();
        assert_eq!(awake.joined(), 0, "a stray leave never goes below none");
        awake.join(hold());
        assert_eq!(holds.load(Ordering::Relaxed), 1, "held again");
    }
}
