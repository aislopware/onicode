//! macOS's seams: ScreenCaptureKit, VideoToolbox, Opus and CoreGraphics events, and the window
//! server's display list. The only file besides the made displays ([`crate::sized`],
//! [`crate::holder`]) that names the platform's libraries.
//!
//! The display list is CoreGraphics', which needs no grant and prompts for none:
//! ScreenCaptureKit's shareable content is asked only by a stream's open, which the session lets
//! through only once Screen Recording reads granted.

use onicode_desktop_proto::screen::DisplayInfo;

use super::Platform;

/// A Mac.
#[derive(Clone, Copy, Debug)]
pub enum MacOs {}

impl Platform for MacOs {
    type Audio = onicode_desktop_codec::Opus;
    type Capture = onicode_desktop_capture::ScreenCaptureKit;
    type Input = onicode_desktop_input::InputThread;
    type Video = onicode_desktop_codec::VideoToolbox;

    fn displays() -> Vec<DisplayInfo> {
        onicode_desktop_capture::active_displays()
    }

    fn made_for_a_client(id: u32) -> bool {
        onicode_desktop_vdisplay::made_for_a_client(id)
    }
}
