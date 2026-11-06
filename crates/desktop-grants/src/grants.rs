//! The grants themselves and the one line a probe prints them as.

use serde::{Deserialize, Serialize};

/// The prefix of a probe's line, with its version: a desktopd from another build that answers
/// in another shape is refused rather than misread.
const LINE_TAG: &str = "onicode-grants/1";

/// The deep link's base: System Settings' Privacy & Security extension on macOS 27.
const SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension";

/// A System Settings privacy list desktopd needs to be in.
///
/// Posting events has no list of its own: `CGPreflightPostEventAccess` is granted with
/// Accessibility, so [`Pane::Accessibility`] covers both [`Grants::accessibility`] and
/// [`Grants::post_event`]. macOS 27's separate "Remote Desktop" list is not a pane here: it has no
/// deep-link anchor and no public API reads it (`docs/onicode/decisions/desktop-grants.md`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Pane {
    /// Screen & System Audio Recording: capture.
    ScreenRecording,
    /// Accessibility: posting input, the focused field, the cursor's picture.
    Accessibility,
}

impl Pane {
    /// Every pane, Screen Recording first, the order the guide asks for them.
    pub const ALL: [Self; 2] = [Self::ScreenRecording, Self::Accessibility];

    /// The anchor System Settings' Privacy & Security extension opens the list at.
    #[must_use]
    pub const fn anchor(self) -> &'static str {
        match self {
            Self::ScreenRecording => "Privacy_ScreenCapture",
            Self::Accessibility => "Privacy_Accessibility",
        }
    }

    /// The `x-apple.systempreferences:` link that opens this list.
    #[must_use]
    pub fn settings_url(self) -> String {
        format!("{SETTINGS_URL}?{}", self.anchor())
    }

    /// The list's name as System Settings shows it, for the guide's words.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ScreenRecording => "Screen Recording",
            Self::Accessibility => "Accessibility",
        }
    }

    /// Whether a running process sees this grant only after it restarts. Screen Recording is read
    /// once per process; Accessibility is live.
    #[must_use]
    pub const fn needs_restart(self) -> bool {
        matches!(self, Self::ScreenRecording)
    }
}

/// What a process may do, read with the preflights, which never prompt.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Grants {
    /// Screen Recording (`CGPreflightScreenCaptureAccess`).
    pub screen_recording: bool,
    /// Accessibility (`AXIsProcessTrusted`).
    pub accessibility: bool,
    /// Posting events (`CGPreflightPostEventAccess`), granted with Accessibility.
    pub post_event: bool,
}

impl Grants {
    /// Everything on.
    pub const ALL: Self = Self {
        screen_recording: true,
        accessibility: true,
        post_event: true,
    };

    /// Whether `pane` is on: Accessibility counts only with posting events.
    #[must_use]
    pub const fn has(self, pane: Pane) -> bool {
        match pane {
            Pane::ScreenRecording => self.screen_recording,
            Pane::Accessibility => self.accessibility && self.post_event,
        }
    }

    /// The panes still off, Screen Recording first.
    #[must_use]
    pub fn missing(self) -> Vec<Pane> {
        Pane::ALL
            .into_iter()
            .filter(|&pane| !self.has(pane))
            .collect()
    }

    /// Whether desktopd may stream and take input.
    #[must_use]
    pub const fn complete(self) -> bool {
        self.screen_recording && self.accessibility && self.post_event
    }

    /// Each grant either side has.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            screen_recording: self.screen_recording || other.screen_recording,
            accessibility: self.accessibility || other.accessibility,
            post_event: self.post_event || other.post_event,
        }
    }

    /// The probe's line: `onicode-grants/1 screen_recording=1 accessibility=0 post_event=1`.
    #[must_use]
    pub fn to_line(self) -> String {
        let bit = |on: bool| if on { '1' } else { '0' };
        format!(
            "{LINE_TAG} screen_recording={} accessibility={} post_event={}",
            bit(self.screen_recording),
            bit(self.accessibility),
            bit(self.post_event),
        )
    }

    /// Reads [`Self::to_line`] back, surrounding whitespace allowed and nothing else.
    ///
    /// # Errors
    ///
    /// [`ParseError`] when the tag, a key, a value or the count of fields is not the probe's.
    pub fn parse_line(line: &str) -> Result<Self, ParseError> {
        let mut words = line.split_ascii_whitespace();
        if words.next() != Some(LINE_TAG) {
            return Err(ParseError::Tag);
        }
        let mut field = |key: &'static str| -> Result<bool, ParseError> {
            let word = words.next().ok_or(ParseError::Missing(key))?;
            let value = word
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix('='))
                .ok_or(ParseError::Missing(key))?;
            match value {
                "1" => Ok(true),
                "0" => Ok(false),
                _ => Err(ParseError::Value(key)),
            }
        };
        let grants = Self {
            screen_recording: field("screen_recording")?,
            accessibility: field("accessibility")?,
            post_event: field("post_event")?,
        };
        if words.next().is_some() {
            return Err(ParseError::Trailing);
        }
        Ok(grants)
    }
}

