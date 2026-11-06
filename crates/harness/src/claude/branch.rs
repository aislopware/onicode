//! Resolving a [`Branch`] into Claude Code's truncating-resume flags.
//!
//! The CLI resumes up to one transcript entry (`--resume-session-at <uuid>`),
//! can refuse a cut that holds more than one turn (`--resume-drops-turn
//! <prompt uuid>`), and can copy the history into a new session
//! (`--fork-session`). The host names prompts by the ids they were written
//! under; which entry ends the kept history is read off the session's
//! transcript: its records chain by `uuid` / `parentUuid`.

use std::path::Path;

use serde_json::Value;

use crate::Branch;

/// The flags one branch launches with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BranchArgs {
    /// `--resume-session-at`: the last entry kept.
    pub at: String,
    /// `--resume-drops-turn`: the prompt whose turn is dropped.
    pub drops_turn: Option<String>,
    /// `--fork-session`.
    pub fork: bool,
}

impl BranchArgs {
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = vec![format!("--resume-session-at={}", self.at)];
        if let Some(prompt) = &self.drops_turn {
            args.push(format!("--resume-drops-turn={prompt}"));
        }
        if self.fork {
            args.push("--fork-session".into());
        }
        args
    }
}

/// One main-chain transcript record, as far as branching cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub uuid: String,
    pub parent: Option<String>,
    /// A prompt: a user record carrying text, not tool results.
    pub prompt: bool,
}

