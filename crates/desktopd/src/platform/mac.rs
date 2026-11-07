//! macOS's seams: ScreenCaptureKit, VideoToolbox, Opus and CoreGraphics events, the window
//! server's display list, the curtain's shield windows and the power assertion that keeps the
//! displays awake. The only file besides the made displays ([`crate::sized`]) that names the
//! platform's libraries.
//!
//! The display list is CoreGraphics', which needs no grant and prompts for none:
//! ScreenCaptureKit's shareable content is asked only by a stream's open, which the session lets
//! through only once Screen Recording reads granted.

use onicode_desktop_capture::{KeepAwake, Shield};
use onicode_desktop_proto::screen::DisplayInfo;

use super::Platform;
use crate::curtain::Drapes;

/// A Mac.
#[derive(Clone, Copy, Debug)]
pub enum MacOs {}

impl Platform for MacOs {
    type Audio = onicode_desktop_codec::Opus;
    type Capture = onicode_desktop_capture::ScreenCaptureKit;
    type Drapes = Shielded;
    type Input = onicode_desktop_input::InputThread;
    type Video = onicode_desktop_codec::VideoToolbox;

    fn displays() -> Vec<DisplayInfo> {
        onicode_desktop_capture::active_displays()
    }

    fn made_for_a_client(id: u32) -> bool {
        onicode_desktop_vdisplay::made_for_a_client(id)
    }

    fn keep_awake() -> Option<Box<dyn Send + Sync>> {
        KeepAwake::hold("Onicode Desktop: a viewer is joined")
            .map(|held| Box::new(held) as Box<dyn Send + Sync>)
    }
}

/// The curtain on a Mac: a black shield window over each of its own displays (never over one
/// made for a client), which no capture shows ([`Shield`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Shielded;

impl Drapes for Shielded {
    type Raised = Shield;

    fn raise(&mut self) -> Result<Shield, String> {
        Shield::raise(onicode_desktop_vdisplay::made_for_a_client)
    }

    fn show(&mut self, raised: &mut Shield) {
        raised.show();
    }

    /// The Mac's own keyboard and pointer still work under the shield: its windows let the mouse
    /// through, so the controller's own clicks reach the apps beneath.
    fn input_held(&self, _raised: &Shield) -> bool {
        false
    }

    fn lower(&mut self, raised: Shield) {
        drop(raised);
    }
}
