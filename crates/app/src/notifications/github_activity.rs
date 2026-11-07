//! Read-only GitHub polling for linked session notifications: the engine reads GitHub
//! (`LinkedActivity`). No GitHub calls occur until the shell invokes poll; acknowledgment
//! persistence stays in shell.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter};
use onicode_protocol::session_groups::WorkItem;
use onicode_protocol::sessions::Session;
use onicode_protocol::work::{LinkedActivity, LinkedCard};

use crate::engine_client::Engine;
use crate::ui_state::UiState;

const SEEN_KEY: &str = "linked-work-item-seen";
pub(crate) type Entry = onicode_protocol::work::LinkedEntry;
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Card {
    pub session: i64,
    pub item: WorkItem,
    pub title: String,
    pub state: String,
    pub updated_at: i64,
    pub entries: Vec<Entry>,
    pub details_error: bool,
    pub truncated: bool,
}
impl From<LinkedCard> for Card {
    fn from(card: LinkedCard) -> Self {
        Self {
            session: card.session,
            item: card.item,
            title: card.title,
            state: card.state,
            updated_at: card.updated_at,
            entries: card.entries,
            details_error: card.details_error,
            truncated: card.truncated,
        }
    }
}
impl Card {
    pub(crate) fn summary(&self) -> String {
        let count = |kind: &str| {
            self.entries
                .iter()
                .filter(|e| {
                    if kind == "comment" {
                        e.kind == "comment" || e.kind == "review_comment"
                    } else {
                        e.kind == kind
                    }
                })
                .count()
        };
        let parts = [
            (count("commit"), "commit"),
            (count("review"), "review"),
            (count("comment"), "comment"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, k)| format!("{n} new {k}{}", if n == 1 { "" } else { "s" }))
        .collect::<Vec<_>>();
        if !parts.is_empty() {
            parts.join(" · ")
        } else if self.details_error {
            "Updated on GitHub · details unavailable".into()
        } else {
            "Metadata or status changed".into()
        }
    }

    pub(crate) fn terminal_label(&self) -> Option<&'static str> {
        match (
            self.item.kind.as_str(),
            self.state.to_ascii_lowercase().as_str(),
        ) {
            ("pr", "merged") => Some("Pull request merged"),
            ("pr", "closed") => Some("Pull request closed"),
            ("issue", "closed") => Some("Issue closed"),
            _ => None,
        }
    }

    pub(crate) fn prompt(&self) -> String {
        let first = self.entries.first();
        let instruction = match first.map(|e| e.kind.as_str()) {
            Some("commit") => "Review the new commit and continue the work where needed.",
            Some(_) => "Address the new feedback and continue the work where needed.",
            None => "Review the latest update and continue the work where needed.",
        };
        let details = if self.entries.is_empty() {
            vec![format!(
                "- {}; current state: {}",
                self.summary(),
                self.state
            )]
        } else {
            self.entries
                .iter()
                .take(12)
                .map(|e| {
                    format!(
                        "- {}{}: {}",
                        e.kind.replace('_', " "),
                        if e.author.is_empty() {
                            String::new()
                        } else {
                            format!(" by @{}", e.author)
                        },
                        if e.text.is_empty() {
                            "No message"
                        } else {
                            &e.text
                        }
                    )
                })
                .collect()
        };
        format!(
            "{instruction}\n\nThe linked GitHub {} has new activity:\n\n#{} {}\n{}\n\n{}",
            if self.item.kind == "pr" {
                "pull request"
            } else {
                "issue"
            },
            self.item.number,
            self.title,
            self.item.url,
            details.join("\n")
        )
    }
}
pub(crate) fn load_seen(ui_state: &UiState) -> HashMap<i64, i64> {
    ui_state
        .get(SEEN_KEY)
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}
pub(crate) fn save_seen(ui_state: &UiState, seen: &HashMap<i64, i64>) -> Result<(), String> {
    ui_state.set(
        SEEN_KEY,
        serde_json::to_string(seen).map_err(|e| e.to_string())?,
    );
    Ok(())
}
pub(crate) enum GithubActivityEvent {
    Changed,
    Acknowledged(HashMap<i64, i64>),
}
pub(crate) struct GithubActivity {
    engine: Engine,
    pub cards: HashMap<i64, Card>,
    seen: HashMap<i64, i64>,
    checked: Option<Instant>,
    checking: bool,
}
impl EventEmitter<GithubActivityEvent> for GithubActivity {}
impl GithubActivity {
    pub(crate) fn new(engine: Engine, seen: HashMap<i64, i64>) -> Self {
        Self {
            engine,
            cards: HashMap::new(),
            seen,
            checked: None,
            checking: false,
        }
    }

    pub(crate) fn restore_seen(&mut self, seen: HashMap<i64, i64>) {
        for (id, at) in seen {
            let current = self.seen.entry(id).or_default();
            *current = (*current).max(at);
        }
        self.cards
            .retain(|id, card| card.updated_at > *self.seen.get(id).unwrap_or(&0));
    }

    pub(crate) fn acknowledge(&mut self, session: i64, cx: &mut Context<Self>) {
        if let Some(card) = self.cards.remove(&session) {
            let seen = self.seen.entry(session).or_default();
            *seen = (*seen).max(card.updated_at);
            if self.seen.len() > 500 {
                let mut entries: Vec<_> = self.seen.iter().map(|(id, at)| (*id, *at)).collect();
                entries.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
                self.seen = entries.into_iter().take(500).collect();
            }
            cx.emit(GithubActivityEvent::Acknowledged(self.seen.clone()));
            cx.emit(GithubActivityEvent::Changed);
            cx.notify();
        }
    }

    pub(crate) fn ready_to_poll(&self) -> bool {
        !self.checking
            && self
                .checked
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
    }

    pub(crate) fn poll(&mut self, sessions: Vec<Session>, cx: &mut Context<Self>) {
        if self.checking
            || self
                .checked
                .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        self.checking = true;
        self.checked = Some(Instant::now());
        let work = crate::engine_client::call::<LinkedActivity>(
            &self.engine,
            None,
            LinkedActivity {
                sessions,
                seen: self.seen.iter().map(|(id, at)| (*id, *at)).collect(),
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            // A failed read shows nothing new, as a failed `gh` did.
            let cards = work.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.checking = false;
                // An acknowledgment made while polling must not reappear.
                this.cards = cards
                    .into_iter()
                    .map(Card::from)
                    .filter(|c| c.updated_at > *this.seen.get(&c.session).unwrap_or(&0))
                    .map(|c| (c.session, c))
                    .collect();
                cx.emit(GithubActivityEvent::Changed);
                cx.notify();
            });
        })
        .detach();
    }
}
