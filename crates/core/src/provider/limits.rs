//! A turn's account limit from the provider's structured report. Claude's reset time is kept
//! through its terminal error, even when its synthetic assistant frame ends the transport before
//! the result frame can arrive.
use super::*;
use crate::usage::Limit;

impl Protocol {
    pub(super) fn claude_error(&mut self, error: &str, frame: &Value) -> String {
        // SDKAssistantMessage's published error code; the footer offers sign-in only after it.
        if error == "authentication_failed" || frame["error"] == "authentication_failed" {
            crate::usage::observe_signed_out(
                self.request.kind,
                &self.request.settings.account,
                crate::usage::now_ms(),
            );
        }
        let info = frame
            .get("rate_limit_info")
            .or_else(|| frame.get("quotaLimits"))
            .or_else(|| frame["apiErrorParams"].get("rate_limit_info"));
        if let Some(limit) = info.and_then(Limit::from_claude_info) {
            self.usage_limit = Some(limit);
        }
        // SDKAssistantMessage's published error code for a spent account, which can arrive before
        // the result frame. An ordinary transient HTTP 429 has no code and stays retryable.
        if self.usage_limit.is_none() && error.trim() == "rate_limit" {
            self.usage_limit = Some(Limit::default());
        }
        if self.usage_limit.is_some() {
            "Claude Code reached its usage limit. Choose another available model or retry when the limit resets.".into()
        } else {
            error.into()
        }
    }

