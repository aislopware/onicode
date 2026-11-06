//! What a `can_use_tool` request asks the person, and how the answer goes
//! back to the CLI.
//!
//! A Claude approval offers more than yes or no: allow once, allow for this
//! chat (the CLI's own `permission_suggestions`, re-targeted to the session),
//! always allow (the suggestions as given, which may write the user's
//! settings), deny (anything typed instead is the note the model reads), and
//! stop (deny and end the turn). The card says why the CLI asks and which
//! path made it, and carries the CLI's rules for answering
//! ([`ApprovalContext`]): `default_to_no` puts deny first, and neither it nor
//! `requires_user_interaction` lets one keystroke approve.

use onicode_proto::{AccessMode, ApprovalContext, UserInputAnswer, UserInputQuestion};
use serde_json::{Value, json};

use super::wire::{self, ControlRequestBody};
use crate::approval::{self, Action};

/// Claude's file-changing tools: auto-accept edits allows these unasked.
pub(super) const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

pub(super) const ALLOW_ONCE: &str = "Allow once";
pub(super) const ALLOW_CHAT: &str = "Allow for this chat";
pub(super) const ALLOW_ALWAYS: &str = "Always allow";
pub(super) const DENY: &str = "Deny";
pub(super) const STOP: &str = "Stop";
pub(super) const IMPLEMENT: &str = "Implement";
pub(super) const KEEP_PLANNING: &str = "Keep planning";

/// What a refused approval tells the agent.
pub(super) const APPROVAL_DENIED: &str = "The user did not allow this.";
/// What a stop tells the agent, as the turn ends.
const APPROVAL_STOPPED: &str = "The user stopped this turn instead of allowing it.";

/// The base question a `can_use_tool` request asks before it is allowed,
/// in `access`; `None` allows it now. `ExitPlanMode` (the plan is ready)
/// asks whether to start implementing it.
pub(super) fn approval_question(
    tool: &str,
    display_name: Option<&str>,
    description: Option<&str>,
    input: &Value,
    access: AccessMode,
) -> Option<UserInputQuestion> {
    if tool == "ExitPlanMode" {
        return (!access.is_full_access()).then(|| {
            let mut question = approval::question(
                "Approve plan",
                "Claude wants to start implementing its plan. Allow it?",
            );
            question.options = vec![IMPLEMENT.into(), KEEP_PLANNING.into(), STOP.into()];
            question
        });
    }
    let edit = EDIT_TOOLS.contains(&tool);
    let action = if edit { Action::Edit } else { Action::Other };
    if approval::allowed_without_asking(access, action) {
        return None;
    }
    let field = |key: &str| input.get(key).and_then(Value::as_str);
    Some(if tool == "Bash" {
        approval::command("Claude", field("command").unwrap_or_default())
    } else if edit {
        let paths: Vec<&str> = field("file_path")
            .or_else(|| field("notebook_path"))
            .into_iter()
            .collect();
        approval::edit("Claude", &paths)
    } else {
        let name = display_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or(tool);
        approval::tool("Claude", name, description)
    })
}

/// One approval to ask, and the rules each answer would add.
#[derive(Debug, Clone)]
pub(super) struct Ask {
    pub question: UserInputQuestion,
    /// `updatedPermissions` for "Allow for this chat".
    chat_rules: Vec<Value>,
    /// `updatedPermissions` for "Always allow".
    always_rules: Vec<Value>,
    tool_use_id: Option<String>,
    exit_plan: bool,
}

/// The person's answer to an [`Ask`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Decision {
    Once,
    Chat,
    Always,
    /// What they typed instead of picking, if anything.
    Deny(Option<String>),
    Stop,
}

