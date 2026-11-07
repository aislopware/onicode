//! The sidebar's session grouping: what reads the checkout to link a work item. The grouping
//! itself lives in `onicode-protocol`.
use std::path::Path;

pub use onicode_protocol::session_groups::*;

use crate::session_title::WorkItemHint;

/// resolveLinkedWorkItem: a first message's explicit GitHub context, as one
/// stable identity. A URL wins; then a numbered reference (or the title
/// model's hint) in the working copy's repository; then "this PR", meaning
/// the current branch's pull request. Lookups that fail link nothing.
pub fn resolve_work_item(
    message: &str,
    cwd: &Path,
    generated: Option<&WorkItemHint>,
) -> Option<WorkItem> {
    if let Some(item) = parse_github_work_item(message) {
        return Some(item);
    }
    let hint = explicit_work_item_hint(message).or_else(|| {
        let hint = generated?;
        let kind = match hint.kind.as_str() {
            "pr" => "pr",
            "issue" => "issue",
            _ => return None,
        };
        (hint.number > 0).then_some((kind, hint.number))
    });
    if let Some((kind, number)) = hint {
        let repo = crate::scm::github_repo(cwd)
            .ok()
            .filter(|r| valid_repo(r))?;
        return Some(WorkItem {
            kind: kind.into(),
            url: github_url(&repo, kind, number),
            repo,
            number,
        });
    }
    if !references_current_pr(message) {
        return None;
    }
    let pr = crate::scm::pr_status(cwd).ok()??;
    let number = u64::try_from(pr.number).ok().filter(|&n| n > 0)?;
    let repo = parse_github_work_item(&pr.url)
        .map(|item| item.repo)
        .or_else(|| crate::scm::github_repo(cwd).ok())
        .filter(|r| valid_repo(r))?;
    let url = if pr.url.is_empty() {
        github_url(&repo, "pr", number)
    } else {
        pr.url
    };
    Some(WorkItem {
        kind: "pr".into(),
        repo,
        number,
        url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_messages_link_their_explicit_github_context() {
        // A pasted URL needs no repository lookup, whatever the folder is.
        let none = Path::new("/nonexistent/monocode-test");
        let hint = WorkItemHint {
            kind: "issue".into(),
            number: 9,
        };
        assert_eq!(
            resolve_work_item(
                "hey lets review this pr\n\nhttps://github.com/acme/app/pull/1142 its for the native one",
                none,
                Some(&hint),
            )
            .map(|item| (item.kind, item.repo, item.number)),
            Some(("pr".into(), "acme/app".into(), 1142))
        );
        assert_eq!(resolve_work_item("Fix the sidebar", none, None), None);

        assert_eq!(
            explicit_work_item_hint("look at PR #42 please"),
            Some(("pr", 42))
        );
        assert_eq!(explicit_work_item_hint("pull request 7"), Some(("pr", 7)));
        assert_eq!(explicit_work_item_hint("see Issue#3"), Some(("issue", 3)));
        for text in ["PR #0", "issue tracker", "approach 12", "prs 5"] {
            assert_eq!(explicit_work_item_hint(text), None, "{text}");
        }
        assert!(references_current_pr("review the current PR"));
        assert!(references_current_pr("land this pull request"));
        assert!(!references_current_pr("open a PR for this"));
    }
}
