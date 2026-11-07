//! The curtain over the Mac's own screens while a controller drives it
//! ([`ScreenRequest::Curtain`](onicode_desktop_proto::screen::ScreenRequest::Curtain)).
//!
//! The client holding the controls may draw it; it stays drawn while anyone holds it, and lifts
//! when its holder lets go, loses the controls or leaves, and when desktopd exits (a process
//! that dies takes its windows with it, so even a crash lifts it). The session asks for it only
//! on behalf of the controller ([`crate::session`]).
//!
//! Drawing it is the platform's ([`Drapes`]): on a Mac a black shield window over each of its own
//! displays, which every display capture leaves out, so the person at the Mac sees black and the
//! viewer sees the desktop (`onicode_desktop_capture::Shield`). Drawing is two steps so no
//! viewer ever sees the shield: [`Curtain::hold`] raises it unseen ([`Moved::Raised`]), the
//! session has every running capture resolve its filter again, and only then
//! [`Curtain::show`] puts it on screen. The Mac's own keyboard and pointer are not held off
//! (`input_held` is false): the shield lets the mouse through, which is what lets the
//! controller's own clicks reach the apps beneath it.

use std::collections::BTreeSet;

use onicode_desktop_proto::screen::CurtainState;
use parking_lot::Mutex;

use crate::seat::ClientId;

/// What draws the curtain.
pub trait Drapes: Send + 'static {
    /// The curtain while it is drawn.
    type Raised: Send;
    /// Draw it, not yet seen: made, and left out of every capture resolved from now on.
    ///
    /// # Errors
    ///
    /// Why it could not be drawn, in words for the client.
    fn raise(&mut self) -> Result<Self::Raised, String>;
    /// Put it on screen, once every running capture leaves it out.
    fn show(&mut self, raised: &mut Self::Raised);
    /// Whether the Mac's own input is held off under it.
    fn input_held(&self, raised: &Self::Raised) -> bool;
    /// Lift it.
    fn lower(&mut self, raised: Self::Raised);
}

/// The drapes of a host with no screens of its own: every curtain is refused.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoScreens;

impl Drapes for NoScreens {
    type Raised = ();

    fn raise(&mut self) -> Result<(), String> {
        Err("this host has no screens to cover".to_owned())
    }

    fn show(&mut self, (): &mut ()) {}

    fn input_held(&self, (): &()) -> bool {
        false
    }

    fn lower(&mut self, (): ()) {}
}

/// The drapes of the drawn platforms: the curtain is drawn as far as anyone can tell, and
/// covers nothing, since no one sits at a drawn display.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unseen;

impl Drapes for Unseen {
    type Raised = ();

    fn raise(&mut self) -> Result<(), String> {
        Ok(())
    }

    fn show(&mut self, (): &mut ()) {}

    fn input_held(&self, (): &()) -> bool {
        false
    }

    fn lower(&mut self, (): ()) {}
}

/// What a change of holders did to the curtain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Moved {
    /// Nothing anyone sees: it was already drawn, or already down.
    Same,
    /// Drawn, unseen until [`Curtain::show`] with this `generation`.
    Raised {
        /// Which drawing it is: a show meant for an earlier one does nothing.
        generation: u64,
    },
    /// Lifted.
    Lowered,
}

struct Stage<D: Drapes> {
    drapes: D,
    raised: Option<D::Raised>,
    /// The drawing now raised, and whether it is on screen.
    generation: u64,
    shown: bool,
    holders: BTreeSet<ClientId>,
}

/// The curtain, held by clients.
pub struct Curtain<D: Drapes> {
    stage: Mutex<Stage<D>>,
}

impl<D: Drapes> std::fmt::Debug for Curtain<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Curtain")
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl<D: Drapes + Default> Default for Curtain<D> {
    fn default() -> Self {
        Self::new(D::default())
    }
}

impl<D: Drapes> Curtain<D> {
    /// Down, drawn by `drapes` when asked.
    pub fn new(drapes: D) -> Self {
        Self {
            stage: Mutex::new(Stage {
                drapes,
                raised: None,
                generation: 0,
                shown: false,
                holders: BTreeSet::new(),
            }),
        }
    }

    /// Where the curtain stands: down, or drawn (from its raising, a moment before it shows).
    #[must_use]
    pub fn state(&self) -> CurtainState {
        let stage = self.stage.lock();
        match &stage.raised {
            None => CurtainState::Down,
            Some(raised) => CurtainState::Up {
                holders: u32::try_from(stage.holders.len()).unwrap_or(u32::MAX),
                input_held: stage.drapes.input_held(raised),
            },
        }
    }

    /// `client` asks for the curtain drawn (`on`) or lets go of it.
    ///
    /// # Errors
    ///
    /// Why the drapes could not draw it; then `client` does not hold it.
    pub fn hold(&self, client: ClientId, on: bool) -> Result<Moved, String> {
        let mut stage = self.stage.lock();
        if on {
            stage.holders.insert(client);
        } else {
            stage.holders.remove(&client);
        }
        let moved = Self::settle(&mut stage);
        if moved.is_err() {
            stage.holders.remove(&client);
        }
        moved
    }

    /// `client` holds the curtain no more: it left, or lost the controls.
    pub fn went(&self, client: ClientId) -> Moved {
        let mut stage = self.stage.lock();
        if !stage.holders.remove(&client) {
            return Moved::Same;
        }
        Self::settle(&mut stage).unwrap_or(Moved::Same)
    }

    /// Put drawing `generation` on screen, when it is still the one drawn and not yet shown;
    /// whether it was.
    pub fn show(&self, generation: u64) -> bool {
        let mut stage = self.stage.lock();
        if stage.shown || stage.generation != generation {
            return false;
        }
        let Stage { drapes, raised, .. } = &mut *stage;
        let Some(raised) = raised.as_mut() else {
            return false;
        };
        drapes.show(raised);
        stage.shown = true;
        true
    }

