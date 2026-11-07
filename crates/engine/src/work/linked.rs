//! Linked activity: what changed on the GitHub items sessions link to, read through `gh`. Every
//! item's summary comes in one GraphQL request (REST per item when that fails); an item that
//! changed then has its comments, reviews, review comments and commits read, and a feed read for
//! the same update is reused instead of asked again.
use std::collections::{BTreeMap, HashMap};

use onicode_core::inbox::timestamp_ms;
use onicode_protocol::sessions::{Session, WorkItem};
use onicode_protocol::work::{LinkedCard, LinkedEntry};
use serde_json::Value;

/// The `gh` CLI, asked with `args`; its output. A seam, so the tests script GitHub.
pub trait Gh: Send + Sync + 'static {
    fn run(&self, args: &[String]) -> Result<String, String>;
}

/// The `gh` on this Mac.
pub struct CliGh;

impl Gh for CliGh {
    fn run(&self, args: &[String]) -> Result<String, String> {
        let binary = onicode_core::scm::resolve_gui_binary("gh").ok_or("GitHub CLI not found")?;
        onicode_core::cli::output(
            &binary,
            args,
            std::path::Path::new("/"),
            std::time::Duration::from_secs(20),
        )
    }
}

/// The feeds read, by URL, with the item update they were read for.
pub(crate) type Feeds = HashMap<String, (i64, Value)>;

/// The `jq` filter that keeps what a card shows of a REST reply.
const SHAPE: &str = r#"if type == "array" then map({id,sha,user:{login:.user.login},author:{login:.author.login},body:((.body // "")[0:256]),state,submitted_at,created_at,html_url,commit:{message:((.commit.message // "")[0:256]),committer:{date:.commit.committer.date}}}) else {title,state,merged,updated_at} end"#;

fn api(gh: &dyn Gh, endpoint: &str) -> Result<Value, String> {
    let text = gh.run(&[
        "api".into(),
        "--method".into(),
        "GET".into(),
        endpoint.into(),
        "--jq".into(),
        SHAPE.into(),
    ])?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Only canonical GitHub repository slugs enter a gh API endpoint or query.
pub(crate) fn canonical(item: &WorkItem) -> bool {
    let parts = item.repo.split('/').collect::<Vec<_>>();
    item.number != 0
        && parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        })
}

/// Every linked item's summary in one GraphQL request, shaped like the REST summary. `None` if
/// the request fails, so the caller falls back to REST.
fn summaries(gh: &dyn Gh, items: &[(String, u64)]) -> Option<HashMap<(String, u64), Value>> {
    let mut out = HashMap::new();
    for chunk in items.chunks(50) {
        let mut query = String::from("query{");
        for (ix, (repo, number)) in chunk.iter().enumerate() {
            let (owner, name) = repo.split_once('/')?;
            query.push_str(&format!(
                "i{ix}:repository(owner:\"{owner}\",name:\"{name}\"){{issueOrPullRequest(number:{number}){{...on PullRequest{{title state merged updatedAt}}...on Issue{{title state updatedAt}}}}}}"
            ));
        }
        query.push('}');
        let text = gh
            .run(&[
                "api".into(),
                "graphql".into(),
                "-f".into(),
                format!("query={query}"),
            ])
            .ok()?;
        let body: Value = serde_json::from_str(&text).ok()?;
        let data = body.get("data")?;
        for (ix, item) in chunk.iter().enumerate() {
            let node = &data[format!("i{ix}")]["issueOrPullRequest"];
            if node.is_null() {
                continue;
            }
            // REST reports a merged pull request as closed plus `merged`.
            let state = match node["state"].as_str().unwrap_or("") {
                "MERGED" => "closed".to_owned(),
                state => state.to_ascii_lowercase(),
            };
            out.insert(
                item.clone(),
                serde_json::json!({
                    "title": node["title"],
                    "state": state,
                    "merged": node["merged"].as_bool().unwrap_or(false),
                    "updated_at": node["updatedAt"],
                }),
            );
        }
    }
    Some(out)
}

