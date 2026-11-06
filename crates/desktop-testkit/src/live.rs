//! How a live test that cannot run here says so.
//!
//! One line, `skipped: <why>`, and the test returns. In a VM live lane (which sets [`IN_VM`]) a
//! guest lacks nothing a live test needs, so there the same call fails the test instead of
//! letting it pass unrun.
//!
//! Tests that touch a device or a grant are opt-in, each kind behind its own variable
//! ([`opted_in`]): they never prompt, play sound, move the pointer or rearrange displays on a
//! machine that did not ask for it, and hosted CI runners (whose paravirtual encoder hangs a
//! process) never run them.

/// The variable a VM live lane sets in its guest.
pub const IN_VM: &str = "ONICODE_VM";

/// `1` runs the tests that open VideoToolbox sessions: real hardware encode and decode of
/// synthetic pictures. No grant, no prompt, but hosted runners hang in it.
pub const VT_TESTS: &str = "ONICODE_VT_TESTS";
/// `1` runs the tests that open the machine's audio output (silence only).
pub const AUDIO_LIVE: &str = "ONICODE_AUDIO_LIVE";
/// `1` runs the tests that open ScreenCaptureKit or read the window server's cursor.
pub const DESKTOP_LIVE: &str = "ONICODE_DESKTOP_LIVE";
/// `1` runs the tests that post input events or change the input source.
pub const INPUT_LIVE: &str = "ONICODE_INPUT_LIVE";
/// `1` runs the tests that create virtual displays.
pub const VDISPLAY_LIVE: &str = "ONICODE_VDISPLAY_LIVE";
/// `1` runs the tests that write named pasteboards (never the general one, never a prompt).
pub const PASTEBOARD_LIVE: &str = "ONICODE_PASTEBOARD_LIVE";

/// Whether the opt-in tests behind `var` run here (`var` set to `1`). When they do not, says so
/// once on standard error, naming the variable, and the caller returns.
#[track_caller]
#[must_use]
pub fn opted_in(var: &str) -> bool {
    let on = std::env::var_os(var).is_some_and(|v| v == "1");
    if !on {
        skip_unasked(var);
    }
    on
}

#[track_caller]
#[expect(
    clippy::print_stderr,
    reason = "a test helper: the skip is said where the test's output is"
)]
fn skip_unasked(var: &str) {
    eprintln!("skipped: opt-in, set {var}=1 to run it");
}

/// Say on standard error that the calling test is not running, and why; the caller returns next.
///
/// # Panics
///
/// In the VM live lane ([`IN_VM`] set), where a skip is a failure.
#[track_caller]
pub fn skip(why: &str) {
    skip_in(std::env::var_os(IN_VM).is_some(), why);
}

#[track_caller]
#[expect(
    clippy::print_stderr,
    reason = "a test helper: the skip is said where the test's output is"
)]
fn skip_in(in_vm: bool, why: &str) {
    assert!(
        !in_vm,
        "skipped in the VM live lane ({IN_VM} is set), where nothing is missing: {why}"
    );
    eprintln!("skipped: {why}");
}

#[cfg(test)]
mod tests {
    use super::skip_in;

    #[test]
    fn a_skip_outside_the_vm_lane_returns() {
        skip_in(false, "nothing to see");
    }

    #[test]
    #[should_panic(
        expected = "skipped in the VM live lane (ONICODE_VM is set), where nothing is missing: fish"
    )]
    fn a_skip_in_the_vm_lane_fails_the_test() {
        skip_in(true, "fish");
    }
}
