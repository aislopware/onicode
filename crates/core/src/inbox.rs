//! Native Inbox services. Provider adapters preserve the Tauri API's requests,
//! validation, response parsers and credential file formats.
mod avatar;
mod media;
pub use media::png as media_png;
pub mod azure_devops;
pub use avatar::png as avatar_png;
mod github;
pub mod gitlab;
pub mod jira;
pub mod linear;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use onicode_protocol::inbox::*;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::desktop_preferences::DesktopPreferences;
use crate::store::Store;

/// A [`ViewState`] kept in the engine's store.
pub trait ViewStateExt: Sized {
    fn load(store: &Store) -> Self;
    fn save(&self, store: &Store) -> Result<(), String>;
}

impl ViewStateExt for ViewState {
    fn load(store: &Store) -> Self {
        if let Some(state) = store
            .setting("native-inbox-state")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            return state;
        }
        let desktop = store
            .data_dir()
            .map(DesktopPreferences::load)
            .unwrap_or_default();
        let mut state = Self::default();
        if let Some(source) = desktop
            .get("onicode.inboxSource")
            .filter(|s| PROVIDERS.contains(s))
        {
            state.source = source.into();
        }
        if let Some(filters) = desktop
            .json("onicode.inboxFilters")
            .and_then(|v| serde_json::from_value(v).ok())
        {
            state.filters = filters;
        }
        if let Some(seen) = desktop.json("onicode.inboxSeen") {
            state.seen = serde_json::from_value(seen.clone()).unwrap_or_else(|_| Seen {
                seeded: true,
                items: serde_json::from_value(seen).unwrap_or_default(),
            });
        }
        state.hidden_linear_teams = desktop
            .json("onicode.linearHiddenTeams")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        state.hidden_jira_projects = desktop
            .json("onicode.jiraHiddenProjects")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        state
    }

    fn save(&self, store: &Store) -> Result<(), String> {
        store.save_setting(
            "native-inbox-state",
            &serde_json::to_string(self).map_err(|e| e.to_string())?,
        )
    }
}

#[derive(Clone)]
pub struct Service {
    pub data_dir: PathBuf,
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
    pub fn status(&self, provider: &str) -> Result<Status, String> {
        match provider {
            "linear" => convert(linear::linear_status(self.data_dir.clone())?),
            "jira" => convert(jira::jira_status(self.data_dir.clone())?),
            "gitlab" => convert(gitlab::gitlab_status(self.data_dir.clone())?),
            "azuredevops" => convert(azure_devops::azure_devops_status(self.data_dir.clone())?),
            _ => {
                let installed = crate::scm::resolve_gui_binary("gh").is_some();
                let authenticated = installed
                    && run_gh(
                        &self.cwd,
                        &["auth", "status", "--active", "--hostname", "github.com"],
                        true,
                    )
                    .is_ok();
                Ok(Status {
                    installed,
                    authenticated,
                    connected: authenticated,
                    ..Default::default()
                })
            }
        }
    }

    pub fn connect(
        &self,
        provider: &str,
        url: String,
        email: String,
        token: String,
    ) -> Result<Status, String> {
        match provider {
            "linear" => convert(linear::linear_set_token(self.data_dir.clone(), token)?),
            "jira" => convert(jira::jira_set_config(
                self.data_dir.clone(),
                url,
                email,
                token,
            )?),
            "gitlab" => convert(gitlab::gitlab_set_config(
                self.data_dir.clone(),
                url,
                token,
            )?),
            "azuredevops" => convert(azure_devops::azure_devops_set_config(
                self.data_dir.clone(),
                url,
                token,
            )?),
            _ => self.status("github"),
        }
    }