    /// Lift it whoever holds it: desktopd is about to exit.
    pub fn lift(&self) -> Moved {
        let mut stage = self.stage.lock();
        stage.holders.clear();
        Self::settle(&mut stage).unwrap_or(Moved::Same)
    }

    fn settle(stage: &mut Stage<D>) -> Result<Moved, String> {
        match (stage.holders.is_empty(), stage.raised.take()) {
            (true, Some(raised)) => {
                stage.drapes.lower(raised);
                stage.shown = false;
                Ok(Moved::Lowered)
            }
            (false, None) => match stage.drapes.raise() {
                Ok(raised) => {
                    stage.raised = Some(raised);
                    stage.generation = stage.generation.wrapping_add(1);
                    stage.shown = false;
                    Ok(Moved::Raised {
                        generation: stage.generation,
                    })
                }
                Err(why) => {
                    tracing::info!(%why, "the curtain was not drawn");
                    Err(why)
                }
            },
            (_, raised) => {
                stage.raised = raised;
                Ok(Moved::Same)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use pretty_assertions::assert_eq;

    use super::*;

    /// Drapes that count how many are drawn and how many shown.
    #[derive(Default)]
    struct Counted {
        up: Arc<AtomicU32>,
        shown: Arc<AtomicU32>,
    }

    impl Drapes for Counted {
        type Raised = bool;

        fn raise(&mut self) -> Result<bool, String> {
            self.up.fetch_add(1, Ordering::Relaxed);
            Ok(false)
        }

        fn show(&mut self, raised: &mut bool) {
            assert!(!*raised, "shown once per drawing");
            *raised = true;
            self.shown.fetch_add(1, Ordering::Relaxed);
        }

        fn input_held(&self, _raised: &bool) -> bool {
            false
        }

        fn lower(&mut self, raised: bool) {
            self.up.fetch_sub(1, Ordering::Relaxed);
            if raised {
                self.shown.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    fn counted() -> (Curtain<Counted>, Arc<AtomicU32>, Arc<AtomicU32>) {
        let drapes = Counted::default();
        let (up, shown) = (Arc::clone(&drapes.up), Arc::clone(&drapes.shown));
        (Curtain::new(drapes), up, shown)
    }

    #[test]
    fn drawn_while_anyone_holds_it() {
        let (curtain, up, _shown) = counted();
        assert_eq!(curtain.state(), CurtainState::Down);
        assert_eq!(curtain.hold(1, true), Ok(Moved::Raised { generation: 1 }));
        assert_eq!(
            curtain.state(),
            CurtainState::Up {
                holders: 1,
                input_held: false
            }
        );
        assert_eq!(curtain.hold(2, true), Ok(Moved::Same));
        assert_eq!(up.load(Ordering::Relaxed), 1, "drawn once");
        assert_eq!(curtain.hold(1, false), Ok(Moved::Same));
        assert!(matches!(
            curtain.state(),
            CurtainState::Up { holders: 1, .. }
        ));
        assert_eq!(curtain.went(2), Moved::Lowered);
        assert_eq!(curtain.state(), CurtainState::Down);
        assert_eq!(up.load(Ordering::Relaxed), 0, "lifted with the last");
        assert_eq!(curtain.went(2), Moved::Same, "gone twice is gone once");
    }

    /// It shows only when asked, once, and only the drawing it was raised for: a show that
    /// comes after the curtain was lifted and drawn again waits for its own.
    #[test]
    fn it_shows_once_and_only_its_own_drawing() {
        let (curtain, _up, shown) = counted();
        let Ok(Moved::Raised { generation }) = curtain.hold(1, true) else {
            panic!("drawn");
        };
        assert_eq!(shown.load(Ordering::Relaxed), 0, "unseen until shown");
        assert!(curtain.show(generation));
        assert!(!curtain.show(generation), "shown once");
        assert_eq!(shown.load(Ordering::Relaxed), 1);
        assert_eq!(curtain.went(1), Moved::Lowered);
        assert_eq!(shown.load(Ordering::Relaxed), 0, "lifted from the screen");
        assert!(!curtain.show(generation), "nothing drawn to show");
        let Ok(Moved::Raised { generation: next }) = curtain.hold(1, true) else {
            panic!("drawn again");
        };
        assert_ne!(next, generation);
        assert!(
            !curtain.show(generation),
            "an old show does not show the new one"
        );
        assert!(curtain.show(next));
    }

    /// The holder losing the controls or leaving lifts it, and so does desktopd exiting.
    #[test]
    fn it_lifts_with_its_holder_and_on_exit() {
        let (curtain, up, _shown) = counted();
        curtain.hold(7, true).unwrap();
        assert_eq!(curtain.went(7), Moved::Lowered, "lost the controls");
        curtain.hold(7, true).unwrap();
        curtain.hold(8, true).unwrap();
        assert_eq!(curtain.lift(), Moved::Lowered);
        assert_eq!(curtain.state(), CurtainState::Down);
        assert_eq!(up.load(Ordering::Relaxed), 0);
        assert_eq!(curtain.lift(), Moved::Same);
    }

    #[test]
    fn a_curtain_that_cannot_be_drawn_says_why_and_holds_nothing() {
        let curtain = Curtain::<NoScreens>::default();
        assert_eq!(
            curtain.hold(1, true),
            Err("this host has no screens to cover".to_owned())
        );
        assert_eq!(curtain.state(), CurtainState::Down);
        assert_eq!(curtain.went(1), Moved::Same, "it held nothing");
        assert_eq!(curtain.hold(1, false), Ok(Moved::Same));
    }
}
