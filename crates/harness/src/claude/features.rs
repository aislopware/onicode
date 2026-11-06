//! What the installed Claude Code supports, by its own version.
//!
//! One table of `feature → oldest CLI known to have it`. A launch flag the
//! CLI does not know makes it exit, so flags are gated before spawn on the
//! binary's `--version`; the run's own `initialize` answer carries
//! `claude_code_version`, which is logged and kept for gating on the live
//! process. An unknown version (a wrapper script, a probe that failed) is
//! taken as current: refusing would break exactly the setups we cannot see.
//!
//! The minimums are the oldest CLI this harness was checked against for each
//! feature, not the CLI's own changelog: a feature may predate them.

use semver::Version;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Feature {
    /// `--session-id`: a fresh run's session id chosen by onicode.
    SessionId,
    /// `--resume-session-at`: resume only up to one transcript entry.
    ResumeAt,
    /// `--resume-drops-turn`: a truncating resume refused unless the cut is
    /// exactly that turn.
    ResumeDropsTurn,
    /// `--fork-session`: resume into a new session holding a copy.
    ForkSession,
    /// `initialize.rapidFollowupPreempt` and `turn_preempted`.
    RapidFollowupPreempt,
    /// `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING` and `rewind_files`.
    FileCheckpointing,
    /// `--include-hook-events`: `hook_started` / `hook_response` frames.
    HookEvents,
}

impl Feature {
    fn minimum(self) -> Version {
        let (major, minor, patch) = match self {
            Feature::SessionId | Feature::ForkSession | Feature::ResumeAt => (2, 1, 247),
            Feature::FileCheckpointing | Feature::HookEvents => (2, 1, 247),
            Feature::ResumeDropsTurn | Feature::RapidFollowupPreempt => (2, 1, 318),
        };
        Version::new(major, minor, patch)
    }

    /// What the feature lets the person do, for a refusal.
    fn what(self) -> &'static str {
        match self {
            Feature::SessionId => "choose a new session's id",
            Feature::ResumeAt | Feature::ResumeDropsTurn => "edit and resend a message",
            Feature::ForkSession => "branch a conversation",
            Feature::RapidFollowupPreempt => "fold a quick follow-up into its turn",
            Feature::FileCheckpointing => "undo file changes",
            Feature::HookEvents => "follow its startup hooks",
        }
    }

    /// Whether a CLI of `version` has it; `None` (unknown) does.
    pub(crate) fn supported(self, version: Option<&Version>) -> bool {
        version.is_none_or(|v| strip(v) >= self.minimum())
    }

    /// "Claude Code 2.1.318 or newer is needed to …".
    pub(crate) fn required(self, version: Option<&Version>) -> String {
        let have = version
            .map(|v| format!(" (this is {v})"))
            .unwrap_or_default();
        format!(
            "Claude Code {} or newer is needed to {}{have}.",
            self.minimum(),
            self.what()
        )
    }
}

/// A pre-release (`2.1.320-next.1`) has what its release will.
fn strip(version: &Version) -> Version {
    Version::new(version.major, version.minor, version.patch)
}

/// `claude_code_version` as the CLI wrote it (`2.1.319`, or with a suffix).
pub(crate) fn parse(version: &str) -> Option<Version> {
    let version = version.trim();
    let core = version
        .split_whitespace()
        .next()
        .unwrap_or(version)
        .trim_start_matches('v');
    Version::parse(core).ok().or_else(|| {
        let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
        Some(Version::new(
            parts.next()??,
            parts.next()??,
            parts.next().flatten().unwrap_or(0),
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Feature; 7] = [
        Feature::SessionId,
        Feature::ResumeAt,
        Feature::ResumeDropsTurn,
        Feature::ForkSession,
        Feature::RapidFollowupPreempt,
        Feature::FileCheckpointing,
        Feature::HookEvents,
    ];

    #[test]
    fn unknown_versions_are_current_and_old_ones_are_refused_by_name() {
        for feature in ALL {
            assert!(feature.supported(None), "{feature:?}");
            assert!(feature.supported(Some(&feature.minimum())), "{feature:?}");
            assert!(
                !feature.supported(Some(&Version::new(2, 0, 0))),
                "{feature:?}"
            );
        }
        let old = Version::new(2, 1, 300);
        assert!(!Feature::ResumeDropsTurn.supported(Some(&old)));
        assert_eq!(
            Feature::ResumeDropsTurn.required(Some(&old)),
            "Claude Code 2.1.318 or newer is needed to edit and resend a message (this is 2.1.300)."
        );
    }

    /// Monotone in the version: whatever an older CLI has, a newer one has.
    #[test]
    fn support_is_monotone_in_the_version() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let a = Version::new(2, next() % 3, next() % 400);
            let b = Version::new(2, a.minor + next() % 2, a.patch + next() % 50);
            for feature in ALL {
                if feature.supported(Some(&a)) {
                    assert!(feature.supported(Some(&b)), "{feature:?} {a} -> {b}");
                }
            }
        }
    }

    #[test]
    fn versions_parse_as_the_cli_writes_them() {
        assert_eq!(parse("2.1.319"), Some(Version::new(2, 1, 319)));
        assert_eq!(
            parse("2.1.319 (Claude Code)"),
            Some(Version::new(2, 1, 319))
        );
        assert_eq!(parse("v2.1"), Some(Version::new(2, 1, 0)));
        assert!(
            Feature::RapidFollowupPreempt.supported(parse("2.1.318-next.4").as_ref()),
            "a pre-release has its release's features"
        );
        assert_eq!(parse("next"), None);
    }
}
