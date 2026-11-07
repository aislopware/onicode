//! Native Inbox services: the GitHub issues and pull requests of a person's projects, read and
//! acted on through the GitHub CLI.
mod avatar;
mod media;
pub use avatar::png as avatar_png;
pub use media::png as media_png;
mod github;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use onicode_protocol::inbox::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Where an Inbox call runs: the checkout `gh` runs in.
#[derive(Clone)]
pub struct Service {
    pub cwd: PathBuf,
}
/// Pairs each repository with one checkout to fetch it through. Several
/// checkouts of the same repository share its rows, so the active checkout
/// goes first and keeps them when the inbox is scoped to it.
fn repo_checkouts<'a>(
    projects: &'a [PathBuf],
    cwd: &Path,
    mut repos: impl FnMut(&Path) -> Option<Vec<String>>,
) -> Vec<(&'a PathBuf, String)> {
    let mut ordered = projects.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|project| project.as_path() != cwd);
    let mut visited = HashSet::new();
    let mut checkouts = vec![];
    for project in ordered {
        for repo in repos(project).unwrap_or_default() {
            if visited.insert(repo.to_lowercase()) {
                checkouts.push((project, repo));
            }
        }
    }
    checkouts
}
fn convert<T: Serialize, U: DeserializeOwned>(value: T) -> Result<U, String> {
    serde_json::from_value(serde_json::to_value(value).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}
impl Service {
    /// GitHub's connection: whether `gh` is installed and signed in to github.com.
    pub fn status(&self) -> Status {
        let installed = crate::scm::resolve_gui_binary("gh").is_some();
        let authenticated = installed
            && run_gh(
                &self.cwd,
                &["auth", "status", "--active", "--hostname", "github.com"],
                true,
            )
            .is_ok();
        Status {
            installed,
            authenticated,
            connected: authenticated,
        }
    }

    pub fn list(&self, projects: &[PathBuf], state: &ViewState) -> Snapshot {
        let mut snapshot = Snapshot {
            connection: self.status(),
            ..Snapshot::default()
        };
        if !snapshot.connection.connected {
            return snapshot;
        }
        match self.items(projects, state) {
            Ok(items) => snapshot.items = items,
            Err(error) => snapshot.error = Some(error),
        }
        snapshot.items.sort_by(|a, b| {
            b.updated_ms()
                .cmp(&a.updated_ms())
                .then_with(|| a.title.cmp(&b.title))
        });
        let mut seen = HashSet::new();
        snapshot.items.retain(|i| seen.insert(i.key()));
        snapshot
    }

    /// The issues and pull requests of every repository `projects` check out, each fetched once.
    fn items(&self, projects: &[PathBuf], state: &ViewState) -> Result<Vec<Item>, String> {
        let limit = if state.filters.fetch_state() == "all" {
            100
        } else {
            40
        };
        let mut items = Vec::new();
        let checkouts = repo_checkouts(projects, &self.cwd, |project| {
            github::git_github_repositories_for(project).ok()
        });
        for (project, repo) in checkouts {
            for kind in ["issue", "pr"] {
                let rows: Vec<Item> = convert(github::git_github_work_items_for(
                    project,
                    &repo,
                    kind,
                    state.filters.assigned_to_me,
                    state.filters.fetch_state(),
                    "",
                    limit,
                )?)?;
                items.extend(rows.into_iter().map(|mut item| {
                    item.project_path = project.to_string_lossy().into_owned();
                    item
                }));
            }
        }
        Ok(items)
    }

    pub fn details(&self, item: &Item) -> Result<Details, String> {
        convert(github::git_github_work_item_details_for(
            self.project(item),
            &item.repo,
            &item.kind,
            item.number,
        )?)
    }

    fn project<'a>(&'a self, item: &'a Item) -> &'a Path {
        if item.project_path.is_empty() {
            &self.cwd
        } else {
            Path::new(&item.project_path)
        }
    }

    pub fn thread(&self, item: &Item) -> Result<Thread, String> {
        convert(github::git_github_work_item_thread_for(
            self.project(item),
            &item.repo,
            &item.kind,
            item.number,
        )?)
    }

    pub fn comment(&self, item: &Item, body: String, reply_to: String) -> Result<String, String> {
        github::git_github_work_item_comment_for(
            self.project(item),
            &item.repo,
            &item.kind,
            item.number,
            &body,
            &reply_to,
        )
    }

    pub fn diff(&self, item: &Item) -> Result<Diff, String> {
        convert(github::git_github_pr_diff_for(
            self.project(item),
            &item.repo,
            item.number,
            false,
        )?)
    }

    pub fn checks(&self, item: &Item) -> Result<Checks, String> {
        convert(github::git_github_pr_checks_for(
            self.project(item),
            &item.repo,
            item.number,
        )?)
    }

    pub fn action(&self, item: &Item, action: &str) -> Result<Item, String> {
        let mut updated: Item = convert(github::git_github_pr_action_for(
            self.project(item),
            &item.repo,
            item.number,
            action,
        )?)?;
        updated.project_path = item.project_path.clone();
        Ok(updated)
    }
}
pub(super) fn run_git(root: &Path, args: &[&str]) -> Result<String, String> {
    run_command("git", root, args, true)
}
/// githubWorkItem: one GitHub issue or pull request a message links to,
/// looked up with `gh` from `cwd` (any folder, when the session's is gone).
pub fn github_work_item(cwd: &Path, repo: &str, kind: &str, number: i64) -> Result<Item, String> {
    convert(github::git_github_work_item_for(
        &gh_root(cwd),
        repo,
        kind,
        number,
    )?)
}
/// githubWorkItemDetails for [`github_work_item`]'s item.
pub fn github_work_item_details(
    cwd: &Path,
    repo: &str,
    kind: &str,
    number: i64,
) -> Result<Details, String> {
    convert(github::git_github_work_item_details_for(
        &gh_root(cwd),
        repo,
        kind,
        number,
    )?)
}
fn gh_root(cwd: &Path) -> PathBuf {
    let cwd = crate::scm::expand_home(cwd);
    if cwd.is_dir() {
        cwd
    } else {
        crate::scm::shell::dirs_home()
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
    }
}
pub(super) fn run_gh(root: &Path, args: &[&str], allow_empty: bool) -> Result<String, String> {
    run_command("gh", root, args, allow_empty)
}
fn run_command(
    program: &str,
    root: &Path,
    args: &[&str],
    allow_empty: bool,
) -> Result<String, String> {
    let binary = crate::scm::resolve_gui_binary(program).ok_or_else(|| {
        format!(
            "{} is not installed. Install {} and check again.",
            if program == "gh" {
                "GitHub CLI (`gh`)"
            } else {
                program
            },
            program
        )
    })?;
    let mut command = Command::new(binary);
    command
        .current_dir(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("GIT_PAGER", "cat");
    crate::scm::shell::apply_gui_env(&mut command);
    crate::process::configure_child(&mut command);
    let output = command.output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() { stdout } else { stderr });
    }
    if stdout.is_empty() && !allow_empty {
        Err(format!("{program} returned no output"))
    } else {
        Ok(stdout)
    }
}