/// The cards for `sessions`, and the feeds read for them.
pub(crate) fn fetch(
    gh: &dyn Gh,
    sessions: Vec<Session>,
    seen: &BTreeMap<i64, i64>,
    previous: &Feeds,
) -> (Vec<LinkedCard>, Feeds) {
    let mut cards = Vec::new();
    let mut feeds = Feeds::new();
    let sessions: Vec<_> = sessions
        .into_iter()
        .filter(|session| !session.archived && session.work_item.as_ref().is_some_and(canonical))
        .collect();
    let mut items: Vec<(String, u64)> = sessions
        .iter()
        .filter_map(|session| session.work_item.as_ref())
        .map(|item| (item.repo.clone(), item.number))
        .collect();
    items.sort();
    items.dedup();
    let batched = summaries(gh, &items);
    let mut cache: HashMap<String, Option<Value>> = HashMap::new();
    for session in sessions {
        let Some(item) = session.work_item.clone() else {
            continue;
        };
        let base = format!(
            "repos/{}/{}s/{}",
            item.repo,
            if item.kind == "pr" { "pull" } else { "issue" },
            item.number
        );
        let summary = match &batched {
            Some(batched) => batched.get(&(item.repo.clone(), item.number)).cloned(),
            None => cache
                .entry(base.clone())
                .or_insert_with(|| api(gh, &base).ok())
                .clone(),
        };
        let Some(body) = summary else {
            continue;
        };
        let updated_at = timestamp_ms(body["updated_at"].as_str().unwrap_or(""));
        let since = i64::try_from(session.updated_at)
            .unwrap_or(i64::MAX)
            .max(seen.get(&session.id).copied().unwrap_or(0));
        if updated_at <= since {
            continue;
        }
        let mut card = LinkedCard {
            session: session.id,
            title: body["title"].as_str().unwrap_or("").into(),
            state: if body["merged"].as_bool() == Some(true) {
                "merged".into()
            } else {
                body["state"].as_str().unwrap_or("").into()
            },
            updated_at,
            item,
            entries: Vec::new(),
            details_error: false,
            truncated: false,
        };
        let issue_base = format!("repos/{}/issues/{}", card.item.repo, card.item.number);
        let mut wanted = vec![(format!("{issue_base}/comments?per_page=100"), "comment")];
        if card.item.kind == "pr" {
            wanted.extend([
                (format!("{base}/reviews?per_page=100"), "review"),
                (format!("{base}/comments?per_page=100"), "review_comment"),
                (format!("{base}/commits?per_page=100"), "commit"),
            ]);
        }
        for (url, kind) in wanted {
            let body = match previous.get(&url).or_else(|| feeds.get(&url)) {
                Some((at, body)) if *at == updated_at => body.clone(),
                _ => match api(gh, &url) {
                    Ok(body) => body,
                    Err(_) => {
                        card.details_error = true;
                        continue;
                    }
                },
            };
            feeds.insert(url, (updated_at, body.clone()));
            let Some(rows) = body.as_array() else {
                card.details_error = true;
                continue;
            };
            card.truncated |= rows.len() == 100;
            for row in rows {
                if let Some(entry) = entry(row, kind, since) {
                    card.entries.push(entry);
                }
            }
        }
        card.entries
            .sort_by_key(|entry| std::cmp::Reverse(timestamp_ms(&entry.created_at)));
        cards.push(card);
    }
    cards.sort_by_key(|card| card.session);
    (cards, feeds)
}

/// One feed row as a card's entry, if it came after `since`.
fn entry(row: &Value, kind: &str, since: i64) -> Option<LinkedEntry> {
    let commit = kind == "commit";
    let at = if commit {
        row["commit"]["committer"]["date"].as_str()
    } else if kind == "review" {
        row["submitted_at"].as_str()
    } else {
        row["created_at"].as_str()
    }
    .unwrap_or("");
    if timestamp_ms(at) <= since {
        return None;
    }
    let text = if commit {
        row["commit"]["message"]
            .as_str()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .into()
    } else {
        let body = row["body"].as_str().unwrap_or("");
        if kind == "review" {
            format!(
                "{}: {body}",
                row["state"].as_str().unwrap_or("").replace('_', " ")
            )
        } else {
            body.to_owned()
        }
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = if text.chars().count() > 140 {
        format!("{}…", text.chars().take(139).collect::<String>())
    } else {
        text
    };
    Some(LinkedEntry {
        id: if commit {
            row["sha"].as_str().unwrap_or("").into()
        } else {
            row["id"].to_string()
        },
        kind: kind.into(),
        author: row["user"]["login"]
            .as_str()
            .or_else(|| row["author"]["login"].as_str())
            .unwrap_or("")
            .into(),
        text,
        created_at: at.into(),
        url: row["html_url"].as_str().unwrap_or("").into(),
    })
}