/// Read the session's main chain from `<config>/projects/*/<id>.jsonl`.
pub(crate) fn read_entries(config_root: &Path, session_id: &str) -> Result<Vec<Entry>, String> {
    use std::io::BufRead as _;
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err("the conversation has no Claude session to branch".into());
    }
    let projects = std::fs::read_dir(config_root.join("projects"))
        .map_err(|_| "Claude's session history is not on this device".to_owned())?;
    let file = projects
        .flatten()
        .find_map(|project| {
            std::fs::File::open(project.path().join(format!("{session_id}.jsonl"))).ok()
        })
        .ok_or_else(|| "Claude's history has no transcript for this session".to_owned())?;
    let mut entries = Vec::new();
    for line in std::io::BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
    {
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        if !(text.contains("\"type\":\"user\"") || text.contains("\"type\":\"assistant\"")) {
            continue;
        }
        if let Some(entry) = serde_json::from_str::<Value>(text)
            .ok()
            .as_ref()
            .and_then(entry)
        {
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// A transcript record as an [`Entry`]; `None` for sidechains (subagent
/// records), meta injections and anything not a user or assistant message.
pub(crate) fn entry(record: &Value) -> Option<Entry> {
    let kind = record.get("type").and_then(Value::as_str)?;
    if !matches!(kind, "user" | "assistant")
        || record.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let uuid = record.get("uuid").and_then(Value::as_str)?.to_owned();
    let parent = record
        .get("parentUuid")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let content = record.pointer("/message/content");
    let prompt = kind == "user"
        && record.get("isMeta").and_then(Value::as_bool) != Some(true)
        && match content {
            Some(Value::String(text)) => !text.trim().is_empty(),
            Some(Value::Array(blocks)) => {
                !blocks
                    .iter()
                    .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                    && blocks
                        .iter()
                        .any(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            }
            _ => false,
        };
    Some(Entry {
        uuid,
        parent,
        prompt,
    })
}

/// The flags for `branch` over a session's main chain `entries` (file
/// order). A prompt the transcript does not hold, or a redo of anything but
/// the last turn, is refused here rather than by the CLI mid-launch.
pub(crate) fn resolve(entries: &[Entry], branch: &Branch) -> Result<BranchArgs, String> {
    let message_id = match branch {
        Branch::Redo { message_id } | Branch::Fork { message_id, .. } => message_id,
    };
    let at = entries
        .iter()
        .position(|e| e.prompt && e.uuid == *message_id)
        .ok_or_else(|| "Claude's transcript does not hold that message".to_owned())?;
    let before = || {
        entries[at].parent.clone().ok_or_else(|| {
            "that message opens the conversation: start a new chat instead".to_owned()
        })
    };
    // The turn runs to the entry before the next prompt.
    let next_prompt = entries[at + 1..]
        .iter()
        .position(|e| e.prompt)
        .map(|n| at + 1 + n);
    match branch {
        Branch::Redo { .. } => {
            if next_prompt.is_some() {
                return Err(
                    "only the last message can be edited in place; branch the chat to edit an earlier one"
                        .into(),
                );
            }
            Ok(BranchArgs {
                at: before()?,
                drops_turn: Some(message_id.clone()),
                fork: false,
            })
        }
        Branch::Fork {
            keep_turn: false, ..
        } => Ok(BranchArgs {
            at: before()?,
            drops_turn: None,
            fork: true,
        }),
        Branch::Fork {
            keep_turn: true, ..
        } => {
            let last = next_prompt.unwrap_or(entries.len()) - 1;
            Ok(BranchArgs {
                at: entries[last].uuid.clone(),
                drops_turn: None,
                fork: true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(uuid: &str, parent: Option<&str>, prompt: bool) -> Entry {
        Entry {
            uuid: uuid.into(),
            parent: parent.map(Into::into),
            prompt,
        }
    }

    /// p1 → a1 → r1 (tool result) → a2 ; p2 → a3
    fn chain() -> Vec<Entry> {
        vec![
            e("p1", None, true),
            e("a1", Some("p1"), false),
            e("r1", Some("a1"), false),
            e("a2", Some("r1"), false),
            e("p2", Some("a2"), true),
            e("a3", Some("p2"), false),
        ]
    }

    #[test]
    fn a_redo_drops_the_last_turn_only() {
        let args = resolve(
            &chain(),
            &Branch::Redo {
                message_id: "p2".into(),
            },
        )
        .unwrap();
        assert_eq!(
            args.args(),
            vec!["--resume-session-at=a2", "--resume-drops-turn=p2"]
        );
        assert!(
            resolve(
                &chain(),
                &Branch::Redo {
                    message_id: "p1".into()
                }
            )
            .is_err()
        );
        assert!(
            resolve(
                &chain(),
                &Branch::Redo {
                    message_id: "zz".into()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn a_fork_keeps_through_or_up_to_its_turn() {
        let through = Branch::Fork {
            message_id: "p1".into(),
            keep_turn: true,
        };
        assert_eq!(
            resolve(&chain(), &through).unwrap().args(),
            vec!["--resume-session-at=a2", "--fork-session"]
        );
        let last = Branch::Fork {
            message_id: "p2".into(),
            keep_turn: true,
        };
        assert_eq!(resolve(&chain(), &last).unwrap().at, "a3");
        let before = Branch::Fork {
            message_id: "p2".into(),
            keep_turn: false,
        };
        assert_eq!(resolve(&chain(), &before).unwrap().at, "a2");
        // Nothing comes before the opening prompt.
        assert!(
            resolve(
                &chain(),
                &Branch::Fork {
                    message_id: "p1".into(),
                    keep_turn: false
                }
            )
            .is_err()
        );
    }

    /// Model test over random chains: the kept history always ends on an
    /// entry the chain holds, never inside a later turn, and a redo or a
    /// fork-before never keeps the named prompt itself.
    #[test]
    fn resolved_cuts_respect_turn_boundaries() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..500 {
            let mut entries: Vec<Entry> = Vec::new();
            for i in 0..(2 + next(30)) {
                let prompt = i == 0 || next(4) == 0;
                let parent = entries.last().map(|e| e.uuid.clone());
                entries.push(Entry {
                    uuid: format!("u{i}"),
                    parent,
                    prompt,
                });
            }
            let prompts: Vec<usize> = (0..entries.len()).filter(|&i| entries[i].prompt).collect();
            for (k, &p) in prompts.iter().enumerate() {
                let id = entries[p].uuid.clone();
                let next_prompt = prompts.get(k + 1).copied();
                for branch in [
                    Branch::Redo {
                        message_id: id.clone(),
                    },
                    Branch::Fork {
                        message_id: id.clone(),
                        keep_turn: true,
                    },
                    Branch::Fork {
                        message_id: id.clone(),
                        keep_turn: false,
                    },
                ] {
                    let Ok(args) = resolve(&entries, &branch) else {
                        continue;
                    };
                    let cut = entries
                        .iter()
                        .position(|e| e.uuid == args.at)
                        .expect("held");
                    match branch {
                        Branch::Fork {
                            keep_turn: true, ..
                        } => {
                            assert!(cut >= p);
                            assert!(next_prompt.is_none_or(|n| cut < n));
                        }
                        _ => assert_eq!(cut + 1, p),
                    }
                    if let Branch::Redo { .. } = branch {
                        assert!(next_prompt.is_none(), "redo of a non-last turn");
                    }
                }
            }
        }
    }

    #[test]
    fn records_read_as_entries() {
        let prompt = serde_json::json!({"type":"user","uuid":"u","parentUuid":"a",
            "message":{"role":"user","content":"hi"}});
        assert_eq!(entry(&prompt), Some(e("u", Some("a"), true)));
        let result = serde_json::json!({"type":"user","uuid":"r","parentUuid":"a",
            "message":{"content":[{"type":"tool_result","tool_use_id":"t"}]}});
        assert_eq!(entry(&result).map(|e| e.prompt), Some(false));
        let side = serde_json::json!({"type":"assistant","uuid":"s","isSidechain":true});
        assert_eq!(entry(&side), None);
        let meta = serde_json::json!({"type":"user","uuid":"m","isMeta":true,
            "message":{"content":"<system-reminder>"}});
        assert_eq!(entry(&meta).map(|e| e.prompt), Some(false));
    }
}