    pub fn list(&self, projects: &[PathBuf], state: &ViewState) -> Snapshot {
        let mut snapshot = Snapshot::default();
        let limit = if state.filters.fetch_state() == "all" {
            100
        } else {
            40
        };
        for provider in PROVIDERS {
            let status = match self.status(provider) {
                Ok(status) => status,
                Err(error) => {
                    snapshot.errors.insert(provider.into(), error);
                    continue;
                }
            };
            let connected = status.connected;
            snapshot.connections.insert(provider.into(), status);
            if !connected {
                continue;
            }
            let result: Result<Vec<Item>, String> = (|| {
                if provider == "linear" {
                    let teams = linear::linear_list_teams(self.data_dir.clone())?;
                    let visible = teams
                        .iter()
                        .filter(|t| !state.hidden_linear_teams.contains(&t.id))
                        .map(|t| t.id.clone())
                        .collect::<Vec<_>>();
                    snapshot.linear_teams = convert(teams)?;
                    if !state.hidden_linear_teams.is_empty() && visible.is_empty() {
                        return Ok(vec![]);
                    }
                    return convert(linear::linear_list_issues(
                        self.data_dir.clone(),
                        state.filters.assigned_to_me,
                        state.filters.fetch_state().into(),
                        if state.hidden_linear_teams.is_empty() {
                            vec![]
                        } else {
                            visible
                        },
                        Some(limit),
                    )?);
                }
                if provider == "jira" {
                    let projects = jira::jira_list_projects(self.data_dir.clone())?;
                    let visible = projects
                        .iter()
                        .filter(|p| !state.hidden_jira_projects.contains(&p.id))
                        .map(|p| p.id.clone())
                        .collect::<Vec<_>>();
                    snapshot.jira_projects = convert(projects)?;
                    if !state.hidden_jira_projects.is_empty() && visible.is_empty() {
                        return Ok(vec![]);
                    }
                    return convert(jira::jira_list_issues(
                        self.data_dir.clone(),
                        state.filters.assigned_to_me,
                        state.filters.fetch_state().into(),
                        if state.hidden_jira_projects.is_empty() {
                            vec![]
                        } else {
                            visible
                        },
                        Some(limit),
                    )?);
                }
                let mut items = Vec::new();
                if state.filters.assigned_to_me && provider != "github" {
                    for kind in ["issue", "pr"] {
                        let rows: Vec<Item> = if provider == "gitlab" {
                            convert(gitlab::gitlab_list_todos(
                                self.data_dir.clone(),
                                kind.into(),
                                Some(limit),
                            )?)?
                        } else {
                            convert(azure_devops::azure_devops_list_todos(
                                self.data_dir.clone(),
                                kind.into(),
                                Some(limit),
                            )?)?
                        };
                        items.extend(rows.into_iter().map(|mut item| {
                            item.provider = provider.into();
                            item
                        }));
                    }
                    return Ok(items);
                }
                let checkouts = repo_checkouts(projects, &self.cwd, |project| {
                    match provider {
                        "github" => github::git_github_repositories_for(project),
                        "gitlab" => gitlab::gitlab_repo(
                            self.data_dir.clone(),
                            project.to_string_lossy().into_owned(),
                        )
                        .map(|r| vec![r]),
                        _ => azure_devops::azure_devops_repo(
                            self.data_dir.clone(),
                            project.to_string_lossy().into_owned(),
                        )
                        .map(|r| vec![r]),
                    }
                    .ok()
                });
                for (project, repo) in checkouts {
                    for kind in ["issue", "pr"] {
                        let rows: Vec<Item> = match provider {
                            "github" => convert(github::git_github_work_items_for(
                                project,
                                &repo,
                                kind,
                                state.filters.assigned_to_me,
                                state.filters.fetch_state(),
                                "",
                                limit,
                            )?)?,
                            "gitlab" => convert(gitlab::gitlab_list_work_items(
                                self.data_dir.clone(),
                                project.to_string_lossy().into_owned(),
                                kind.into(),
                                false,
                                state.filters.fetch_state().into(),
                                Some(limit),
                            )?)?,
                            _ => convert(azure_devops::azure_devops_list_work_items(
                                self.data_dir.clone(),
                                project.to_string_lossy().into_owned(),
                                kind.into(),
                                false,
                                state.filters.fetch_state().into(),
                                Some(limit),
                            )?)?,
                        };
                        items.extend(rows.into_iter().map(|mut item| {
                            item.provider = provider.into();
                            item.project_path = project.to_string_lossy().into_owned();
                            item
                        }));
                    }
                }
                Ok(items)
            })();
            match result {
                Ok(items) => snapshot.items.extend(items),
                Err(error) => {
                    snapshot.errors.insert(provider.into(), error);
                }
            }
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

    pub fn details(&self, item: &Item) -> Result<Details, String> {
        match item.provider.as_str() {
            "linear" => convert(linear::linear_issue_details(
                self.data_dir.clone(),
                item.id.clone(),
            )?),
            "jira" => convert(jira::jira_issue_details(
                self.data_dir.clone(),
                item.identifier.clone(),
            )?),
            "gitlab" => convert(gitlab::gitlab_work_item_details(
                self.data_dir.clone(),
                item.repo.clone(),
                item.kind.clone(),
                item.number,
            )?),
            "azuredevops" => convert(azure_devops::azure_devops_work_item_details(
                self.data_dir.clone(),
                item.repo.clone(),
                item.kind.clone(),
                item.number,
            )?),
            _ => convert(github::git_github_work_item_details_for(
                self.project(item),
                &item.repo,
                &item.kind,
                item.number,
            )?),
        }
    }

    fn project<'a>(&'a self, item: &'a Item) -> &'a Path {
        if item.project_path.is_empty() {
            &self.cwd
        } else {
            Path::new(&item.project_path)
        }
    }

    pub fn thread(&self, item: &Item) -> Result<Thread, String> {
        match item.provider.as_str() {
            "linear" => convert(linear::linear_issue_thread(
                self.data_dir.clone(),
                item.id.clone(),
            )?),
            "jira" => convert(jira::jira_issue_thread(
                self.data_dir.clone(),
                item.identifier.clone(),
            )?),
            "gitlab" => convert(gitlab::gitlab_work_item_thread(
                self.data_dir.clone(),
                item.repo.clone(),
                item.kind.clone(),
                item.number,
            )?),
            "azuredevops" => convert(azure_devops::azure_devops_work_item_thread(
                self.data_dir.clone(),
                item.repo.clone(),
                item.kind.clone(),
                item.number,
            )?),
            _ => convert(github::git_github_work_item_thread_for(
                self.project(item),
                &item.repo,
                &item.kind,
                item.number,
            )?),
        }
    }

    pub fn comment(&self, item: &Item, body: String, reply_to: String) -> Result<String, String> {
        match item.provider.as_str() {
            "linear" => {
                linear::linear_issue_comment(self.data_dir.clone(), item.id.clone(), body, reply_to)
            }
            "jira" => {
                jira::jira_issue_comment(self.data_dir.clone(), item.identifier.clone(), body)
            }
            "gitlab" => gitlab::gitlab_work_item_comment(
                self.data_dir.clone(),
                item.repo.clone(),
                item.kind.clone(),
                item.number,
                body,
            ),
            "azuredevops" => azure_devops::azure_devops_work_item_comment(
                self.data_dir.clone(),
                item.repo.clone(),
                item.kind.clone(),
                item.number,
                body,
            ),
            _ => github::git_github_work_item_comment_for(
                self.project(item),
                &item.repo,
                &item.kind,
                item.number,
                &body,
                &reply_to,
            ),
        }
    }

    pub fn diff(&self, item: &Item) -> Result<Diff, String> {
        self.diff_context(item, false)
    }

    pub fn diff_context(&self, item: &Item, full_context: bool) -> Result<Diff, String> {
        match item.provider.as_str() {
            "gitlab" => convert(gitlab::gitlab_mr_diff(
                self.data_dir.clone(),
                item.repo.clone(),
                item.number,
            )?),
            "azuredevops" => convert(azure_devops::azure_devops_mr_diff(
                self.data_dir.clone(),
                item.repo.clone(),
                item.number,
            )?),
            "github" => convert(github::git_github_pr_diff_for(
                self.project(item),
                &item.repo,
                item.number,
                full_context,
            )?),
            _ => Err("This issue has no pull request diff".into()),
        }
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
        updated.provider = item.provider.clone();
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
    let mut item: Item = convert(github::git_github_work_item_for(
        &gh_root(cwd),
        repo,
        kind,
        number,
    )?)?;
    item.provider = "github".into();
    Ok(item)
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

    fn item(number: i64, provider: &str) -> Item {
        Item {
            provider: provider.into(),
            kind: "issue".into(),
            repo: "acme/app".into(),
            number,
            id: number.to_string(),
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
                .observe(&snapshot(vec![item(1, "github")]), "scope".into())
                .is_empty()
        );
        let mut updated = item(1, "github");
        updated.updated_at = "2026-10-21T11:00:00Z".into();
        let appeared = tracker.observe(&snapshot(vec![updated, item(2, "github")]), "scope".into());
        assert_eq!(
            appeared.iter().map(|i| i.number).collect::<Vec<_>>(),
            vec![2]
        );
        assert!(
            tracker
                .observe(&snapshot(vec![item(2, "github")]), "scope".into())
                .is_empty()
        );
    }

    #[test]
    fn failed_provider_has_its_own_successful_baseline() {
        let mut tracker = NotificationTracker::default();
        let mut failed = snapshot(vec![item(1, "linear")]);
        failed.errors.insert("linear".into(), "offline".into());
        assert!(tracker.observe(&failed, "scope".into()).is_empty());
        assert!(
            tracker
                .observe(&snapshot(vec![item(2, "linear")]), "scope".into())
                .is_empty()
        );
        let appeared = tracker.observe(
            &snapshot(vec![item(2, "linear"), item(3, "linear")]),
            "scope".into(),
        );
        assert_eq!(appeared.len(), 1);
        assert_eq!(appeared[0].number, 3);
    }

    #[test]
    fn changing_query_baselines_new_scope_without_forgetting_known_items() {
        let mut tracker = NotificationTracker::default();
        assert!(
            tracker
                .observe(&snapshot(vec![item(1, "github")]), "one".into())
                .is_empty()
        );
        assert!(
            tracker
                .observe(&snapshot(vec![item(2, "github")]), "two".into())
                .is_empty()
        );
        assert!(
            tracker
                .observe(
                    &snapshot(vec![item(1, "github"), item(2, "github")]),
                    "two".into()
                )
                .is_empty()
        );
        assert_eq!(
            tracker
                .observe(&snapshot(vec![item(3, "github")]), "two".into())
                .len(),
            1
        );
    }

    #[test]
    fn seen_keys_match_desktop_identity_rules() {
        let mut github = item(12, "github");
        github.url = "https://GitHub.com/Acme/App/pull/12/".into();
        assert_eq!(github.key(), "github:acme/app:issue:12");
        let mut linear = item(7, "linear");
        linear.id = "stable-id".into();
        linear.identifier = "MON-7".into();
        assert_eq!(linear.key(), "linear:mon-7");
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
