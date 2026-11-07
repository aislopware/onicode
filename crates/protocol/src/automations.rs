//! Automations' data: what runs, when and on which events, and each run's record. Storing and
//! running them is the engine's.
use std::collections::HashMap;

use chrono::{Datelike, Local, TimeZone, Timelike};
use serde::{Deserialize, Serialize};

/// The time now in Unix milliseconds, as automations store it.
pub fn now_millis() -> i64 {
    crate::time::now_ms().min(i64::MAX as u64) as i64
}

/// A fresh automation or run id: 32 random hex digits.
pub fn new_id() -> String {
    format!("{:032x}", uuid::Uuid::new_v4().as_u128())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub harness: String,
    pub model: String,
    #[serde(default)]
    pub model_settings: HashMap<String, String>,
    pub cwd: String,
    pub workspace_mode: String,
    #[serde(default)]
    pub worktree_cwd: String,
    #[serde(default)]
    pub session_folder_id: String,
    pub reuse_session: bool,
    pub runtime_mode: String,
    #[serde(default = "default_trigger_kind")]
    pub trigger_kind: String,
    #[serde(default)]
    pub trigger_event: String,
    pub schedule_kind: String,
    pub minute: i64,
    pub time: String,
    pub day_of_week: i64,
    #[serde(default)]
    pub triggers: Option<Vec<AutomationTrigger>>,
    pub missed_run_grace_minutes: i64,
    pub enabled: bool,
    pub next_run_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_session_id: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationUpsert {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub harness: String,
    pub model: String,
    #[serde(default)]
    pub model_settings: HashMap<String, String>,
    pub cwd: String,
    pub workspace_mode: String,
    #[serde(default)]
    pub worktree_cwd: String,
    #[serde(default)]
    pub session_folder_id: String,
    #[serde(default)]
    pub reuse_session: bool,
    pub runtime_mode: String,
    #[serde(default = "default_trigger_kind")]
    pub trigger_kind: String,
    #[serde(default)]
    pub trigger_event: String,
    pub schedule_kind: String,
    pub minute: i64,
    pub time: String,
    pub day_of_week: i64,
    #[serde(default)]
    pub triggers: Option<Vec<AutomationTrigger>>,
    pub missed_run_grace_minutes: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub next_run_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRun {
    pub id: String,
    pub automation_id: String,
    pub trigger: String,
    pub scheduled_for: i64,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationTrigger {
    #[serde(default)]
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub event: String,
    #[serde(default = "default_schedule_kind")]
    pub schedule_kind: String,
    #[serde(default)]
    pub minute: i64,
    #[serde(default = "default_time")]
    pub time: String,
    #[serde(default = "default_day_of_week")]
    pub day_of_week: i64,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default = "default_actor")]
    pub actor: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DueAutomationRun {
    pub automation: Automation,
    pub run: AutomationRun,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationEventClaim {
    pub event_key: String,
    pub event_kind: String,
    pub event: String,
    pub scheduled_for: i64,
    pub prompt: String,
}

pub fn default_true() -> bool {
    true
}

pub fn default_trigger_kind() -> String {
    "time".into()
}

pub fn default_schedule_kind() -> String {
    "weekdays".into()
}

pub fn default_time() -> String {
    "09:00".into()
}

pub fn default_day_of_week() -> i64 {
    1
}

pub fn default_actor() -> String {
    "anyone".into()
}

pub fn hydrate_triggers(automation: &mut Automation) {
    if automation.triggers.is_some() {
        return;
    }
    automation.triggers = Some(vec![legacy_trigger(automation)]);
}

pub fn legacy_trigger(automation: &Automation) -> AutomationTrigger {
    trigger_from_fields(
        &automation.id,
        &automation.trigger_kind,
        &automation.trigger_event,
        &automation.schedule_kind,
        automation.minute,
        &automation.time,
        automation.day_of_week,
    )
}

pub fn has_time_trigger(automation: &Automation) -> bool {
    match &automation.triggers {
        Some(triggers) => triggers.iter().any(|trigger| trigger.kind == "time"),
        None => automation.trigger_kind == "time",
    }
}

pub fn normalize_triggers(input: &AutomationUpsert) -> Vec<AutomationTrigger> {
    let mut triggers = input.triggers.clone().unwrap_or_else(|| {
        vec![trigger_from_fields(
            &input.id,
            &input.trigger_kind,
            &input.trigger_event,
            &input.schedule_kind,
            input.minute,
            &input.time,
            input.day_of_week,
        )]
    });
    for trigger in &mut triggers {
        if trigger.id.trim().is_empty() {
            trigger.id = new_id();
        }
        trigger.event = trigger.event.trim().to_string();
        trigger.repo = trigger.repo.trim().to_string();
        trigger.branch = trigger.branch.trim().to_string();
        trigger.actor = trigger.actor.trim().to_string();
        if trigger.actor.is_empty() {
            trigger.actor = default_actor();
        }
        trigger.repos = trigger
            .repos
            .iter()
            .map(|repo| repo.trim().to_string())
            .filter(|repo| !repo.is_empty())
            .collect();
    }
    triggers
}

pub fn trigger_from_fields(
    id: &str,
    kind: &str,
    event: &str,
    schedule_kind: &str,
    minute: i64,
    time: &str,
    day_of_week: i64,
) -> AutomationTrigger {
    AutomationTrigger {
        id: format!("{id}:legacy"),
        kind: kind.to_string(),
        event: if event.is_empty() {
            schedule_kind.to_string()
        } else {
            event.to_string()
        },
        schedule_kind: schedule_kind.to_string(),
        minute,
        time: time.to_string(),
        day_of_week,
        repos: Vec::new(),
        repo: String::new(),
        branch: String::new(),
        actor: default_actor(),
    }
}

pub fn parse_automation(value: String) -> Result<Automation, String> {
    let mut automation: Automation =
        serde_json::from_str(&value).map_err(|error| error.to_string())?;
    hydrate_triggers(&mut automation);
    Ok(automation)
}

pub fn valid_time(value: &str) -> bool {
    let Some((hour, minute)) = value.split_once(':') else {
        return false;
    };
    hour.len() == 2
        && minute.len() == 2
        && hour.parse::<u8>().is_ok_and(|value| value < 24)
        && minute.parse::<u8>().is_ok_and(|value| value < 60)
}

pub const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

impl AutomationTrigger {
    pub fn new(kind: &str, event: &str) -> Self {
        Self {
            id: new_id(),
            kind: kind.into(),
            event: event.into(),
            schedule_kind: if matches!(event, "hourly" | "daily" | "weekdays" | "weekly") {
                event.into()
            } else {
                "weekdays".into()
            },
            minute: 0,
            time: "09:00".into(),
            day_of_week: 1,
            repos: Vec::new(),
            repo: String::new(),
            branch: String::new(),
            actor: "anyone".into(),
        }
    }

    pub fn label(&self) -> String {
        if self.kind != "time" {
            return event_label(&self.kind, &self.event);
        }
        match self.schedule_kind.as_str() {
            "hourly" => format!("Hourly at :{:02}", self.minute),
            "daily" => format!("Daily at {}", self.time),
            "weekly" => format!(
                "{} at {}",
                WEEKDAYS.get(self.day_of_week as usize).unwrap_or(&"Weekly"),
                self.time
            ),
            _ => format!("Weekdays at {}", self.time),
        }
    }

    /// Calendar arithmetic in the user's time zone, including daylight-saving changes.
    pub fn next_at(&self, after: i64) -> i64 {
        let Some(start) = Local.timestamp_millis_opt(after).single() else {
            return after.saturating_add(86_400_000);
        };
        if self.schedule_kind == "hourly" {
            // Enumerating actual clock minutes also handles repeated/nonexistent DST hours.
            let mut candidate = (after / 60_000 + 1) * 60_000;
            for _ in 0..180 {
                if Local
                    .timestamp_millis_opt(candidate)
                    .single()
                    .is_some_and(|date| date.minute() == self.minute.clamp(0, 59) as u32)
                {
                    return candidate;
                }
                candidate += 60_000;
            }
            return candidate;
        }
        let (hour, minute) = self
            .time
            .split_once(':')
            .and_then(|(h, m)| Some((h.parse::<u32>().ok()?, m.parse::<u32>().ok()?)))
            .unwrap_or((9, 0));
        for offset in 0..15 {
            let date = start.date_naive() + chrono::Duration::days(offset);
            let weekday = date.weekday().num_days_from_sunday();
            if self.schedule_kind == "weekdays" && (weekday == 0 || weekday == 6) {
                continue;
            }
            if self.schedule_kind == "weekly" && weekday != self.day_of_week.clamp(0, 6) as u32 {
                continue;
            }
            if let Some(naive) = date.and_hms_opt(hour.min(23), minute.min(59), 0) {
                // A repeated clock time fires once. For a spring-forward gap use the first
                // valid clock minute after the requested wall time, as Date.setHours does.
                for gap in 0..=180 {
                    let target = naive + chrono::Duration::minutes(gap);
                    if let Some(candidate) = Local.from_local_datetime(&target).earliest() {
                        let at = candidate.timestamp_millis();
                        if at > after {
                            return at;
                        }
                        break;
                    }
                }
            }
        }
        after.saturating_add(7 * 86_400_000)
    }
}

pub fn event_label(kind: &str, event: &str) -> String {
    match (kind, event) {
        ("jira", "issue_created") => "Issue appeared",
        (_, "draft_opened") => "Draft opened",
        (_, "pull_request_opened") => "Pull request opened",
        (_, "issue_opened") => "Issue opened",
        (_, "merge_request_opened") => "Merge request opened",
        (_, "issue_created") => "Issue created",
        (_, "pull_request_appeared") => "Pull request appeared",
        (_, "work_item_appeared") => "Work item appeared",
        (_, "pull_request_merged") => "Pull request merged",
        (_, "pull_request_review_requested") => "Review requested",
        (_, "issue_assigned") => "Issue assigned",
        (_, "check_failed") => "Check failed",
        _ => event,
    }
    .to_owned()
}

pub fn next_run_at(triggers: &[AutomationTrigger], after: i64) -> i64 {
    triggers
        .iter()
        .filter(|t| t.kind == "time")
        .map(|t| t.next_at(after))
        .min()
        .unwrap_or_else(|| after.saturating_add(365 * 86_400_000))
}

impl Automation {
    pub fn triggers(&self) -> Vec<AutomationTrigger> {
        self.triggers
            .clone()
            .unwrap_or_else(|| vec![legacy_trigger(self)])
    }

    pub fn trigger_label(&self) -> String {
        let triggers = self.triggers();
        match triggers.len() {
            0 => "Manual only".into(),
            1 => triggers[0].label(),
            count => format!("{} + {} more", triggers[0].label(), count - 1),
        }
    }
}

impl AutomationUpsert {
    pub fn new(cwd: String, harness: String, model: String) -> Self {
        Self {
            id: new_id(),
            name: String::new(),
            prompt: String::new(),
            harness,
            model,
            model_settings: HashMap::new(),
            cwd,
            workspace_mode: "worktree".into(),
            worktree_cwd: String::new(),
            session_folder_id: String::new(),
            reuse_session: false,
            runtime_mode: "auto".into(),
            trigger_kind: "time".into(),
            trigger_event: String::new(),
            schedule_kind: "weekdays".into(),
            minute: 0,
            time: "09:00".into(),
            day_of_week: 1,
            triggers: Some(Vec::new()),
            missed_run_grace_minutes: 720,
            enabled: true,
            next_run_at: now_millis().saturating_add(365 * 86_400_000),
        }
    }

    pub fn from_automation(automation: &Automation) -> Self {
        Self {
            id: automation.id.clone(),
            name: automation.name.clone(),
            prompt: automation.prompt.clone(),
            harness: automation.harness.clone(),
            model: automation.model.clone(),
            model_settings: automation.model_settings.clone(),
            cwd: automation.cwd.clone(),
            workspace_mode: automation.workspace_mode.clone(),
            worktree_cwd: automation.worktree_cwd.clone(),
            session_folder_id: automation.session_folder_id.clone(),
            reuse_session: automation.reuse_session,
            runtime_mode: automation.runtime_mode.clone(),
            trigger_kind: automation.trigger_kind.clone(),
            trigger_event: automation.trigger_event.clone(),
            schedule_kind: automation.schedule_kind.clone(),
            minute: automation.minute,
            time: automation.time.clone(),
            day_of_week: automation.day_of_week,
            triggers: Some(automation.triggers()),
            missed_run_grace_minutes: automation.missed_run_grace_minutes,
            enabled: automation.enabled,
            next_run_at: automation.next_run_at,
        }
    }

    pub fn sync_triggers(&mut self, now: i64) {
        let triggers = self.triggers.get_or_insert_with(Vec::new);
        if let Some(primary) = triggers
            .iter()
            .find(|t| t.kind == "time")
            .or_else(|| triggers.first())
        {
            self.trigger_kind = primary.kind.clone();
            self.trigger_event = primary.event.clone();
            self.schedule_kind = primary.schedule_kind.clone();
            self.minute = primary.minute;
            self.time = primary.time.clone();
            self.day_of_week = primary.day_of_week;
        } else {
            self.trigger_kind = "time".into();
            self.trigger_event.clear();
        }
        self.next_run_at = next_run_at(triggers, now);
    }
}
