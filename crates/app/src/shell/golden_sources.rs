//! The fixed text the code scenes of `golden_tests` draw: a Rust file for the editor and the diff,
//! a Markdown change for the diff's second file, and a run of terminal output. ASCII only, so every
//! glyph comes from Menlo or the system UI font.
use std::ops::Range;

/// The editor's file, and the newer side of the diff's first file. Line 27's doc comment runs past
/// the editor's width, so it wraps there and scrolls sideways in the diff.
pub(super) const EDGE_FADE: &str = r#"//! The transcript's edge fade: a linear ramp at each edge of the list,
//! drawn only once there is content past that edge.
use std::ops::Range;

/// How tall the top ramp is, in points.
pub const TOP_FADE: f32 = 24.;
/// The bottom ramp stops at the composer's shadow.
pub const BOTTOM_FADE: f32 = 40.;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EdgeFade {
    pub top: f32,
    pub bottom: f32,
}

impl EdgeFade {
    /// The fade for a list scrolled `offset` points into `content`, seen through `viewport`.
    pub fn new(offset: f32, content: f32, viewport: f32) -> Self {
        let past_top = offset.max(0.);
        let past_bottom = (content - viewport - offset).max(0.);
        Self {
            top: (past_top / TOP_FADE).min(1.),
            bottom: (past_bottom / BOTTOM_FADE).min(1.),
        }
    }

    /// Opacity at `y`, mixing both ramps with `min` and never their product, so a short list that fades at both of its edges keeps its middle at full strength.
    pub fn alpha(&self, y: f32, height: f32) -> f32 {
        let top = if self.top > 0. { (y / TOP_FADE).clamp(0., 1.) } else { 1. };
        let bottom = if self.bottom > 0. {
            ((height - y) / BOTTOM_FADE).clamp(0., 1.)
        } else {
            1.
        };
        top.min(bottom)
    }

    /// Rows that need the snapshot pass: the ones under either ramp.
    pub fn rows(&self, height: f32, row: f32) -> Range<usize> {
        let first = (TOP_FADE * self.top / row).ceil() as usize;
        let last = ((height - BOTTOM_FADE * self.bottom) / row).floor() as usize;
        first..last.max(first)
    }

    pub fn is_flat(&self) -> bool {
        self.top == 0. && self.bottom == 0.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_at_rest_keeps_a_crisp_lower_edge() {
        let fade = EdgeFade::new(0., 400., 600.);
        assert!(fade.is_flat());
        assert_eq!(fade.alpha(590., 600.), 1.);
    }

    #[test]
    fn both_edges_fade_mid_scroll() {
        let fade = EdgeFade::new(120., 2_000., 600.);
        assert_eq!((fade.top, fade.bottom), (1., 1.));
        assert_eq!(fade.alpha(12., 600.), 0.5);
        // Painted before and after the list.
        let marker = "edge-fade-marker";
        assert_eq!(marker.len(), 16);
    }
}
"#;

/// The older side of the diff's first file: a shorter top ramp, the ramps multiplied, no
/// `is_flat` and two fewer checks in the tests, so the diff holds several hunks of additions,
/// deletions and changed lines.
pub(super) fn edge_fade_before() -> String {
    let is_flat = "    pub fn is_flat(&self) -> bool {\n        \
                   self.top == 0. && self.bottom == 0.\n    }\n";
    let marker = "        // Painted before and after the list.\n        \
                  let marker = \"edge-fade-marker\";\n        \
                  assert_eq!(marker.len(), 16);\n";
    EDGE_FADE
        .replace("= 24.;", "= 16.;")
        .replace("with `min` and never their product", "by multiplying them")
        .replace("        top.min(bottom)\n", "        top * bottom\n")
        .replace(is_flat, "")
        .replace("        assert!(fade.is_flat());\n", "")
        .replace(marker, "")
}

/// The diff's second file, before and after.
pub(super) const GOLDEN_DOC: (&str, &str) = (
    "# Golden images\n\n\
     The scenes, light and dark:\n\n\
     - shell\n\
     - menu\n\
     - modal\n\
     - toast\n\n\
     Re-record only for an intended UI change.\n",
    "# Golden images\n\n\
     The scenes, light and dark, each pinned before the widget migration:\n\n\
     - shell\n\
     - menu\n\
     - modal\n\
     - toast\n\
     - editor\n\
     - diff\n\
     - terminal\n\n\
     Re-record only for an intended UI change.\n",
);

/// The byte range of lines `first..=last` (1-based), without the last line's newline.
pub(super) fn lines(text: &str, first: usize, last: usize) -> Range<usize> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect();
    let start = starts[first - 1];
    let end = starts.get(last).map_or(text.len(), |next| next - 1);
    start..end
}

/// A test run's output in the terminal: bold and coloured status words, a warning with a dim
/// location, every text attribute, the 16 colours as foreground and background, a 256-colour grey
/// ramp, a truecolor line, and a prompt the cursor waits at.
pub(super) fn terminal_output() -> String {
    let mut out = String::new();
    out.push_str("\x1b[1;32m$\x1b[0m cargo nextest run -E 'test(golden)'\r\n");
    out.push_str("\x1b[1;32m   Compiling\x1b[0m onicode v0.1.0 (/work/onicode/crates/app)\r\n");
    out.push_str(
        "\x1b[1;32m    Finished\x1b[0m `test` profile [unoptimized + debuginfo] in 41.07s\r\n",
    );
    out.push_str("\x1b[1;32m        PASS\x1b[0m [   2.118s] onicode golden_tests::editor\r\n");
    out.push_str("\x1b[1;31m        FAIL\x1b[0m [   1.904s] onicode golden_tests::diff\r\n");
    out.push_str("\x1b[1;33mwarning\x1b[0m\x1b[1m: unused variable: `look`\x1b[0m\r\n");
    out.push_str("\x1b[2m  --> crates/app/src/shell/golden_tests.rs:42:9\x1b[0m\r\n\r\n");
    out.push_str(
        "plain \x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \
         \x1b[4munderline\x1b[0m \x1b[9mstrike\x1b[0m \x1b[7minverse\x1b[0m\r\n",
    );
    for base in [30, 90] {
        for n in 0..8 {
            out.push_str(&format!("\x1b[{}mcol{n} \x1b[0m", base + n));
        }
        out.push_str("\r\n");
    }
    for base in [40, 100] {
        for n in 0..8 {
            out.push_str(&format!("\x1b[{}m     \x1b[0m", base + n));
        }
        out.push_str("\r\n");
    }
    for grey in 232..=255 {
        out.push_str(&format!("\x1b[48;5;{grey}m  "));
    }
    out.push_str("\x1b[0m\r\n");
    for (r, g, b, name) in [
        (232, 181, 131, "truecolor peach"),
        (111, 132, 163, "slate"),
        (228, 127, 166, "pink"),
    ] {
        out.push_str(&format!("\x1b[38;2;{r};{g};{b}m{name}\x1b[0m "));
    }
    out.push_str("\r\n\r\n\x1b[1;32m$\x1b[0m ");
    out
}