    /// Codex marks a turn failed by a spent account with `codexErrorInfo: "usageLimitExceeded"`.
    pub(super) fn note_codex_limit(&mut self, error: &Value) {
        if error["codexErrorInfo"] == "usageLimitExceeded" {
            self.usage_limit = Some(Limit::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol() -> Protocol {
        Protocol::new(Request {
            kind: Kind::Claude,
            binary: "unused".into(),
            cwd: "/tmp".into(),
            prompt: "Hello".into(),
            provider_id: None,
            model: None,
            settings: Default::default(),
        })
    }

    #[test]
    fn claude_limit_assistant_keeps_the_rejected_windows_reset_time() {
        let mut p = protocol();
        let mut out = Vec::new();
        p.frame(
            json!({"type":"rate_limit_event","rate_limit_info":{
                "status":"rejected","resetsAt":1792600200,"rateLimitType":"five_hour"
            }}),
            &mut out,
        )
        .unwrap();
        let error = p
            .frame(
                json!({"type":"assistant","error":"rate_limit",
                    "message":{"content":[{"type":"text","text":"You've hit your session limit"}]}
                }),
                &mut out,
            )
            .unwrap_err();
        assert!(error.contains("usage limit"));
        assert_eq!(
            p.usage_limit.as_ref().unwrap().resets_at,
            Some(1_792_600_200_000)
        );
        assert!(!p.usage_limit.unwrap().resume_at_reset);
    }

    #[test]
    fn claude_limit_result_and_embedded_error_info_work_without_a_rate_event() {
        for frame in [
            json!({"type":"result","is_error":true,"result":"rate_limit"}),
            json!({"type":"assistant","error":"rate_limit","apiErrorParams":{
                "rate_limit_info":{"status":"rejected","resetsAt":"1792600200000"}
            }}),
        ] {
            let mut p = protocol();
            assert!(
                p.frame(frame, &mut Vec::new())
                    .unwrap_err()
                    .contains("usage limit")
            );
            assert!(p.usage_limit.is_some());
        }
    }

    #[test]
    fn limit_wording_without_a_structured_report_is_an_ordinary_error() {
        let mut p = protocol();
        let error = p
            .frame(
                json!({"type":"result","is_error":true,"errors":["You've hit your usage limit."]}),
                &mut Vec::new(),
            )
            .unwrap_err();
        assert!(!error.contains("Claude Code reached its usage limit"));
        assert!(p.usage_limit.is_none());
    }

    #[test]
    fn codex_usage_limit_is_read_from_its_error_info() {
        for (info, limited) in [("usageLimitExceeded", true), ("other", false)] {
            let mut p = protocol();
            p.request.kind = Kind::Codex;
            let error = p
                .frame(
                    json!({"method":"turn/completed","params":{"turn":{"status":"failed",
                        "error":{"message":"You've hit your usage limit.","codexErrorInfo":info}}}}),
                    &mut Vec::new(),
                )
                .unwrap_err();
            assert_eq!(error, "You've hit your usage limit.");
            assert_eq!(p.usage_limit.is_some(), limited, "{info}");
        }
    }

    #[test]
    fn claude_recovered_overage_and_child_limits_do_not_block_the_parent() {
        let mut p = protocol();
        let mut out = Vec::new();
        for info in [
            json!({"status":"allowed_warning","resetsAt":1792600200}),
            json!({"status":"rejected","isUsingOverage":true}),
            json!({"status":"rejected","rateLimitType":"overage"}),
        ] {
            p.usage_limit = Some(Limit::default());
            p.frame(
                json!({"type":"rate_limit_event","rate_limit_info":info}),
                &mut out,
            )
            .unwrap();
            assert!(p.usage_limit.is_none());
        }
        p.frame(
            json!({"type":"rate_limit_event","parent_tool_use_id":"child",
                "rate_limit_info":{"status":"rejected","resetsAt":1792600200}
            }),
            &mut out,
        )
        .unwrap();
        assert!(p.usage_limit.is_none());
        assert_eq!(
            p.frame(
                json!({"type":"assistant","error":"authentication_failed"}),
                &mut out
            )
            .unwrap_err(),
            "authentication_failed"
        );
        assert!(p.usage_limit.is_none());
    }

    #[test]
    fn claude_rate_events_feed_the_accounts_meter_and_auth_failures_sign_it_out() {
        let mut p = protocol();
        p.request.settings.account = crate::accounts::Profile::new(
            &format!("limits-meter-{}", crate::new_id()),
            std::path::Path::new("/fixture"),
        );
        let account = p.request.settings.account.clone();
        let mut out = Vec::new();
        for frame in [
            json!({"type":"rate_limit_event","rate_limit_info":{
                "status":"allowed","resetsAt":4_102_444_800_i64,"utilization":0.25,"rateLimitType":"five_hour"
            }}),
            json!({"type":"rate_limit_event","parent_tool_use_id":"child","rate_limit_info":{
                "status":"allowed_warning","resetsAt":4_102_444_800_i64,"rateLimitType":"seven_day"
            }}),
            json!({"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"overage"}}),
        ] {
            p.frame(frame, &mut out).unwrap();
        }
        assert!(p.usage_limit.is_none());
        let snapshot = crate::usage::observed(Kind::Claude, &account)
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot
                .windows
                .iter()
                .map(|w| (w.minutes, w.used_percent))
                .collect::<Vec<_>>(),
            vec![(300, Some(25.)), (10_080, None)]
        );
        assert_eq!(
            snapshot.standing(snapshot.updated_at),
            Some(crate::usage::Standing::Low(None))
        );
        p.frame(
            json!({"type":"assistant","error":"authentication_failed"}),
            &mut out,
        )
        .unwrap_err();
        assert!(
            crate::usage::observed(Kind::Claude, &account)
                .unwrap()
                .unwrap_err()
                .unavailable
        );
    }

    #[test]
    fn claude_transport_delivers_the_limit_before_returning_its_terminal_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("claude-fixture");
        std::fs::write(&binary, r#"#!/bin/sh
read -r initialize
printf '%s\n' '{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":1792600200}}' '{"type":"assistant","error":"rate_limit"}'
"#).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut request = protocol().request;
        request.binary = binary;
        request.cwd = dir.path().into();
        let mut limited = Vec::new();
        let result = run(request, Arc::new(AtomicBool::new(false)), |events| {
            limited.extend(events.into_iter().filter_map(|event| match event {
                Event::UsageLimited(limit) => Some(limit),
                _ => None,
            }));
            Ok(())
        });
        assert!(result.unwrap_err().contains("usage limit"));
        assert_eq!(
            limited,
            vec![Limit {
                resets_at: Some(1_792_600_200_000),
                resume_at_reset: false
            }]
        );
    }
}
