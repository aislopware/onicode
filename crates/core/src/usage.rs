//! Subscription usage, ported from rateLimits.ts and src-tauri/rate_limits.rs.
//! Codex is polled through its app-server on a worker. Claude Code publishes no poll, so its
//! meter is what its own stream reported (`rate_limit_event`), kept per account in [`observed`].
//! No provider's credentials are ever read.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::catalog;
use crate::provider::Kind;

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Allowed,
    Warning,
    Rejected,
}
impl Status {
    fn from_claude(value: &Value) -> Option<Self> {
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
#[derive(Clone, Copy, Debug, PartialEq)]
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

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub windows: Vec<Window>,
    pub updated_at: i64,
    #[serde(default)]
    pub reset_credits: Option<ResetCredits>,
    #[serde(default)]
    pub extra_usage_balance: Option<f64>,
    #[serde(default)]
    pub identity: Option<crate::accounts::Identity>,
}
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ResetCredits {
    pub available_count: u64,
    pub credits: Vec<ResetCredit>,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ResetCredit {
    pub id: String,
    pub status: String,
    pub reset_type: String,
    pub granted_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub title: Option<String>,
    pub description: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Clone, Debug, PartialEq)]
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
type Ledgers = HashMap<(Kind, String), Ledger>;
fn ledgers() -> std::sync::MutexGuard<'static, Ledgers> {
    static LEDGERS: OnceLock<Mutex<Ledgers>> = OnceLock::new();
    LEDGERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}
/// Whether this provider's meter comes from its turns rather than a poll.
pub fn from_turns(kind: Kind) -> bool {
    kind == Kind::Claude
}
pub fn observe(kind: Kind, profile: &crate::accounts::Profile, observation: Observation) {
    ledgers()
        .entry((kind, profile.id.clone()))
        .or_default()
        .observe(observation);
}
pub fn observe_signed_out(kind: Kind, profile: &crate::accounts::Profile, at: i64) {
    ledgers()
        .entry((kind, profile.id.clone()))
        .or_default()
        .sign_out(at);
}
/// A sign-in or sign-out makes what earlier turns reported belong to someone else.
pub fn forget(kind: Kind, profile: &crate::accounts::Profile) {
    ledgers().remove(&(kind, profile.id.clone()));
}
pub fn observed(
    kind: Kind,
    profile: &crate::accounts::Profile,
) -> Option<Result<Snapshot, FetchError>> {
    ledgers().get(&(kind, profile.id.clone()))?.snapshot()
}
#[derive(Clone, Debug)]
pub struct FetchError {
    pub message: String,
    pub unavailable: bool,
}
impl FetchError {
    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unavailable: true,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unavailable: false,
        }
    }
}

pub fn fetch(kind: Kind) -> Result<Snapshot, FetchError> {
    fetch_for(kind, &Default::default(), None)
}
pub fn fetch_for(
    kind: Kind,
    profile: &crate::accounts::Profile,
    model: Option<&str>,
) -> Result<Snapshot, FetchError> {
    // Only Codex publishes a poll (app-server `account/rateLimits/read`). Claude Code's meter is
    // what its turns reported; the others' would need credential files or private endpoints.
    let _ = model;
    if from_turns(kind) {
        return observed(kind, profile).unwrap_or_else(|| {
            Err(FetchError::unavailable(
                "Usage appears after this account's next turn.",
            ))
        });
    }
    if kind != Kind::Codex || !kind.subscription_usage() {
        return Err(FetchError::unavailable(format!(
            "No subscription usage is available for {}.",
            kind.label()
        )));
    }
    let value = catalog::codex_request(profile, "account/rateLimits/read", serde_json::json!({}))
        .map_err(|message| {
        let lower = message.to_lowercase();
        if [
            "not installed",
            "not signed in",
            "not authenticated",
            "authentication required",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            FetchError::unavailable("Sign in with codex login, then refresh usage.")
        } else {
            FetchError::failed(message)
        }
    })?;
    let snapshot = parse_codex(&value);
    if snapshot.windows.is_empty() && snapshot.reset_credits.is_none() {
        return Err(FetchError::unavailable(
            "No Codex subscription usage available for this account.",
        ));
    }
    Ok(snapshot)
}