/// The full approval for `body`, or `None` when `access` allows it unasked.
pub(super) fn ask(body: &ControlRequestBody, access: AccessMode) -> Option<Ask> {
    let mut question = approval_question(
        &body.tool_name,
        body.display_name.as_deref(),
        body.description.as_deref(),
        &body.input,
        access,
    )?;
    let exit_plan = body.tool_name == "ExitPlanMode";
    // A lasting rule only where the CLI offered one and saves it.
    let rules = (!body.suppress_always_allow_rule)
        .then_some(&body.permission_suggestions)
        .filter(|s| !s.is_empty());
    let chat_rules: Vec<Value> = rules
        .map(|rules| rules.iter().map(for_this_chat).collect())
        .unwrap_or_default();
    let always_rules = rules.cloned().unwrap_or_default();
    if !exit_plan {
        let mut options = vec![ALLOW_ONCE.to_owned()];
        if !chat_rules.is_empty() {
            options.push(ALLOW_CHAT.into());
        }
        // "Always" differs from "this chat" only when a rule names a
        // destination beyond the session.
        if always_rules
            .iter()
            .any(|r| destination(r) != Some("session"))
        {
            options.push(ALLOW_ALWAYS.into());
        }
        options.push(DENY.into());
        options.push(STOP.into());
        question.options = options;
    }
    if body.default_to_no
        && let Some(at) = question
            .options
            .iter()
            .position(|o| o == DENY || o == KEEP_PLANNING)
    {
        let deny = question.options.remove(at);
        question.options.insert(0, deny);
    }
    let reason = body
        .decision_reason
        .as_deref()
        .map(sanitize)
        .filter(|r| !r.is_empty());
    let path = body
        .blocked_path
        .as_deref()
        .map(sanitize)
        .filter(|p| !p.is_empty());
    if let Some(reason) = reason {
        question
            .question
            .push_str(&format!("\n\nWhy it asks: {reason}"));
    }
    if let Some(path) = path {
        question.question.push_str(&format!("\nPath: {path}"));
    }
    if body.default_to_no || body.requires_user_interaction {
        question.approval = Some(ApprovalContext {
            default_to_no: body.default_to_no,
            requires_user_interaction: body.requires_user_interaction,
        });
    }
    Some(Ask {
        question,
        chat_rules,
        always_rules,
        tool_use_id: body.tool_use_id.clone(),
        exit_plan,
    })
}

fn destination(rule: &Value) -> Option<&str> {
    rule.get("destination").and_then(Value::as_str)
}

/// A suggested rule, kept for this session only.
fn for_this_chat(rule: &Value) -> Value {
    let mut rule = rule.clone();
    if let Some(map) = rule.as_object_mut() {
        map.insert("destination".into(), json!("session"));
    }
    rule
}

impl Ask {
    /// How `answers` decide this ask. No answer — closed unanswered, or the
    /// run stopped waiting — denies; anything typed rather than picked is a
    /// deny carrying that text.
    pub(super) fn decide(&self, answers: &[UserInputAnswer]) -> Decision {
        let Some(label) = answers
            .iter()
            .find(|a| a.question_id == self.question.id)
            .and_then(|a| a.labels.first())
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
        else {
            return Decision::Deny(None);
        };
        let offered = |option: &str| self.question.options.iter().any(|o| o == option);
        match label {
            l if (l == ALLOW_ONCE || l == IMPLEMENT) && offered(l) => Decision::Once,
            l if l == ALLOW_CHAT && offered(l) => Decision::Chat,
            l if l == ALLOW_ALWAYS && offered(l) => Decision::Always,
            l if (l == DENY || l == KEEP_PLANNING) && offered(l) => Decision::Deny(None),
            l if l == STOP && offered(l) => Decision::Stop,
            note => Decision::Deny(Some(note.to_owned())),
        }
    }

    /// The `can_use_tool` reply for `decision`. Approving a plan leaves
    /// plan mode for `after_plan` (the run's access mode).
    pub(super) fn response(&self, decision: &Decision, input: Value, after_plan: &str) -> Value {
        let id = self.tool_use_id.as_deref();
        match decision {
            Decision::Once if self.exit_plan => wire::exit_plan_response(input, after_plan),
            Decision::Once => wire::allow_with_rules_response(input, Vec::new(), id, false),
            Decision::Chat => {
                wire::allow_with_rules_response(input, self.chat_rules.clone(), id, false)
            }
            Decision::Always => {
                wire::allow_with_rules_response(input, self.always_rules.clone(), id, true)
            }
            Decision::Deny(note) => {
                wire::deny_with_response(note.as_deref().unwrap_or(APPROVAL_DENIED), false, id)
            }
            Decision::Stop => wire::deny_with_response(APPROVAL_STOPPED, true, id),
        }
    }
}

