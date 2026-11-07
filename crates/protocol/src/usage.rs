//! Subscription usage, ported from rateLimits.ts and src-tauri/rate_limits.rs: the meter's
//! windows and limits, and what a provider's reports parse to. Polling and the per-account
//! ledgers are the engine's.
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::accounts::Identity;
use crate::provider::Kind;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// None when the provider reported only a status, as Claude Code's events may.
    pub used_percent: Option<f64>,
    pub minutes: u64,
    pub resets_at: Option<i64>,
    /// The provider's own verdict on this window, when it gives one.
    #[serde(default)]
    pub status: Option<Status>,
}
/// Claude Code's `rate_limit_event` status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "UsageStatus", rename_all = "snake_case")]
pub enum Status {
    Allowed,
    Warning,
    Rejected,
}
impl Status {
    /// Claude Code's status word, as its `rate_limit_event` sends it.
    pub fn from_claude(value: &Value) -> Option<Self> {
        match value.as_str()? {
            "allowed" => Some(Self::Allowed),
            "allowed_warning" => Some(Self::Warning),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }

    /// The account picker's words, shown where a percentage would be.
    pub fn label(self) -> &'static str {
        match self {
            Self::Allowed => "Ready",
            Self::Warning => "Running low",
            Self::Rejected => "Exhausted",
        }
    }
}
/// What an account's windows add up to: the account picker's tone.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Standing {
    Ready,
    /// The percentage left, when the provider reported one.
    Low(Option<f64>),
    /// When every exhausted window has reset, if known and still ahead.
    Exhausted(Option<i64>),
}
impl Window {
    fn reset(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|at| at <= now)
    }

    /// Percentage left, or None when the window reported neither a percentage nor a rejection.
    pub fn headroom(&self, now: i64) -> Option<f64> {
        if self.reset(now) {
            Some(100.)
        } else if self.status == Some(Status::Rejected) {
            Some(0.)
        } else {
            self.used_percent.map(|used| 100. - used.clamp(0., 100.))
        }
    }

    pub fn exhausted(&self) -> bool {
        self.status == Some(Status::Rejected) || self.used_percent.is_some_and(|used| used >= 100.)
    }

    /// The word shown in place of a percentage the provider did not report.
    pub fn status_label(&self) -> &'static str {
        self.status.unwrap_or(Status::Allowed).label()
    }

    pub fn label(&self) -> String {
        match self.minutes {
            300 => "5h".into(),
            1_440 => "1d".into(),
            10_080 => "wk".into(),
            43_200 => "mo".into(),
            n if n % 60 == 0 => format!("{}h", n / 60),
            n => format!("{n}m"),
        }
    }

    pub fn countdown(&self, now: i64) -> String {
        self.resets_at
            .map(|at| duration(at.saturating_sub(now)))
            .unwrap_or_else(|| self.label())
    }

    pub fn reset_label(&self, now: i64) -> String {
        match self.resets_at {
            Some(at) if at <= now => "Resets now".into(),
            Some(at) => format!("Resets in {}", duration(at - now)),
            None => format!("{} window", self.label()),
        }
    }
}

