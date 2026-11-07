//! A project's appearance: its color, mascot, theme and background settings, and the fade a
//! background takes. Reading and rendering the image is the viewer's.
use std::hash::Hash;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const COLORS: [u32; 9] = [
    0x8b949c, 0x459cf7, 0xea603e, 0xf4bf25, 0x39c363, 0xe45a9f, 0xb168d4, 0x37beb4, 0xef8c39,
];
pub const MASCOTS: [&str; 10] = [
    "invader", "ghost", "robot", "cat", "skull", "crab", "mushroom", "rocket", "dino", "frog",
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectAppearance {
    pub name: String,
    pub color: Option<u32>,
    pub mascot: Option<String>,
    pub logo: Option<String>,
    pub background: Option<Background>,
    /// Colors this project uses instead of the app-wide Appearance settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<ProjectTheme>,
}
impl ProjectAppearance {
    pub fn label(&self, project: &str) -> String {
        if self.name.trim().is_empty() {
            Path::new(project)
                .file_name()
                .map_or_else(|| project.into(), |s| s.to_string_lossy().into_owned())
        } else {
            self.name.clone()
        }
    }

    pub fn color(&self, project: &str) -> u32 {
        self.color.filter(|c| *c <= 0xffffff).unwrap_or_else(|| {
            let hash = project
                .encode_utf16()
                .fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(c.into()));
            COLORS[(hash as usize % (COLORS.len() - 1)) + 1]
        })
    }

    pub fn mascot(&self, project: &str) -> usize {
        self.mascot
            .as_ref()
            .and_then(|name| MASCOTS.iter().position(|m| *m == name))
            .unwrap_or_else(|| {
                let hash = project
                    .encode_utf16()
                    .fold(0u32, |h, c| h.wrapping_mul(131).wrapping_add(c.into()));
                hash as usize % MASCOTS.len()
            })
    }

    pub fn normalize(&mut self) {
        self.name = self.name.trim().chars().take(200).collect();
        self.color = self.color.filter(|c| *c <= 0xffffff);
        self.mascot = self.mascot.take().filter(|s| MASCOTS.contains(&s.as_str()));
        if let Some(background) = &mut self.background {
            background.normalize();
        }
        self.background = self.background.take().filter(|b| !b.path.trim().is_empty());
        if let Some(theme) = &mut self.theme {
            theme.normalize();
        }
    }
}

/// A project's own copy of the Appearance page's color settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectTheme {
    pub workspace_theme: String,
    /// `#rrggbb`, or `None` for the workspace theme's own accent.
    pub accent_color: Option<String>,
    pub hue: u32,
    pub saturation: u32,
    pub dark_lightness: u32,
}
impl Default for ProjectTheme {
    fn default() -> Self {
        Self {
            workspace_theme: "default".into(),
            accent_color: None,
            hue: 240,
            saturation: 0,
            dark_lightness: 9,
        }
    }
}
impl ProjectTheme {
    pub fn normalize(&mut self) {
        self.hue = self.hue.min(360);
        self.saturation = self.saturation.min(100);
        self.dark_lightness = self.dark_lightness.min(30);
        self.accent_color = self.accent_color.take().filter(|hex| {
            hex.len() == 7
                && hex
                    .strip_prefix('#')
                    .is_some_and(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
        });
        if self.workspace_theme.trim().is_empty() {
            self.workspace_theme = "default".into();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    #[default]
    None,
    Dither,
    Ascii,
    Halftone,
    Scanlines,
    GradientBlur,
}
impl Effect {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Dither,
        Self::Ascii,
        Self::Halftone,
        Self::Scanlines,
        Self::GradientBlur,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Dither => "Dither",
            Self::Ascii => "ASCII",
            Self::Halftone => "Halftone",
            Self::Scanlines => "Scanlines",
            Self::GradientBlur => "Haze",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::None => "Shows the original artwork.",
            Self::Dither => "Rebuilds the artwork with a dithered color palette.",
            Self::Ascii => "Recreates the artwork with colored characters on black.",
            Self::Halftone => "Recreates the artwork with colored print dots on black.",
            Self::Scanlines => "Adds a pronounced horizontal display-line texture.",
            Self::GradientBlur => "Blurs and fades the artwork into the background below.",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Empty,
    #[default]
    All,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Background {
    pub path: String,
    pub effect: Effect,
    pub scope: Scope,
    pub empty_opacity: f32,
    pub session_opacity: f32,
}
impl Default for Background {
    fn default() -> Self {
        Self {
            path: String::new(),
            effect: Effect::None,
            scope: Scope::All,
            empty_opacity: 0.24,
            session_opacity: 0.24,
        }
    }
}
impl Background {
    pub fn normalize(&mut self) {
        for value in [&mut self.empty_opacity, &mut self.session_opacity] {
            *value = if value.is_finite() {
                value.clamp(0.05, 0.65)
            } else {
                0.24
            };
        }
    }

    pub fn opacity(&self, empty: bool) -> f32 {
        if !empty && self.scope == Scope::Empty {
            0.
        } else if empty {
            self.empty_opacity
        } else {
            self.session_opacity
        }
    }
}

pub fn background_fade(y: f32, empty: bool, haze: bool) -> f32 {
    let stops: &[(f32, f32)] = match (empty, haze) {
        (true, true) => &[
            (0., 1.),
            (0.19, 1.),
            (0.30, 0.94),
            (0.42, 0.72),
            (0.54, 0.42),
            (0.66, 0.16),
            (0.78, 0.04),
            (0.90, 0.),
        ],
        (false, true) => &[(0., 1.), (0.18, 1.), (0.76, 0.)],
        (true, false) => &[
            (0., 1.),
            (0.19, 1.),
            (0.30, 0.79),
            (0.42, 0.50),
            (0.54, 0.20),
            (0.64, 0.05),
            (0.72, 0.),
        ],
        (false, false) => &[
            (0., 1.),
            (0.18, 1.),
            (0.32, 0.61),
            (0.47, 0.32),
            (0.62, 0.11),
            (0.72, 0.),
        ],
    };
    for pair in stops.windows(2) {
        if y <= pair[1].0 {
            let t = ((y - pair[0].0) / (pair[1].0 - pair[0].0)).clamp(0., 1.);
            return pair[0].1 + (pair[1].1 - pair[0].1) * t;
        }
    }
    0.
}
