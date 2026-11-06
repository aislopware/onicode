//! Keystrokes from runtime strings. GPUI's `Keystroke::parse` and `KeyBinding::new` take only
//! `'static` text with explicit modifiers; settings, shortcuts and tests build keystrokes at
//! runtime and use the `secondary-` alias, which these resolve for the host.

use gpui::{InvalidKeystrokeError, Keystroke, OperatingSystem};

/// The operating system `secondary-` resolves against: the GUI runs on macOS.
pub const HOST_OS: OperatingSystem = if cfg!(target_os = "macos") {
    OperatingSystem::Mac
} else {
    OperatingSystem::Linux
};

/// Parse one keystroke such as `cmd-shift-p` or `secondary-k`.
///
/// # Errors
///
/// When the text is not a keystroke.
pub fn parse(source: &str) -> Result<Keystroke, InvalidKeystrokeError> {
    Keystroke::parse_with_os(source, HOST_OS)
}

/// A key binding from runtime text, such as a combo built with `format!` or read from
/// settings: `KeyBinding::new` takes only `'static` text.
///
/// # Panics
///
/// When `keystrokes` or `context` does not parse, as `KeyBinding::new` does: callers pass
/// combos built from literals or validated settings.
pub fn binding<A: gpui::Action>(
    cx: &gpui::App,
    keystrokes: &str,
    action: A,
    context: Option<&str>,
) -> gpui::KeyBinding {
    let predicate = context.map(|context| {
        std::rc::Rc::new(
            gpui::KeyBindingContextPredicate::parse(context)
                .unwrap_or_else(|error| panic!("key context {context:?}: {error}")),
        )
    });
    gpui::KeyBinding::load(keystrokes, Box::new(action), predicate, false, None, cx)
        .unwrap_or_else(|error| panic!("key binding {keystrokes:?}: {error}"))
}