/// Text the CLI may have written for a terminal: ANSI escapes and other
/// control characters out, whitespace runs folded.
pub(super) fn sanitize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // CSI: parameters up to a final byte in @..~.
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST.
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// An MCP server's `elicitation` as questions, and how their answers go
/// back. Forms take flat schemas (the same bridge Codex's elicitations
/// use); a URL is opened by the person, who then says whether they are done.
pub(super) enum Elicitation {
    Form(crate::codex::elicitation::Form),
    Url { question: UserInputQuestion },
}

const ELICIT_DONE: &str = "Done";
const ELICIT_CANCEL: &str = "Cancel";

impl Elicitation {
    /// `None` for a form whose schema the bridge cannot ask: declined.
    pub(super) fn new(body: &ControlRequestBody) -> Option<Self> {
        let server = body
            .mcp_server_name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or("An MCP server");
        let message = body.message.as_deref().map(sanitize).unwrap_or_default();
        if body.mode.as_deref() == Some("url") {
            let url = body
                .url
                .as_deref()
                .map(sanitize)
                .filter(|u| !u.is_empty())?;
            let mut question = approval::question(
                format!("{server} asks"),
                if message.is_empty() {
                    format!("Open {url} and finish there, then choose Done.")
                } else {
                    format!("{message}\n\nOpen {url} and finish there, then choose Done.")
                },
            );
            question.options = vec![ELICIT_DONE.into(), ELICIT_CANCEL.into()];
            return Some(Self::Url { question });
        }
        let mut schema = body.requested_schema.clone().unwrap_or_else(|| json!({}));
        // The server's own field order, where the CLI says it (kept where
        // the JSON map keeps insertion order).
        if !body.property_order.is_empty()
            && let Some(properties) = schema.get("properties").and_then(Value::as_object)
        {
            let mut ordered = serde_json::Map::new();
            for key in &body.property_order {
                if let Some(field) = properties.get(key) {
                    ordered.insert(key.clone(), field.clone());
                }
            }
            for (key, field) in properties {
                if !ordered.contains_key(key) {
                    ordered.insert(key.clone(), field.clone());
                }
            }
            schema["properties"] = Value::Object(ordered);
        }
        crate::codex::elicitation::form(&json!({
            "mode": "form",
            "serverName": server,
            "message": message,
            "requestedSchema": schema,
        }))
        .map(Self::Form)
    }

    pub(super) fn questions(&self) -> Vec<UserInputQuestion> {
        match self {
            Self::Form(form) => form.questions.clone(),
            Self::Url { question } => vec![question.clone()],
        }
    }

