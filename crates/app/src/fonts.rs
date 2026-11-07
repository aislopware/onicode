use std::sync::OnceLock;

use gpui::{App, Font, FontFallbacks};

/// GPUI resolves this family to the platform system UI font.
pub(crate) const UI: &str = ".SystemUIFont";

/// The monospace family for code and the terminal.
pub(crate) fn mono() -> &'static str {
    "Menlo"
}

/// The terminal's lead faces, as the web build's `--font-terminal`: a Nerd
/// Font patched JetBrains Mono keeps prompt text and icons in one face.
const TERMINAL_PRIMARY: &[&str] = &[
    "JetBrainsMono NFM",
    "JetBrainsMono Nerd Font Mono",
    "JetBrainsMono NF",
];
/// Fill the Private Use Area glyphs (powerline arrows, git and folder icons)
/// a plain monospace lacks; the system cascade never reaches them.
const TERMINAL_ICONS: &[&str] = &[
    "Symbols Nerd Font Mono",
    "Symbols Nerd Font",
    "MesloLGS NF",
    "MesloLGS Nerd Font Mono",
    "FiraCode Nerd Font Mono",
    "Hack Nerd Font Mono",
    "CaskaydiaCove Nerd Font Mono",
];

static TERMINAL: OnceLock<Font> = OnceLock::new();

/// The terminal's font: a Nerd Font when installed, else the code monospace,
/// with any installed Nerd Font as the fallback for icon glyphs.
pub(crate) fn terminal(cx: &App) -> Font {
    TERMINAL
        .get_or_init(|| terminal_font(&cx.text_system().all_font_names(), mono()))
        .clone()
}

fn terminal_font(installed: &[String], mono: &str) -> Font {
    let has = |name: &str| installed.iter().any(|font| font == name);
    let family = TERMINAL_PRIMARY
        .iter()
        .copied()
        .find(|name| has(name))
        .unwrap_or(mono);
    let icons: Vec<String> = TERMINAL_ICONS
        .iter()
        .filter(|name| has(name) && **name != family)
        .map(|name| name.to_string())
        .collect();
    let mut font = gpui::font(family.to_owned());
    font.fallbacks = (!icons.is_empty()).then(|| FontFallbacks::from_fonts(icons));
    font
}

#[cfg(test)]
mod tests {
    use super::terminal_font;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn terminal_leads_with_a_nerd_font_and_falls_back_for_icons() {
        let installed = names(&["Menlo", "JetBrainsMono NFM", "Symbols Nerd Font Mono"]);
        let font = terminal_font(&installed, "Menlo");
        assert_eq!(font.family.as_ref(), "JetBrainsMono NFM");
        assert_eq!(
            font.fallbacks.unwrap().fallback_list(),
            ["Symbols Nerd Font Mono"]
        );

        let installed = names(&["Menlo", "MesloLGS NF", "Hack Nerd Font Mono"]);
        let font = terminal_font(&installed, "Menlo");
        assert_eq!(font.family.as_ref(), "Menlo");
        assert_eq!(
            font.fallbacks.unwrap().fallback_list(),
            ["MesloLGS NF", "Hack Nerd Font Mono"]
        );

        let font = terminal_font(&names(&["Menlo"]), "Menlo");
        assert_eq!(font.family.as_ref(), "Menlo");
        assert!(font.fallbacks.is_none());
    }
}
