//! Stable conversation links shared by sidebar copy actions and inbound URL routing.

use onicode_notify::links::encode_component;
// The link format itself is shared with the zgx app (crates/notify).
pub use onicode_notify::links::{
    ConversationDeepLink, conversation_link as onicode_conversation_link,
    parse_conversation_link as parse_onicode_conversation_link,
};
use onicode_proto::{Chat, HarnessId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessConversationLink {
    pub label: &'static str,
    pub url: String,
}

/// Opaque locator: enough to reject links for another profile without putting
/// the device id in the URL. `None` until the engine names this device.
pub fn workspace_locator(local_device_id: Option<&str>) -> Option<String> {
    Some(onicode_notify::links::workspace_locator_of(
        "Local",
        &format!("device:{}", local_device_id?),
    ))
}

/// Only return schemes verified against the harness app. Hermes exposes a
/// candidate scheme, but its contract is not stable enough to put on users'
/// clipboards yet.
pub fn harness_conversation_link(chat: &Chat) -> Option<HarnessConversationLink> {
    let id = chat.harness_session_id.as_deref()?.trim();
    if id.is_empty() || chat.config.as_ref()?.harness != HarnessId::Codex {
        return None;
    }
    Some(HarnessConversationLink {
        label: "Codex conversation link",
        url: format!("codex://threads/{}", encode_component(id)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn harness_chat(harness: HarnessId) -> Chat {
        Chat {
            id: "chat".into(),
            device_id: "device".into(),
            title: None,
            archived: false,
            cwd: None,
            branch: None,
            checkout_id: None,
            source_context: None,
            config: Some(onicode_proto::ChatConfig {
                harness,
                model: None,
                reasoning: None,
                model_options: Default::default(),
                sandbox: onicode_proto::SandboxLevel::WorkspaceWrite,
                access: Default::default(),
                plan: false,
            }),
            last_message_preview: None,
            last_message_at: None,
            created_at: chrono::DateTime::UNIX_EPOCH,
            harness_session_id: Some("thread/one".into()),
            harness_session_cwd: None,
            harness_session_harness: None,
            parent_chat_id: None,
            child_kind: None,
            space_id: None,
            last_seen_at: None,
        }
    }

    #[test]
    fn workspace_locator_waits_for_device_identity() {
        assert_eq!(workspace_locator(None), None);
        let first = workspace_locator(Some("device-a"));
        let second = workspace_locator(Some("device-b"));
        assert!(first.is_some());
        assert_ne!(first, second);
    }

    #[test]
    fn codex_link_is_exact_and_unverified_harnesses_are_omitted() {
        assert_eq!(
            harness_conversation_link(&harness_chat(HarnessId::Codex))
                .unwrap()
                .url,
            "codex://threads/thread%2Fone"
        );
        assert!(harness_conversation_link(&harness_chat(HarnessId::Hermes)).is_none());
    }
}