    /// The elicitation reply. Nothing answered is a cancel (the person
    /// went away); a refusal declines.
    pub(super) fn response(&self, answers: &[UserInputAnswer]) -> Value {
        match self {
            Self::Form(_) if answers.is_empty() => json!({ "action": "cancel" }),
            Self::Form(form) => form.response(answers),
            Self::Url { question } => {
                match answers
                    .iter()
                    .find(|a| a.question_id == question.id)
                    .and_then(|a| a.labels.first())
                    .map(|l| l.trim())
                {
                    Some(ELICIT_DONE) => json!({ "action": "accept" }),
                    Some(_) => json!({ "action": "decline" }),
                    None => json!({ "action": "cancel" }),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(raw: Value) -> ControlRequestBody {
        serde_json::from_value(raw).unwrap()
    }

    fn answer(ask: &Ask, label: &str) -> Vec<UserInputAnswer> {
        vec![UserInputAnswer {
            question_id: ask.question.id.clone(),
            labels: vec![label.to_owned()],
        }]
    }

    fn bash_ask(extra: Value) -> Ask {
        let mut raw = json!({
            "subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": "toolu_1",
            "input": {"command": "rm -rf build"},
        });
        for (k, v) in extra.as_object().unwrap() {
            raw[k] = v.clone();
        }
        ask(&body(raw), AccessMode::Supervised).expect("asks")
    }

    #[test]
    fn suggestions_offer_this_chat_and_always_as_the_cli_gave_them() {
        let rule = json!({"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "rm -rf build"}],
            "behavior": "allow", "destination": "localSettings"});
        let ask = bash_ask(json!({"permission_suggestions": [rule.clone()]}));
        assert_eq!(
            ask.question.options,
            vec![ALLOW_ONCE, ALLOW_CHAT, ALLOW_ALWAYS, DENY, STOP]
        );
        let input = json!({"command": "rm -rf build"});
        let chat = ask.response(
            &ask.decide(&answer(&ask, ALLOW_CHAT)),
            input.clone(),
            "default",
        );
        assert_eq!(chat["behavior"], "allow");
        assert_eq!(chat["updatedPermissions"][0]["destination"], "session");
        assert_eq!(chat["updatedPermissions"][0]["rules"], rule["rules"]);
        assert_eq!(chat["toolUseID"], "toolu_1");
        assert_eq!(chat["decisionClassification"], "user_temporary");
        let always = ask.response(
            &ask.decide(&answer(&ask, ALLOW_ALWAYS)),
            input.clone(),
            "default",
        );
        assert_eq!(always["updatedPermissions"][0], rule);
        assert_eq!(always["decisionClassification"], "user_permanent");
        let once = ask.response(&ask.decide(&answer(&ask, ALLOW_ONCE)), input, "default");
        assert!(once.get("updatedPermissions").is_none());
    }

    #[test]
    fn no_lasting_rule_without_suggestions_or_when_suppressed() {
        assert_eq!(
            bash_ask(json!({})).question.options,
            vec![ALLOW_ONCE, DENY, STOP]
        );
        let suppressed = bash_ask(json!({
            "permission_suggestions": [{"type": "addRules", "rules": [], "behavior": "allow", "destination": "session"}],
            "suppress_always_allow_rule": true,
        }));
        assert_eq!(suppressed.question.options, vec![ALLOW_ONCE, DENY, STOP]);
        // A session-only suggestion has nothing more to "always" allow.
        let session = bash_ask(json!({
            "permission_suggestions": [{"type": "addRules", "rules": [], "behavior": "allow", "destination": "session"}],
        }));
        assert_eq!(
            session.question.options,
            vec![ALLOW_ONCE, ALLOW_CHAT, DENY, STOP]
        );
    }

    #[test]
    fn deny_carries_a_typed_note_and_stop_ends_the_turn() {
        let ask = bash_ask(json!({}));
        let input = json!({});
        let note = ask.decide(&answer(&ask, "use cargo clean instead"));
        assert_eq!(note, Decision::Deny(Some("use cargo clean instead".into())));
        let denied = ask.response(&note, input.clone(), "default");
        assert_eq!(denied["behavior"], "deny");
        assert_eq!(denied["message"], "use cargo clean instead");
        assert!(denied.get("interrupt").is_none());
        let stop = ask.response(&ask.decide(&answer(&ask, STOP)), input.clone(), "default");
        assert_eq!(stop["interrupt"], true);
        // Nothing answered denies with the stock wording; an option the ask
        // did not offer is only text.
        assert_eq!(ask.decide(&[]), Decision::Deny(None));
        assert_eq!(
            ask.decide(&answer(&ask, ALLOW_ALWAYS)),
            Decision::Deny(Some(ALLOW_ALWAYS.into()))
        );
        assert_eq!(
            ask.response(&Decision::Deny(None), input, "default")["message"],
            APPROVAL_DENIED
        );
    }

    #[test]
    fn the_card_says_why_and_follows_the_clis_answering_rules() {
        let ask = bash_ask(json!({
            "decision_reason": "\u{1b}[1mDangerous\u{1b}[0m rm\n of a\tdirectory",
            "blocked_path": "/work/build",
            "default_to_no": true,
            "requires_user_interaction": true,
        }));
        assert!(
            ask.question
                .question
                .ends_with("\n\nWhy it asks: Dangerous rm of a directory\nPath: /work/build"),
            "{}",
            ask.question.question
        );
        assert_eq!(ask.question.options[0], DENY, "deny comes first");
        let rules = ask.question.approval.clone().unwrap();
        assert!(rules.default_to_no && rules.requires_user_interaction);
        assert!(!rules.allows_quick_answer());
        assert_eq!(bash_ask(json!({})).question.approval, None);
    }

    #[test]
    fn a_plan_is_implemented_or_kept_planning() {
        let ask = ask(
            &body(json!({"subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": {"plan": "p"}})),
            AccessMode::AutoAcceptEdits,
        )
        .unwrap();
        assert_eq!(ask.question.options, vec![IMPLEMENT, KEEP_PLANNING, STOP]);
        let yes = ask.response(
            &ask.decide(&answer(&ask, IMPLEMENT)),
            json!({}),
            "acceptEdits",
        );
        assert_eq!(yes["updatedPermissions"][0]["mode"], "acceptEdits");
        assert_eq!(
            ask.decide(&answer(&ask, KEEP_PLANNING)),
            Decision::Deny(None)
        );
    }

    /// Seeded property: whatever is answered, only an offered allow label
    /// allows, and an allow never carries rules the CLI did not suggest.
    #[test]
    fn only_offered_allow_labels_allow() {
        let labels = [
            ALLOW_ONCE,
            ALLOW_CHAT,
            ALLOW_ALWAYS,
            DENY,
            STOP,
            "",
            "yes",
            "Allow",
        ];
        let mut seed = 0xd1b5_4a32_d192_ed03u64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..500 {
            let suggest = next(2) == 0;
            let suppress = next(3) == 0;
            let suggestions = if suggest {
                json!([{"type": "addRules", "rules": [{"toolName": "Bash"}], "behavior": "allow",
                    "destination": if next(2) == 0 { "session" } else { "userSettings" }}])
            } else {
                json!([])
            };
            let ask = bash_ask(json!({
                "permission_suggestions": suggestions,
                "suppress_always_allow_rule": suppress,
                "default_to_no": next(2) == 0,
            }));
            let label = labels[next(labels.len())];
            let decision = ask.decide(&answer(&ask, label));
            let response = ask.response(&decision, json!({}), "default");
            let allowed = response["behavior"] == "allow";
            assert_eq!(
                allowed,
                ask.question.options.iter().any(|o| o == label)
                    && [ALLOW_ONCE, ALLOW_CHAT, ALLOW_ALWAYS].contains(&label),
                "{label} offered {:?}",
                ask.question.options
            );
            if let Some(rules) = response.get("updatedPermissions") {
                assert!(suggest && !suppress, "rules nobody suggested: {rules}");
            }
        }
    }

    #[test]
    fn elicitations_ask_forms_and_links() {
        let form = Elicitation::new(&body(json!({
            "subtype": "elicitation", "mcp_server_name": "deploy", "message": "Pick",
            "requested_schema": {"type": "object", "properties": {
                "b": {"type": "string"}, "a": {"type": "boolean"}}},
            "property_order": ["b", "a"],
        })))
        .expect("form");
        let questions = form.questions();
        assert_eq!(questions.len(), 2);
        assert_eq!(form.response(&[]), json!({"action": "cancel"}));
        // A question is its field's key (no title or description given).
        let id_of = |key: &str| {
            questions
                .iter()
                .find(|q| q.question.ends_with(key))
                .map(|q| q.id.clone())
                .unwrap()
        };
        let answers = vec![
            UserInputAnswer {
                question_id: id_of("b"),
                labels: vec!["x".into()],
            },
            UserInputAnswer {
                question_id: id_of("a"),
                labels: vec!["Yes".into()],
            },
        ];
        let reply = form.response(&answers);
        assert_eq!(reply["action"], "accept");
        assert_eq!(reply["content"]["b"], "x");
        assert_eq!(reply["content"]["a"], true);

        let url = Elicitation::new(&body(json!({
            "subtype": "elicitation", "mcp_server_name": "github", "mode": "url",
            "url": "https://example.test/auth", "message": "Sign in",
        })))
        .expect("url");
        let q = &url.questions()[0];
        assert!(q.question.contains("https://example.test/auth"));
        let pick = |label: &str| {
            vec![UserInputAnswer {
                question_id: q.id.clone(),
                labels: vec![label.into()],
            }]
        };
        assert_eq!(
            url.response(&pick(ELICIT_DONE)),
            json!({"action": "accept"})
        );
        assert_eq!(
            url.response(&pick(ELICIT_CANCEL)),
            json!({"action": "decline"})
        );
        assert_eq!(url.response(&[]), json!({"action": "cancel"}));
        // A schema the bridge cannot ask is declined, not errored.
        assert!(
            Elicitation::new(&body(json!({"subtype": "elicitation",
                "requested_schema": {"properties": {"n": {"type": "object"}}}})))
            .is_none()
        );
    }

    #[test]
    fn terminal_text_is_cleaned() {
        assert_eq!(
            sanitize("a\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7} b"),
            "alink b"
        );
        assert_eq!(sanitize("\u{1b}[31mred\u{1b}[0m\r\n  x"), "red x");
    }
}
