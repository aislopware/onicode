//! The curtain over the Mac's own screens while clients drive it
//! ([`ScreenRequest::Curtain`](onicode_desktop_proto::screen::ScreenRequest::Curtain)).
//!
//! Each client that asks holds it; it is drawn while anyone holds it and lifted when the last
//! lets go or leaves. Drawing it is the platform's ([`Drapes`]): a shield over every display
//! but those made for clients, left out of every capture, and the Mac's own keyboard and
//! pointer held off. That shield is not ported yet (slopty's `slopty_platform::curtain`), so
//! every platform's drapes refuse and a client is told why ([`CurtainState::Refused`]).

use std::collections::BTreeSet;

use onicode_desktop_proto::screen::CurtainState;
use parking_lot::Mutex;

use crate::seat::ClientId;

/// What draws the curtain.
pub trait Drapes: Send + 'static {
    /// The curtain while it is drawn.
    type Raised: Send;
    /// Draw it.
    ///
    /// # Errors
    ///
    /// Why it could not be drawn, in words for the client.
    fn raise(&mut self) -> Result<Self::Raised, String>;
    /// Whether the Mac's own input is held off under it.
    fn input_held(&self, raised: &Self::Raised) -> bool;
    /// Lift it.
    fn lower(&mut self, raised: Self::Raised);
}

/// The platform's drapes: none yet.
#[derive(Clone, Copy, Debug, Default)]
pub struct Native;

impl Drapes for Native {
    type Raised = ();

    fn raise(&mut self) -> Result<(), String> {
        Err("this Mac's desktopd has no curtain yet".to_owned())
    }

    fn input_held(&self, (): &()) -> bool {
        false
    }

    fn lower(&mut self, (): ()) {}
}

struct Stage<D: Drapes> {
    drapes: D,
    raised: Option<D::Raised>,
    holders: BTreeSet<ClientId>,
    refused: Option<String>,
}

/// The curtain, held by clients.
pub struct Curtain<D: Drapes = Native> {
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
                holders: BTreeSet::new(),
                refused: None,
            }),
        }
    }

    /// Where the curtain stands.
    #[must_use]
    pub fn state(&self) -> CurtainState {
        let stage = self.stage.lock();
        if let Some(why) = &stage.refused {
            return CurtainState::Refused { why: why.clone() };
        }
        match &stage.raised {
            None => CurtainState::Down,
            Some(raised) => CurtainState::Up {
                holders: u32::try_from(stage.holders.len()).unwrap_or(u32::MAX),
                input_held: stage.drapes.input_held(raised),
            },
        }
    }

    /// `client` asks for the curtain drawn (`on`) or lets go of it. Where it stands after.
    pub fn hold(&self, client: ClientId, on: bool) -> CurtainState {
        {
            let mut stage = self.stage.lock();
            stage.refused = None;
            if on {
                stage.holders.insert(client);
            } else {
                stage.holders.remove(&client);
            }
            Self::settle(&mut stage);
        }
        self.state()
    }

    /// `client` left: it holds the curtain no more.
    pub fn went(&self, client: ClientId) {
        let mut stage = self.stage.lock();
        if stage.holders.remove(&client) {
            Self::settle(&mut stage);
        }
    }

    fn settle(stage: &mut Stage<D>) {
        match (stage.holders.is_empty(), stage.raised.take()) {
            (true, Some(raised)) => stage.drapes.lower(raised),
            (false, None) => match stage.drapes.raise() {
                Ok(raised) => stage.raised = Some(raised),
                Err(why) => {
                    tracing::info!(%why, "the curtain was not drawn");
                    stage.holders.clear();
                    stage.refused = Some(why);
                }
            },
            (_, raised) => stage.raised = raised,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use pretty_assertions::assert_eq;

    use super::*;

    #[derive(Default)]
    struct Counted(Arc<AtomicU32>);

    impl Drapes for Counted {
        type Raised = ();

        fn raise(&mut self) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn input_held(&self, (): &()) -> bool {
            true
        }

        fn lower(&mut self, (): ()) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn drawn_while_anyone_holds_it() {
        let up = Arc::new(AtomicU32::new(0));
        let curtain = Curtain::new(Counted(Arc::clone(&up)));
        assert_eq!(curtain.state(), CurtainState::Down);
        assert_eq!(
            curtain.hold(1, true),
            CurtainState::Up {
                holders: 1,
                input_held: true
            }
        );
        curtain.hold(2, true);
        assert_eq!(up.load(Ordering::Relaxed), 1, "drawn once");
        curtain.hold(1, false);
        assert!(matches!(
            curtain.state(),
            CurtainState::Up { holders: 1, .. }
        ));
        curtain.went(2);
        assert_eq!(curtain.state(), CurtainState::Down);
        assert_eq!(up.load(Ordering::Relaxed), 0, "lifted with the last");
    }

    #[test]
    fn a_curtain_that_cannot_be_drawn_says_why() {
        let curtain = Curtain::<Native>::default();
        assert!(matches!(
            curtain.hold(1, true),
            CurtainState::Refused { .. }
        ));
        assert_eq!(curtain.hold(1, false), CurtainState::Down);
    }
}
