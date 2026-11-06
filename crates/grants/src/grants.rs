//! The helper's grants and the one line its probe prints them as.

use serde::{Deserialize, Serialize};

use crate::Privilege;

/// The prefix of a probe's line, with its version: a desktopd from another build that answers
/// in another shape is refused rather than misread.
const LINE_TAG: &str = "onicode-grants/1";

/// What the helper may do, read with the preflights, which never prompt: its rows of the
/// checklist ([`Privilege::HELPER`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub struct Grants {
    /// Screen Recording (`CGPreflightScreenCaptureAccess`).
    pub screen_recording: bool,
    /// Accessibility (`AXIsProcessTrusted`).
    pub accessibility: bool,
    /// Posting events (`CGPreflightPostEventAccess`), listed under Accessibility.
    pub post_event: bool,
}

impl Grants {
    /// Everything on.
    pub const ALL: Self = Self {
        screen_recording: true,
        accessibility: true,
        post_event: true,
    };

    /// Whether `privilege` is on; `None` for a privilege that is not the helper's.
    #[must_use]
    pub const fn get(self, privilege: Privilege) -> Option<bool> {
        match privilege {
            Privilege::ScreenRecording => Some(self.screen_recording),
            Privilege::Accessibility => Some(self.accessibility),
            Privilege::PostEvent => Some(self.post_event),
            _ => None,
        }
    }

    /// The helper's privileges still off, in the checklist's order.
    #[must_use]
    pub fn missing(self) -> Vec<Privilege> {
        Privilege::HELPER
            .into_iter()
            .filter(|&privilege| self.get(privilege) == Some(false))
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
    fn missing_names_each_privilege_off() {
        let no_post = Grants {
            screen_recording: true,
            accessibility: true,
            post_event: false,
        };
        assert_eq!(no_post.missing(), vec![Privilege::PostEvent]);
        assert_eq!(Grants::default().missing(), Privilege::HELPER.to_vec());
        assert!(Grants::ALL.missing().is_empty());
        assert_eq!(Grants::ALL.get(Privilege::Microphone), None);
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