pub fn consume_reset(
    profile: &crate::accounts::Profile,
    credit: Option<&str>,
) -> Result<String, String> {
    let mut params = serde_json::json!({"idempotencyKey": crate::accounts::request_id()});
    if let Some(credit) = credit {
        params["creditId"] = credit.into();
    }
    let result = catalog::codex_request(profile, "account/rateLimitResetCredit/consume", params)?;
    match result["outcome"].as_str() {
        Some(outcome @ ("reset" | "nothingToReset" | "noCredit" | "alreadyRedeemed")) => {
            Ok(outcome.into())
        }
        _ => Err("Codex returned an unknown reset result".into()),
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
fn number(value: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| value[*key].as_f64().filter(|v| v.is_finite()))
}
fn timestamp(value: &Value) -> Option<i64> {
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
fn window(value: &Value, minutes: u64, percent_keys: &[&str]) -> Option<Window> {
    Some(Window {
        used_percent: Some(number(value, percent_keys)?.clamp(0., 100.)),
        minutes,
        resets_at: timestamp(&value["resetsAt"]).or_else(|| timestamp(&value["resets_at"])),
        status: None,
    })
}
fn parse_codex(value: &Value) -> Snapshot {
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
            if !windows.iter().any(|w| w.minutes == minutes) {
                if let Some(w) = window(
                    raw,
                    minutes,
                    &["usedPercent", "used_percent", "used_percentage"],
                ) {
                    windows.push(w);
                }
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

fn parse_reset_credits(value: &Value) -> Option<ResetCredits> {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn explicit_codex_windows_win_before_positional_fallbacks() {
        let snapshot = parse_codex(
            &json!({"primary":{"usedPercent":99},"secondary":{"usedPercent":12,"windowDurationMins":300}}),
        );
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].used_percent, Some(12.));
        let snapshot = parse_codex(
            &json!({"primary":{"usedPercent":44,"windowDurationMins":10080},"secondary":{"usedPercent":88}}),
        );
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].used_percent, Some(44.));
    }
    #[test]
    fn reset_credit_details_keep_aliases_and_unknown_statuses() {
        let credits = parse_reset_credits(&json!({"rate_limit_reset_credits":{"available_count":2.9,"credits":[
            {"id":"a","status":"available","resetType":"codexRateLimits","expires_at":1800000000},
            {"id":"b","status":"future-status","resetType":"future-type","grantedAt":"2026-10-18T12:30:00Z"},
            {"id":""}
        ]}})).unwrap();
        assert_eq!(credits.available_count, 2);
        assert_eq!(credits.credits.len(), 2);
        assert_eq!(credits.credits[0].expires_at, Some(1_800_000_000_000));
        assert_eq!(credits.credits[1].status, "unknown");
        assert_eq!(credits.credits[1].reset_type, "unknown");
        assert!(parse_reset_credits(&json!({"rateLimitResetCredits":{"credits":[]}})).is_none());
    }
    #[test]
    fn exhausted_accounts_wait_for_all_windows_and_resume_only_when_armed() {
        let mut snapshot = Snapshot {
            windows: vec![
                Window {
                    used_percent: Some(100.),
                    minutes: 300,
                    resets_at: Some(100_000),
                    status: None,
                },
                Window {
                    used_percent: Some(100.),
                    minutes: 10080,
                    resets_at: Some(200_000),
                    status: None,
                },
            ],
            ..Default::default()
        };
        assert_eq!(snapshot.headroom(99_000), Some(0.));
        assert_eq!(snapshot.headroom(150_000), Some(0.));
        assert_eq!(snapshot.headroom(200_000), Some(100.));
        assert_eq!(snapshot.exhausted_reset(), Some(200_000));
        snapshot.windows[1].resets_at = None;
        assert_eq!(snapshot.exhausted_reset(), None);
        let mut limit = Limit {
            resets_at: Some(200_000),
            resume_at_reset: false,
        };
        assert!(!limit.resume_due(230_000, false));
        limit.resume_at_reset = true;
        assert!(!limit.resume_due(229_999, false));
        assert!(!limit.resume_due(230_000, true));
        assert!(limit.resume_due(230_000, false));
        assert!(Limit::from_error("usage_limit_reached").is_some());
        assert!(Limit::from_error("insufficient_quota").is_some());
        assert!(Limit::from_error("rate_limit").is_some());
        assert!(Limit::from_error("You've hit your session limit · resets 5:30pm").is_some());
        assert!(Limit::from_error("429: rate limit exceeded, retry in 3 seconds").is_none());
        assert!(Limit::from_error("Network timeout").is_none());
    }

    #[test]
    fn codex_classifies_swapped_and_monthly_windows() {
        let snapshot = parse_codex(&json!({"rateLimits": {
            "primary": {"usedPercent": 35, "windowDurationMins": 10080, "resetsAt": 1800000000},
            "secondary": {"used_percent": 105, "window_duration_mins": 300, "resets_at": 1800000000000_i64}
        }}));
        assert_eq!(snapshot.windows.len(), 2);
        assert_eq!(snapshot.windows[0].minutes, 300);
        assert_eq!(snapshot.windows[0].used_percent, Some(100.));
        assert_eq!(snapshot.windows[1].minutes, 10_080);
        assert_eq!(snapshot.windows[0].resets_at, snapshot.windows[1].resets_at);
        let monthly =
            parse_codex(&json!({"primary": {"usedPercent": 12, "windowDurationMins": 43200}}));
        assert_eq!(monthly.windows[0].label(), "mo");
        assert!(
            parse_codex(&json!({"primary": {"usedPercent": "bad"}}))
                .windows
                .is_empty()
        );
    }
    #[test]
    fn reset_countdowns_match_the_footer() {
        for (ms, expected) in [
            (0, "now"),
            (-1, "now"),
            (47 * 60_000, "47m"),
            (234 * 60_000, "3h 54m"),
            (9060 * 60_000, "6d 7h"),
        ] {
            assert_eq!(duration(ms), expected);
        }
        let w = Window {
            used_percent: Some(4.),
            minutes: 300,
            ..Default::default()
        };
        assert_eq!(w.countdown(0), "5h");
    }

    fn claude(info: Value, at: i64) -> Option<Observation> {
        Observation::from_claude_info(&info, at)
    }

    #[test]
    fn claude_rate_limit_events_map_each_status_with_and_without_utilization() {
        for (status, expected) in [
            ("allowed", Status::Allowed),
            ("allowed_warning", Status::Warning),
            ("rejected", Status::Rejected),
        ] {
            let measured = claude(
                json!({"status":status,"resetsAt":1792600200,"utilization":0.25,"rateLimitType":"five_hour"}),
                7,
            )
            .unwrap();
            assert_eq!(
                measured,
                Observation {
                    window: Window {
                        used_percent: Some(25.),
                        minutes: 300,
                        resets_at: Some(1_792_600_200_000),
                        status: Some(expected),
                    },
                    observed_at: 7,
                }
            );
            let bare = claude(json!({"status":status,"resetsAt":1792600200}), 7).unwrap();
            assert_eq!(bare.window.used_percent, None);
            assert_eq!(bare.window.minutes, 300);
            assert_eq!(bare.window.status_label(), expected.label());
        }
        let weekly = claude(
            json!({"status":"allowed","rateLimitType":"seven_day","utilization":3.5}),
            0,
        )
        .unwrap();
        assert_eq!(weekly.window.minutes, 10_080);
        assert_eq!(weekly.window.used_percent, Some(100.));
        assert_eq!(weekly.window.label(), "wk");
        for ignored in [
            json!({"status":"allowed","rateLimitType":"overage"}),
            json!({"status":"allowed","rateLimitType":"seven_day_opus"}),
            json!({"status":"queued","rateLimitType":"five_hour"}),
            json!({"resetsAt":1792600200}),
        ] {
            assert_eq!(claude(ignored, 0), None);
        }
    }

    fn five_hour(used: Option<f64>, status: Option<Status>, resets_at: Option<i64>) -> Window {
        Window {
            used_percent: used,
            minutes: 300,
            resets_at,
            status,
        }
    }
    fn standing(windows: Vec<Window>, now: i64) -> Option<Standing> {
        Snapshot {
            windows,
            ..Default::default()
        }
        .standing(now)
    }

    #[test]
    fn account_standing_follows_reported_status_and_percentage() {
        use Standing::*;
        use Status::*;
        let ahead = Some(10_000);
        for (windows, expected) in [
            (vec![], None),
            (vec![five_hour(None, Some(Allowed), ahead)], Some(Ready)),
            (vec![five_hour(None, Some(Warning), ahead)], Some(Low(None))),
            (
                vec![five_hour(Some(70.), Some(Warning), ahead)],
                Some(Low(Some(30.))),
            ),
            (
                vec![five_hour(None, Some(Rejected), ahead)],
                Some(Exhausted(ahead)),
            ),
            (
                vec![five_hour(None, Some(Rejected), None)],
                Some(Exhausted(None)),
            ),
            // A rejected window whose reset passed is usable again.
            (
                vec![five_hour(None, Some(Rejected), Some(500))],
                Some(Ready),
            ),
            (vec![five_hour(None, Some(Warning), Some(500))], Some(Ready)),
            (vec![five_hour(Some(79.), None, ahead)], Some(Ready)),
            (
                vec![five_hour(Some(80.), None, ahead)],
                Some(Low(Some(20.))),
            ),
            (
                vec![five_hour(Some(100.), None, ahead)],
                Some(Exhausted(ahead)),
            ),
            (
                vec![
                    five_hour(Some(10.), Some(Allowed), ahead),
                    Window {
                        minutes: 10_080,
                        ..five_hour(None, Some(Warning), ahead)
                    },
                ],
                Some(Low(None)),
            ),
        ] {
            assert_eq!(standing(windows, 1_000), expected);
        }
    }

    #[test]
    fn claude_ledger_reports_nothing_until_a_turn_and_sign_in_only_after_an_auth_failure() {
        let mut ledger = Ledger::default();
        assert!(ledger.snapshot().is_none());
        ledger.observe(claude(json!({"status":"allowed","utilization":0.25}), 100).unwrap());
        let snapshot = ledger.snapshot().unwrap().unwrap();
        assert_eq!(snapshot.updated_at, 100);
        assert_eq!(snapshot.windows[0].used_percent, Some(25.));
        ledger.sign_out(200);
        assert!(ledger.snapshot().unwrap().unwrap_err().unavailable);
        // A report from before the failure does not sign the account back in; a later one does.
        ledger.observe(claude(json!({"status":"allowed"}), 150).unwrap());
        assert!(ledger.snapshot().unwrap().is_err());
        ledger.observe(claude(json!({"status":"allowed"}), 250).unwrap());
        assert_eq!(ledger.snapshot().unwrap().unwrap().updated_at, 250);
    }

    #[test]
    fn observed_usage_is_kept_per_provider_and_account() {
        let profile = crate::accounts::Profile::new(
            &format!("usage-ledger-{}", crate::new_id()),
            std::path::Path::new("/fixture"),
        );
        assert!(observed(Kind::Claude, &profile).is_none());
        assert!(
            fetch_for(Kind::Claude, &profile, None)
                .unwrap_err()
                .unavailable
        );
        observe(
            Kind::Claude,
            &profile,
            claude(json!({"status":"rejected","resetsAt":1792600200}), 1).unwrap(),
        );
        let snapshot = fetch_for(Kind::Claude, &profile, None).unwrap();
        assert_eq!(snapshot.exhausted_reset(), Some(1_792_600_200_000));
        assert!(observed(Kind::Codex, &profile).is_none());
        observe_signed_out(Kind::Claude, &profile, 2);
        assert!(observed(Kind::Claude, &profile).unwrap().is_err());
        forget(Kind::Claude, &profile);
        assert!(observed(Kind::Claude, &profile).is_none());
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            /// Metamorphic: however a window's reports interleave, the newest one is what shows.
            #[test]
            fn the_newest_report_per_window_wins_in_any_arrival_order(
                reports in prop::collection::vec(
                    (0..3_usize, prop::option::of(0.0..1.0_f64), any::<bool>()),
                    1..24,
                )
                .prop_flat_map(|reports| {
                    let order = Just((0..reports.len()).collect::<Vec<_>>()).prop_shuffle();
                    (Just(reports), order)
                })
            ) {
                let (reports, order) = reports;
                let observations: Vec<Observation> = reports
                    .iter()
                    .enumerate()
                    .map(|(at, (status, utilization, weekly))| {
                        let status = ["allowed", "allowed_warning", "rejected"][*status];
                        let mut info = json!({
                            "status": status,
                            "rateLimitType": if *weekly { "seven_day" } else { "five_hour" },
                        });
                        if let Some(fraction) = utilization {
                            info["utilization"] = json!(fraction);
                        }
                        claude(info, at as i64).unwrap()
                    })
                    .collect();
                let mut in_order = Ledger::default();
                for observation in &observations {
                    in_order.observe(observation.clone());
                }
                let mut shuffled = Ledger::default();
                for ix in order {
                    shuffled.observe(observations[ix].clone());
                }
                let expected = in_order.snapshot().unwrap().unwrap();
                let actual = shuffled.snapshot().unwrap().unwrap();
                prop_assert_eq!(&actual.windows, &expected.windows);
                prop_assert_eq!(actual.updated_at, observations.len() as i64 - 1);
                for w in &actual.windows {
                    let newest = observations.iter().rfind(|o| o.window.minutes == w.minutes).unwrap();
                    prop_assert_eq!(w, &newest.window);
                }
                prop_assert!(actual.windows.windows(2).all(|pair| pair[0].minutes < pair[1].minutes));
            }

            /// Without a provider status, the tone keeps monocode's percentage thresholds.
            #[test]
            fn polled_windows_keep_the_percentage_thresholds(
                used in prop::collection::vec(0.0..=100.0_f64, 1..3),
            ) {
                let windows: Vec<_> = used.iter().map(|u| five_hour(Some(*u), None, None)).collect();
                let left = 100. - used.iter().copied().fold(0., f64::max);
                let expected = if left <= 0. {
                    Standing::Exhausted(None)
                } else if left <= 20. {
                    Standing::Low(Some(left))
                } else {
                    Standing::Ready
                };
                prop_assert_eq!(standing(windows, 0), Some(expected));
            }
        }
    }
}