/// Why a probe's line was refused.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum ParseError {
    /// It does not start with this build's tag.
    #[error("not a grants line from this build")]
    Tag,
    /// A field is missing or out of order.
    #[error("the grants line has no {0}")]
    Missing(&'static str),
    /// A field is neither `0` nor `1`.
    #[error("the grants line's {0} is neither 0 nor 1")]
    Value(&'static str),
    /// Something follows the last field.
    #[error("the grants line goes on past its fields")]
    Trailing,
}

/// Whether a running process that reads `live` should restart to take up what a fresh process
/// reads (`fresh`): a grant that is read once per process came on after this one started.
/// Accessibility is live, so it never asks for a restart on its own.
#[must_use]
pub const fn needs_restart(live: Grants, fresh: Grants) -> bool {
    (fresh.screen_recording && !live.screen_recording) || (fresh.post_event && !live.post_event)
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use proptest::prelude::*;

    use super::*;

    fn grants() -> impl Strategy<Value = Grants> {
        (any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
            |(screen_recording, accessibility, post_event)| Grants {
                screen_recording,
                accessibility,
                post_event,
            },
        )
    }

    #[test]
    fn the_line_is_pinned() {
        let grants = Grants {
            screen_recording: true,
            accessibility: false,
            post_event: true,
        };
        assert_eq!(
            grants.to_line(),
            "onicode-grants/1 screen_recording=1 accessibility=0 post_event=1"
        );
    }

    #[test]
    fn a_foreign_or_broken_line_is_refused() {
        assert_eq!(Grants::parse_line(""), Err(ParseError::Tag));
        assert_eq!(
            Grants::parse_line("onicode-grants/2 screen_recording=1 accessibility=1 post_event=1"),
            Err(ParseError::Tag)
        );
        assert_eq!(
            Grants::parse_line("onicode-grants/1 accessibility=1 screen_recording=1 post_event=1"),
            Err(ParseError::Missing("screen_recording"))
        );
        assert_eq!(
            Grants::parse_line(
                "onicode-grants/1 screen_recording=yes accessibility=1 post_event=1"
            ),
            Err(ParseError::Value("screen_recording"))
        );
        assert_eq!(
            Grants::parse_line("onicode-grants/1 screen_recording=1 accessibility=1"),
            Err(ParseError::Missing("post_event"))
        );
        assert_eq!(
            Grants::parse_line(
                "onicode-grants/1 screen_recording=1 accessibility=1 post_event=1 x"
            ),
            Err(ParseError::Trailing)
        );
        assert_eq!(
            Grants::parse_line("onicode-grants/1 screen_recording=11 accessibility=1 post_event=1"),
            Err(ParseError::Value("screen_recording"))
        );
    }

    #[test]
    fn panes_link_to_their_lists() {
        assert_eq!(
            Pane::ScreenRecording.settings_url(),
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture"
        );
        assert_eq!(
            Pane::Accessibility.settings_url(),
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_Accessibility"
        );
    }

    #[test]
    fn accessibility_counts_only_with_posting_events() {
        let no_post = Grants {
            screen_recording: true,
            accessibility: true,
            post_event: false,
        };
        assert_eq!(no_post.missing(), vec![Pane::Accessibility]);
        assert_eq!(Grants::default().missing(), Pane::ALL.to_vec());
        assert!(Grants::ALL.missing().is_empty());
    }

    proptest! {
        #[test]
        fn lines_read_back(grants in grants(), pad in "[ \t]{0,3}") {
            let line = format!("{pad}{}{pad}\n", grants.to_line());
            prop_assert_eq!(Grants::parse_line(&line), Ok(grants));
        }

        #[test]
        fn parsing_never_panics(line in ".{0,120}") {
            let _ = Grants::parse_line(&line);
        }

        #[test]
        fn complete_means_nothing_missing(grants in grants()) {
            prop_assert_eq!(grants.complete(), grants.missing().is_empty());
        }

        #[test]
        fn a_restart_only_gains(live in grants(), fresh in grants()) {
            // Nothing fresh lacks restarts, and a restart is always for something new.
            if needs_restart(live, fresh) {
                prop_assert_ne!(live.union(fresh), live);
            }
            prop_assert!(!needs_restart(live, live));
            prop_assert!(!needs_restart(live.union(fresh), fresh));
        }

        #[test]
        fn accessibility_alone_never_restarts(live in grants()) {
            let fresh = Grants { accessibility: true, ..live };
            prop_assert!(!needs_restart(live, fresh));
        }
    }
}