/// Named `UsageSnapshot` on the wire, beside the git and inbox snapshots.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename = "UsageSnapshot")]
pub struct Snapshot {
    pub windows: Vec<Window>,
    pub updated_at: i64,
    #[serde(default)]
    pub reset_credits: Option<ResetCredits>,
    #[serde(default)]
    pub extra_usage_balance: Option<f64>,
    #[serde(default)]
    pub identity: Option<Identity>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetCredits {
    pub available_count: u64,
    pub credits: Vec<ResetCredit>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetCredit {
    pub id: String,
    pub status: String,
    pub reset_type: String,
    pub granted_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub title: Option<String>,
    pub description: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limit {
    pub resets_at: Option<i64>,
    pub resume_at_reset: bool,
}
impl Limit {
    /// Manual continuation becomes available when the reported reset passes.
    pub fn waiting(&self, now: i64) -> bool {
        self.resets_at.is_some_and(|at| at > now)
    }

    pub fn from_error(message: &str) -> Option<Self> {
        // Claude's synthetic assistant errors use this code before the result
        // frame arrives. Ordinary transient HTTP 429s must remain retryable.
        if message.trim().eq_ignore_ascii_case("rate_limit") {
            return Some(Self::default());
        }
        static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let pattern=PATTERN.get_or_init(||regex::Regex::new(r"(?i)(?:usage|spending|monthly|weekly|daily)[ _]limit[ _](?:reached|exceeded)|(?:quota|credits?) (?:exceeded|exhausted|depleted)|insufficient[_ ](?:quota|credits)|hit your (?:(?:usage|session) )?limit|credit balance is too low|reached (?:its|your) usage limit").unwrap());
        pattern.is_match(message).then(Self::default)
    }

    /// Extra-usage refusals fall back to the subscription and do not stop a turn.
    pub fn from_claude_info(info: &Value) -> Option<Self> {
        (info["status"] == "rejected"
            && info["isUsingOverage"] != true
            && info["rateLimitType"] != "overage")
            .then(|| Self {
                resets_at: timestamp(&info["resetsAt"]),
                resume_at_reset: false,
            })
    }

    pub fn resume_due(&self, now: i64, busy: bool) -> bool {
        !busy
            && self.resume_at_reset
            && self
                .resets_at
                .is_some_and(|at| now >= at.saturating_add(30_000))
    }
}
/// One window as an agent's stream reported it, stamped when it arrived.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    pub window: Window,
    pub observed_at: i64,
}
impl Observation {
    /// Claude Code's `rate_limit_info`: `status`, `resetsAt` (epoch seconds), optional
    /// `utilization` (0..1) and `rateLimitType`. Events without a type are the five-hour session
    /// window; overage and types this meter has no row for are not windows of the subscription.
    pub fn from_claude_info(info: &Value, now: i64) -> Option<Self> {
        let status = Status::from_claude(&info["status"])?;
        let minutes = match info["rateLimitType"].as_str() {
            None | Some("five_hour") => 300,
            Some("seven_day") => 10_080,
            Some(_) => return None,
        };
        Some(Self {
            window: Window {
                used_percent: number(info, &["utilization"])
                    .map(|fraction| (fraction * 100.).clamp(0., 100.)),
                minutes,
                resets_at: timestamp(&info["resetsAt"]),
                status: Some(status),
            },
            observed_at: now,
        })
    }
}
/// What one account's turns have reported: the newest observation per window, and whether
/// the stream said the account is signed out since.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    windows: Vec<(i64, Window)>,
    signed_out: Option<i64>,
}
impl Ledger {
    /// Turns of one account run side by side, so reports can land out of order.
    pub fn observe(&mut self, observation: Observation) {
        let Observation {
            window,
            observed_at,
        } = observation;
        if self.signed_out.is_some_and(|at| at <= observed_at) {
            self.signed_out = None;
        }
        match self
            .windows
            .iter_mut()
            .find(|(_, w)| w.minutes == window.minutes)
        {
            Some(slot) if slot.0 > observed_at => {}
            Some(slot) => *slot = (observed_at, window),
            None => {
                self.windows.push((observed_at, window));
                self.windows.sort_by_key(|(_, w)| w.minutes);
            }
        }
    }

    pub fn sign_out(&mut self, at: i64) {
        if self.windows.iter().all(|(seen, _)| *seen <= at) {
            self.signed_out = Some(at);
        }
    }

    /// None until a turn reports something: the meter is not known yet, not unavailable.
    pub fn snapshot(&self) -> Option<Result<Snapshot, FetchError>> {
        if self.signed_out.is_some() {
            return Some(Err(FetchError::unavailable(
                "Claude Code reported that this account is signed out.",
            )));
        }
        let updated_at = self.windows.iter().map(|(at, _)| *at).max()?;
        Some(Ok(Snapshot {
            windows: self.windows.iter().map(|(_, w)| w.clone()).collect(),
            updated_at,
            ..Default::default()
        }))
    }
}
/// Whether this provider's meter comes from its turns rather than a poll.
pub fn from_turns(kind: Kind) -> bool {
    kind == Kind::Claude
}
/// Why a meter could not be read: `unavailable` when the provider has none for the account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchError {
    pub message: String,
    pub unavailable: bool,
}
impl FetchError {
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unavailable: true,
        }
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unavailable: false,
        }
    }
}

impl Snapshot {
    pub fn headroom(&self, now: i64) -> Option<f64> {
        self.windows
            .iter()
            .filter_map(|w| w.headroom(now))
            .reduce(f64::min)
    }

    /// React's `accountStatus` thresholds, plus the provider's own warning when it sent one.
    pub fn standing(&self, now: i64) -> Option<Standing> {
        if self.windows.is_empty() {
            return None;
        }
        let headroom = self.headroom(now);
        if headroom.is_some_and(|left| left <= 0.) {
            return Some(Standing::Exhausted(
                self.exhausted_reset().filter(|at| *at > now),
            ));
        }
        if let Some(left) = headroom.filter(|left| *left <= 20.) {
            return Some(Standing::Low(Some(left)));
        }
        let mut warned = self
            .windows
            .iter()
            .filter(|w| w.status == Some(Status::Warning) && !w.reset(now))
            .peekable();
        if warned.peek().is_some() {
            return Some(Standing::Low(
                warned.filter_map(|w| w.headroom(now)).reduce(f64::min),
            ));
        }
        Some(Standing::Ready)
    }

