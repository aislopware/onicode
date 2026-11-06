//! The controller seat: one client drives the Mac, any number watch.
//!
//! Every client gets its own capture and encode (per-viewer encodes), but only the controller's
//! input reaches the Mac. A client takes the seat by joining with `control` or by asking
//! (`Up::Control { take: true }`), and the last to ask holds it: they are the owner's own
//! devices, and the one in front of the person is the one asking. Only a client the tailnet
//! says is the owner's may take it. Leaving, or letting go, frees it; nobody else takes it
//! until they ask.
//!
//! Pure: [`crate::session`] keeps one [`Seats`] for desktopd and tells every client its view
//! ([`Seats::view`]) whenever it changes.

use std::collections::BTreeMap;

use onicode_desktop_link::Seat;

/// A client of desktopd: one link.
pub type ClientId = u64;

/// Why a client may not take the seat.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Refusal {
    /// The tailnet does not say it is the owner's.
    #[error("only the owner's own devices take the controls")]
    NotOwner,
    /// It has not joined.
    #[error("not joined")]
    NotJoined,
}

#[derive(Clone, Debug)]
struct Member {
    name: String,
    owner: bool,
}

/// Who is linked, and who holds the controls.
#[derive(Clone, Debug, Default)]
pub struct Seats {
    members: BTreeMap<ClientId, Member>,
    controller: Option<ClientId>,
}

impl Seats {
    /// `id` joined as `name`, the owner's when `owner`, asking for the controls when
    /// `control`. Whether the controller changed.
    pub fn join(&mut self, id: ClientId, name: String, owner: bool, control: bool) -> bool {
        self.members.insert(id, Member { name, owner });
        if control && owner {
            return self.controller.replace(id) != Some(id);
        }
        let lost = !owner && self.controller == Some(id);
        if lost {
            self.controller = None;
        }
        lost
    }

    /// `id` asks to take the controls, or to let go of them. Whether the controller changed.
    ///
    /// # Errors
    ///
    /// [`Refusal`] for a client not joined, or not the owner's when it asks to take them.
    pub fn take(&mut self, id: ClientId, take: bool) -> Result<bool, Refusal> {
        let member = self.members.get(&id).ok_or(Refusal::NotJoined)?;
        if !take {
            let held = self.controller == Some(id);
            if held {
                self.controller = None;
            }
            return Ok(held);
        }
        if !member.owner {
            return Err(Refusal::NotOwner);
        }
        Ok(self.controller.replace(id) != Some(id))
    }

    /// `id` left. Whether the controller changed.
    pub fn leave(&mut self, id: ClientId) -> bool {
        self.members.remove(&id);
        let held = self.controller == Some(id);
        if held {
            self.controller = None;
        }
        held
    }

    /// The client holding the controls.
    #[must_use]
    pub const fn controller(&self) -> Option<ClientId> {
        self.controller
    }

    /// Whether `id` holds the controls.
    #[must_use]
    pub fn controls(&self, id: ClientId) -> bool {
        self.controller == Some(id)
    }

    /// Clients joined.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether nobody joined.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The seat as `id` is told it.
    #[must_use]
    pub fn view(&self, id: ClientId) -> Seat {
        Seat {
            controller: self
                .controller
                .and_then(|c| self.members.get(&c))
                .map(|m| m.name.clone()),
            you: self.controls(id),
            clients: u32::try_from(self.members.len()).unwrap_or(u32::MAX),
        }
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn joining_with_control_takes_it_and_the_last_asker_wins() {
        let mut seats = Seats::default();
        assert!(seats.join(1, "MacBook".into(), true, true));
        assert!(!seats.join(2, "iPhone".into(), true, false), "a viewer");
        assert_eq!(
            seats.view(2),
            Seat {
                controller: Some("MacBook".into()),
                you: false,
                clients: 2
            }
        );
        assert_eq!(seats.take(2, true), Ok(true));
        assert!(seats.view(2).you);
        assert!(!seats.view(1).you);
        assert_eq!(seats.take(2, true), Ok(false), "already held");
        assert_eq!(
            seats.take(1, false),
            Ok(false),
            "1 holds nothing to let go of"
        );
        assert!(seats.leave(2), "leaving frees it");
        assert_eq!(seats.controller(), None, "nobody takes it until they ask");
    }

    #[test]
    fn only_the_owner_takes_the_controls() {
        let mut seats = Seats::default();
        assert!(!seats.join(1, "guest".into(), false, true));
        assert_eq!(seats.take(1, true), Err(Refusal::NotOwner));
        assert_eq!(seats.take(9, true), Err(Refusal::NotJoined));
        assert_eq!(seats.controller(), None);
    }

    #[derive(Clone, Debug)]
    enum Step {
        Join { id: u8, owner: bool, control: bool },
        Take { id: u8, take: bool },
        Leave { id: u8 },
    }

    fn step() -> impl Strategy<Value = Step> {
        prop_oneof![
            (0_u8..4, any::<bool>(), any::<bool>()).prop_map(|(id, owner, control)| Step::Join {
                id,
                owner,
                control
            }),
            (0_u8..4, any::<bool>()).prop_map(|(id, take)| Step::Take { id, take }),
            (0_u8..4).prop_map(|id| Step::Leave { id }),
        ]
    }

    proptest! {
        /// Against a model: the controller is always a joined owner, and is the last owner to
        /// ask who has neither let go nor left.
        #[test]
        fn the_seat_follows_the_last_owner_to_ask(steps in proptest::collection::vec(step(), 0..64)) {
            let mut seats = Seats::default();
            let mut joined: BTreeMap<ClientId, bool> = BTreeMap::new();
            let mut model: Option<ClientId> = None;
            for step in steps {
                let before = seats.controller();
                let changed = match step {
                    Step::Join { id, owner, control } => {
                        let id = ClientId::from(id);
                        joined.insert(id, owner);
                        if control && owner {
                            model = Some(id);
                        } else if !owner && model == Some(id) {
                            model = None;
                        }
                        seats.join(id, format!("client {id}"), owner, control)
                    }
                    Step::Take { id, take } => {
                        let id = ClientId::from(id);
                        let answer = seats.take(id, take);
                        match joined.get(&id) {
                            None => prop_assert_eq!(answer, Err(Refusal::NotJoined)),
                            Some(false) if take => prop_assert_eq!(answer, Err(Refusal::NotOwner)),
                            Some(_) if take => model = Some(id),
                            Some(_) => {
                                if model == Some(id) {
                                    model = None;
                                }
                            }
                        }
                        answer.unwrap_or(false)
                    }
                    Step::Leave { id } => {
                        let id = ClientId::from(id);
                        joined.remove(&id);
                        if model == Some(id) {
                            model = None;
                        }
                        seats.leave(id)
                    }
                };
                prop_assert_eq!(seats.controller(), model);
                prop_assert_eq!(changed, before != model);
                if let Some(c) = model {
                    prop_assert_eq!(joined.get(&c), Some(&true));
                }
                prop_assert_eq!(seats.len(), joined.len());
                let you: Vec<bool> = joined.keys().map(|&id| seats.view(id).you).collect();
                prop_assert!(you.iter().filter(|&&y| y).count() <= 1);
            }
        }
    }
}
