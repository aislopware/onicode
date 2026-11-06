//! The grants and System Settings as this Mac has them. Reading TCC and the window list is gated
//! by `ONICODE_GRANTS_LIVE=1`; nothing here requests a grant, opens System Settings, shows the
//! panel or takes focus, since the owner works on this Mac through Parsec.

#![cfg(target_os = "macos")]

use std::path::Path;

use onicode_desktop_grants::{Grants, guide, probe_with};

fn live() -> bool {
    let on = std::env::var_os("ONICODE_GRANTS_LIVE").is_some_and(|value| value == "1");
    if !on {
        onicode_desktop_testkit::live::skip("set ONICODE_GRANTS_LIVE=1");
    }
    on
}

/// The preflights answer without a prompt, and what they answer survives the probe's line.
#[test]
fn this_process_grants_read_back_through_the_line() {
    if !live() {
        return;
    }
    let grants = Grants::current();
    assert_eq!(Grants::parse_line(&grants.to_line()), Ok(grants));
    // Whatever a new process sees on top, the report never loses what this one has.
    let fresh = Grants::fresh();
    assert_eq!(fresh.union(grants), fresh);
}

/// System Settings' windows, when it runs, have the bounds the panel follows; the list needs no
/// Screen Recording.
#[test]
fn settings_windows_have_bounds() {
    if !live() {
        return;
    }
    for window in guide::settings_windows() {
        assert!(window.bounds.w > 0.0 && window.bounds.h > 0.0, "{window:?}");
    }
}

/// A program that does not answer the probe is refused rather than read as "nothing granted".
#[test]
fn a_program_that_does_not_answer_is_refused() {
    let silent = probe_with(Path::new("/usr/bin/true"));
    assert!(silent.is_err(), "{silent:?}");
    // `echo` answers with the argument itself, which is no grants line.
    let echo = probe_with(Path::new("/bin/echo"));
    assert!(echo.is_err(), "{echo:?}");
    let failing = probe_with(Path::new("/usr/bin/false"));
    assert!(failing.is_err(), "{failing:?}");
}