    pub fn exhausted_reset(&self) -> Option<i64> {
        let exhausted: Vec<_> = self.windows.iter().filter(|w| w.exhausted()).collect();
        if exhausted.is_empty() || exhausted.iter().any(|w| w.resets_at.is_none()) {
            None
        } else {
            exhausted.iter().filter_map(|w| w.resets_at).max()
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
pub fn duration(ms: i64) -> String {
    if ms <= 0 {
        return "now".into();
    }
    let minutes = ms / 60_000;
    let hours = minutes / 60;
    if hours >= 24 {
        let days = hours / 24;
        let rest = hours % 24;
        if rest > 0 {
            format!("{days}d {rest}h")
        } else {
            format!("{days}d")
        }
    } else if hours > 0 {
        let rest = minutes % 60;
        if rest > 0 {
            format!("{hours}h {rest}m")
        } else {
            format!("{hours}h")
        }
    } else {
        format!("{minutes}m")
    }
}
pub fn number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| value[*key].as_f64().filter(|v| v.is_finite()))
}
pub fn timestamp(value: &Value) -> Option<i64> {
    let numeric = value
        .as_f64()
        .or_else(|| value.as_str()?.parse::<f64>().ok());
    if let Some(n) = numeric.filter(|n| n.is_finite() && *n > 0.) {
        return Some(if n > 10_000_000_000. { n } else { n * 1000. } as i64);
    }
    chrono::DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(|d| d.timestamp_millis())
}
pub fn window(value: &Value, minutes: u64, percent_keys: &[&str]) -> Option<Window> {
    Some(Window {
        used_percent: Some(number(value, percent_keys)?.clamp(0., 100.)),
        minutes,
        resets_at: timestamp(&value["resetsAt"]).or_else(|| timestamp(&value["resets_at"])),
        status: None,
    })
}
pub fn parse_codex(value: &Value) -> Snapshot {
    let wrapper = value.get("rateLimits").unwrap_or(value);
    let mut windows: Vec<Window> = Vec::new();
    // An explicitly identified window always wins over a positional fallback.
    for fallback_pass in [false, true] {
        for (key, fallback) in [("primary", 300), ("secondary", 10_080)] {
            let raw = &wrapper[key];
            let classified =
                number(raw, &["windowDurationMins", "window_duration_mins"]).and_then(|n| {
                    [300_u64, 10_080, 43_200]
                        .into_iter()
                        .find(|m| (n - *m as f64).abs() <= 1.)
                });
            if classified.is_none() != fallback_pass {
                continue;
            }
            let minutes = classified.unwrap_or(fallback);
            if !windows.iter().any(|w| w.minutes == minutes)
                && let Some(w) = window(
                    raw,
                    minutes,
                    &["usedPercent", "used_percent", "used_percentage"],
                )
            {
                windows.push(w);
            }
        }
    }
    windows.sort_by_key(|w| w.minutes);
    Snapshot {
        windows,
        updated_at: now_ms(),
        reset_credits: parse_reset_credits(value),
        ..Default::default()
    }
}

pub fn parse_reset_credits(value: &Value) -> Option<ResetCredits> {
    let raw = value
        .get("rateLimitResetCredits")
        .or_else(|| value.get("rate_limit_reset_credits"))?;
    let count = number(raw, &["availableCount", "available_count"])?;
    let credits = raw["credits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = row["id"].as_str().filter(|s| !s.trim().is_empty())?;
            Some(ResetCredit {
                id: id.into(),
                status: row["status"]
                    .as_str()
                    .filter(|s| matches!(*s, "available" | "redeeming" | "redeemed"))
                    .unwrap_or("unknown")
                    .into(),
                reset_type: if row["resetType"] == "codexRateLimits" {
                    "codexRateLimits"
                } else {
                    "unknown"
                }
                .into(),
                granted_at: timestamp(
                    row.get("grantedAt")
                        .or_else(|| row.get("granted_at"))
                        .unwrap_or(&Value::Null),
                ),
                expires_at: timestamp(
                    row.get("expiresAt")
                        .or_else(|| row.get("expires_at"))
                        .unwrap_or(&Value::Null),
                ),
                title: row["title"].as_str().map(str::to_owned),
                description: row["description"].as_str().map(str::to_owned),
            })
        })
        .collect();
    Some(ResetCredits {
        available_count: count.max(0.).floor() as u64,
        credits,
    })
}