#[cfg(test)]
mod notification_tests {
    use super::*;

    fn item(number: i64) -> Item {
        Item {
            kind: "issue".into(),
            repo: "acme/app".into(),
            number,
            updated_at: "2026-10-21T10:00:00Z".into(),
            ..Item::default()
        }
    }
    fn snapshot(items: Vec<Item>) -> Snapshot {
        Snapshot {
            items,
            ..Snapshot::default()
        }
    }

    #[test]
    fn first_baseline_and_existing_revisions_never_appear() {
        let mut tracker = NotificationTracker::default();
        assert!(
            tracker
                .observe(&snapshot(vec![item(1)]), "scope".into())
                .is_empty()
        );
        let mut updated = item(1);
        updated.updated_at = "2026-10-21T11:00:00Z".into();
        let appeared = tracker.observe(&snapshot(vec![updated, item(2)]), "scope".into());
        assert_eq!(
            appeared.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![2]
        );
        assert!(
            tracker
                .observe(&snapshot(vec![item(2)]), "scope".into())
                .is_empty()
        );
    }

    #[test]
    fn a_failed_fetch_sets_no_baseline() {
        let mut tracker = NotificationTracker::default();
        let mut failed = snapshot(vec![item(1)]);
        failed.error = Some("offline".into());
        assert!(tracker.observe(&failed, "scope".into()).is_empty());
        assert!(
            tracker
                .observe(&snapshot(vec![item(2)]), "scope".into())
                .is_empty()
        );
        let appeared = tracker.observe(&snapshot(vec![item(2), item(3)]), "scope".into());
        assert_eq!(appeared.len(), 1);
        assert_eq!(appeared[0].number, 3);
    }

    #[test]
    fn changing_query_baselines_new_scope_without_forgetting_known_items() {
        let mut tracker = NotificationTracker::default();
        assert!(
            tracker
                .observe(&snapshot(vec![item(1)]), "one".into())
                .is_empty()
        );
        assert!(
            tracker
                .observe(&snapshot(vec![item(2)]), "two".into())
                .is_empty()
        );
        assert!(
            tracker
                .observe(&snapshot(vec![item(1), item(2)]), "two".into())
                .is_empty()
        );
        assert_eq!(
            tracker
                .observe(&snapshot(vec![item(3)]), "two".into())
                .len(),
            1
        );
    }

    #[test]
    fn seen_keys_match_desktop_identity_rules() {
        let mut github = item(12);
        github.url = "https://GitHub.com/Acme/App/pull/12/".into();
        assert_eq!(github.key(), "acme/app:issue:12");
        github.repo.clear();
        assert_eq!(github.key(), "https://github.com/acme/app/pull/12/");
    }

    #[test]
    fn two_checkouts_of_one_repository_fetch_it_through_the_active_one() {
        let first = PathBuf::from("/work/app");
        let second = PathBuf::from("/work/app-review");
        let other = PathBuf::from("/work/site");
        let projects = [first.clone(), second.clone(), other.clone()];
        let repos = |project: &Path| {
            Some(vec![if project == other {
                "acme/site".to_string()
            } else {
                "Acme/App".to_string()
            }])
        };
        for (cwd, owner) in [(&first, &first), (&second, &second), (&other, &first)] {
            let checkouts = repo_checkouts(&projects, cwd, repos);
            assert_eq!(checkouts.len(), 2, "each repository is fetched once");
            assert!(
                checkouts
                    .iter()
                    .any(|(project, repo)| *project == owner && repo == "Acme/App"),
                "from {cwd:?}: {checkouts:?}"
            );
        }
    }
}
