//! Keep-awake against its model: seeded loads on an injected clock, through a fake assertion
//! that counts what was taken. Never more than one assertion; held whenever the setting allows it
//! and anything works; let go only after [`LINGER_MS`] with nothing working, or at once when the
//! setting turns off; a refused assertion is asked for again. No test touches the machine's power.
use proptest::prelude::*;

use super::{Keeper, LINGER_MS, Load, PowerAssertion, REASON};

/// Counts takes and releases, and refuses when told.
#[derive(Default)]
struct Fake {
    held: usize,
    takes: usize,
    releases: usize,
    refuse: bool,
}

impl PowerAssertion for Fake {
    fn take(&mut self, reason: &str) -> bool {
        assert_eq!(reason, REASON);
        if self.refuse {
            return false;
        }
        self.held += 1;
        self.takes += 1;
        true
    }

    fn release(&mut self) {
        if self.held > 0 {
            self.held -= 1;
            self.releases += 1;
        }
    }
}

#[test]
fn back_to_back_turns_hold_one_assertion() {
    let mut keeper = Keeper::new(Fake::default());
    let busy = |turns| Load {
        allowed: true,
        turns,
        commands: 0,
    };
    keeper.apply(busy(1), 0);
    keeper.apply(busy(2), 100);
    keeper.apply(busy(0), 1_000);
    keeper.apply(busy(1), 1_000 + LINGER_MS - 1);
    keeper.apply(busy(0), 20_000);
    assert_eq!(keeper.deadline(), Some(20_000 + LINGER_MS));
    keeper.apply(busy(0), 20_000 + LINGER_MS - 1);
    assert_eq!((keeper.power().takes, keeper.power().held), (1, 1));
    keeper.apply(busy(0), 20_000 + LINGER_MS);
    assert_eq!((keeper.power().takes, keeper.power().releases), (1, 1));
    assert!(!keeper.held());
}

#[test]
fn a_refused_assertion_is_asked_for_again() {
    let mut keeper = Keeper::new(Fake {
        refuse: true,
        ..Fake::default()
    });
    let load = Load {
        allowed: true,
        turns: 0,
        commands: 1,
    };
    keeper.apply(load, 0);
    assert!(!keeper.held());
    keeper.apply(load, 5_000);
    assert_eq!(keeper.power().takes, 0);
}

/// One look: time moves by `dt`, and the load is what it is then.
#[derive(Clone, Copy, Debug)]
struct Look {
    dt: u64,
    allowed: bool,
    turns: usize,
    commands: usize,
}

fn look() -> impl Strategy<Value = Look> {
    (
        0u64..15_000,
        prop::bool::weighted(0.9),
        0usize..3,
        0usize..3,
    )
        .prop_map(|(dt, allowed, turns, commands)| Look {
            dt,
            allowed,
            turns,
            commands,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn the_assertion_follows_the_work(looks in proptest::collection::vec(look(), 1..60)) {
        let mut keeper = Keeper::new(Fake::default());
        let mut now = 0u64;
        // When the work last stopped while the setting allowed it.
        let mut idle_since: Option<u64> = None;
        let mut held = false;
        for look in looks {
            now += look.dt;
            let load = Load {
                allowed: look.allowed,
                turns: look.turns,
                commands: look.commands,
            };
            keeper.apply(load, now);
            let busy = look.turns + look.commands > 0;
            // The model: held while allowed and busy; after the work stops, held until the
            // linger passes; never while not allowed.
            if !look.allowed {
                held = false;
                idle_since = None;
            } else if busy {
                held = true;
                idle_since = None;
            } else if held {
                let since = *idle_since.get_or_insert(now);
                if now - since >= LINGER_MS {
                    held = false;
                    idle_since = None;
                }
            }
            prop_assert!(keeper.power().held <= 1, "two assertions at once");
            prop_assert_eq!(keeper.held(), held);
            prop_assert_eq!(keeper.power().held == 1, held);
        }
    }
}
