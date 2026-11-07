//! What a turn did to its checkout, read from Git itself: where HEAD, its upstream and the index
//! stood when the turn started ([`Mark`]) and, when it ended, the commits HEAD gained, what is
//! staged and whether the upstream moved to HEAD ([`TurnGit`]). Every read is plumbing or
//! porcelain-v2 output under `LC_ALL=C`; nothing reads the agent's commands or what they printed.
use std::path::Path;

pub use onicode_protocol::git::{CommitFile, TurnCommit, TurnGit, TurnPush};

/// A turn reports at most this many of the commits HEAD gained, the newest.
const COMMITS: usize = 50;

/// Where a checkout stood: HEAD, its upstream's commit and what is staged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mark {
    head: Option<String>,
    upstream: Option<String>,
    staged: Vec<CommitFile>,
}

/// `git status --porcelain=v2 --branch`'s headers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Branch {
    oid: Option<String>,
    head: Option<String>,
    upstream: Option<String>,
    ahead: u64,
    behind: u64,
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let mut command = crate::process::git();
    crate::process::configure_child(&mut command);
    let output = command
        .args([
            "--no-pager",
            "--no-optional-locks",
            "-c",
            "core.quotepath=false",
            "-C",
        ])
        .arg(root)
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Where the checkout at `cwd` stands; `None` outside a Git work tree.
pub fn mark(cwd: &Path) -> Option<Mark> {
    read(&crate::scm::expand_home(cwd)).map(|(mark, _)| mark)
}

/// What moved in the checkout at `cwd` since `before`: the commits HEAD gained, the index when it
/// changed, and the push when the upstream moved to HEAD. `None` when none of them moved.
pub fn turn_git(cwd: &Path, before: &Mark) -> Option<TurnGit> {
    let root = crate::scm::expand_home(cwd);
    let (after, branch) = read(&root)?;
    if after == *before {
        return None;
    }
    let commits = match &after.head {
        Some(head) if after.head != before.head => {
            let range = before
                .head
                .as_ref()
                .map_or_else(|| head.clone(), |old| format!("{old}..{head}"));
            commits(&root, &range)
        }
        _ => Vec::new(),
    };
    let staged = if after.staged == before.staged {
        Vec::new()
    } else {
        after.staged.clone()
    };
    let push = branch
        .upstream
        .as_ref()
        .filter(|_| after.upstream.is_some() && after.upstream != before.upstream)
        .filter(|_| after.upstream == after.head)
        .map(|upstream| push(&root, branch.head.as_deref(), upstream));
    if commits.is_empty() && staged.is_empty() && push.is_none() {
        return None;
    }
    Some(TurnGit {
        branch: branch.head,
        commits,
        staged,
        ahead: branch.ahead,
        behind: branch.behind,
        push,
    })
}

fn read(root: &Path) -> Option<(Mark, Branch)> {
    let branch = parse_branch(&git(
        root,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=no",
            "--ignore-submodules=all",
        ],
    )?);
    let upstream = branch
        .upstream
        .as_ref()
        .and_then(|_| git(root, &["rev-parse", "--verify", "-q", "@{upstream}"]))
        .map(|oid| oid.trim().to_owned())
        .filter(|oid| !oid.is_empty());
    let staged = git(
        root,
        &[
            "diff",
            "--cached",
            "--numstat",
            "-z",
            "--no-renames",
            "--no-ext-diff",
            "--no-color",
        ],
    )
    .map(|text| parse_numstat_z(&text))
    .unwrap_or_default();
    Some((
        Mark {
            head: branch.oid.clone(),
            upstream,
            staged,
        },
        branch,
    ))
}

fn parse_branch(text: &str) -> Branch {
    let mut branch = Branch::default();
    for line in text.lines() {
        let Some((key, value)) = line
            .strip_prefix("# branch.")
            .and_then(|header| header.split_once(' '))
        else {
            continue;
        };
        match key {
            "oid" => branch.oid = (value != "(initial)").then(|| value.to_owned()),
            "head" => branch.head = (value != "(detached)").then(|| value.to_owned()),
            "upstream" => branch.upstream = Some(value.to_owned()),
            "ab" => {
                for count in value.split_whitespace() {
                    if let Some(ahead) = count.strip_prefix('+') {
                        branch.ahead = ahead.parse().unwrap_or(0);
                    } else if let Some(behind) = count.strip_prefix('-') {
                        branch.behind = behind.parse().unwrap_or(0);
                    }
                }
            }
            _ => {}
        }
    }
    branch
}

/// `diff --numstat -z` without renames: `added\tdeleted\tpath\0` per file, `-` for binary counts.
fn parse_numstat_z(text: &str) -> Vec<CommitFile> {
    text.split('\0')
        .filter_map(|entry| {
            let mut parts = entry.splitn(3, '\t');
            let additions = parts.next()?.parse().ok();
            let deletions = parts.next()?.parse().ok();
            let path = parts.next().filter(|path| !path.is_empty())?.to_owned();
            Some(CommitFile {
                path,
                additions,
                deletions,
            })
        })
        .collect()
}

/// The commits of `range`, oldest first, with their `--numstat` counts.
fn commits(root: &Path, range: &str) -> Vec<TurnCommit> {
    let count = format!("--max-count={COMMITS}");
    let Some(text) = git(
        root,
        &[
            "log",
            "--no-color",
            "--no-renames",
            "--no-ext-diff",
            "--format=%x1e%H%x1f%s",
            "--numstat",
            &count,
            range,
            "--",
        ],
    ) else {
        return Vec::new();
    };
    let mut commits = parse_log(&text);
    commits.reverse();
    commits
}

/// `log --format=%x1e%H%x1f%s --numstat`: a record per commit, newest first.
fn parse_log(text: &str) -> Vec<TurnCommit> {
    text.split('\x1e')
        .filter_map(|record| {
            let mut lines = record.lines();
            let (hash, subject) = lines.next()?.split_once('\x1f')?;
            let mut commit = TurnCommit {
                hash: hash.to_owned(),
                subject: subject.to_owned(),
                ..TurnCommit::default()
            };
            for line in lines {
                let mut parts = line.splitn(3, '\t');
                let (Some(additions), Some(deletions), Some(_)) =
                    (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                commit.files += 1;
                commit.additions += additions.parse::<u64>().unwrap_or(0);
                commit.deletions += deletions.parse::<u64>().unwrap_or(0);
            }
            Some(commit)
        })
        .collect()
}

fn push(root: &Path, branch: Option<&str>, upstream: &str) -> TurnPush {
    let remote = branch
        .and_then(|branch| {
            git(
                root,
                &["config", "--get", &format!("branch.{branch}.remote")],
            )
        })
        .map(|remote| remote.trim().to_owned())
        .filter(|remote| !remote.is_empty() && remote != ".");
    let hosted = remote
        .and_then(|remote| git(root, &["remote", "get-url", "--", &remote]))
        .and_then(|url| web_url(url.trim()));
    TurnPush {
        upstream: upstream.to_owned(),
        repo: hosted.as_ref().map(|(_, repo)| repo.clone()),
        web: hosted.map(|(web, _)| web),
    }
}

/// A hosted remote's web page and `owner/repo`, from its fetch URL.
fn web_url(remote: &str) -> Option<(String, String)> {
    let remote = remote.trim().trim_end_matches('/');
    let remote = remote.strip_suffix(".git").unwrap_or(remote);
    let (host, path) = if let Some(rest) = remote
        .strip_prefix("https://")
        .or_else(|| remote.strip_prefix("http://"))
        .or_else(|| remote.strip_prefix("ssh://"))
    {
        let (host, path) = rest.split_once('/')?;
        let host = host.rsplit('@').next()?;
        (host.split(':').next()?, path)
    } else {
        let (user_host, path) = remote.split_once(':')?;
        if user_host.contains('/') {
            return None;
        }
        (user_host.rsplit('@').next()?, path)
    };
    let path = path.trim_matches('/');
    if host.is_empty() || !host.contains('.') || path.split('/').count() < 2 {
        return None;
    }
    Some((format!("https://{host}/{path}"), path.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn run(cwd: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(cwd)
            .env("GIT_AUTHOR_NAME", "Onicode")
            .env("GIT_AUTHOR_EMAIL", "onicode@test")
            .env("GIT_COMMITTER_NAME", "Onicode")
            .env("GIT_COMMITTER_EMAIL", "onicode@test")
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}: {status:?}");
    }

    /// A checkout on `main` with one commit, and a bare remote it does not track yet.
    fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("project");
        let remote = temp.path().join("remote.git");
        std::fs::create_dir(&cwd).unwrap();
        run(temp.path(), &["init", "--bare", "-b", "main", "remote.git"]);
        run(&cwd, &["init", "-b", "main"]);
        run(&cwd, &["config", "core.autocrlf", "false"]);
        std::fs::write(cwd.join("a.rs"), "one\n").unwrap();
        run(&cwd, &["add", "."]);
        run(&cwd, &["commit", "-m", "Base"]);
        run(&cwd, &["remote", "add", "origin", remote.to_str().unwrap()]);
        (temp, cwd, remote)
    }

    fn commit(cwd: &Path, file: &str, text: &str, subject: &str) {
        std::fs::write(cwd.join(file), text).unwrap();
        run(cwd, &["add", "--", file]);
        run(cwd, &["commit", "-m", subject]);
    }

    #[test]
    fn a_turn_that_moved_nothing_reports_nothing() {
        let (_temp, cwd, _) = repo();
        let before = mark(&cwd).unwrap();
        std::fs::write(cwd.join("a.rs"), "edited, never staged\n").unwrap();
        assert_eq!(turn_git(&cwd, &before), None);
        assert!(mark(&cwd.parent().unwrap().join("nowhere")).is_none());
    }

    #[test]
    fn commits_come_from_the_log_oldest_first_with_their_counts() {
        let (_temp, cwd, _) = repo();
        let before = mark(&cwd).unwrap();
        commit(&cwd, "a.rs", "one\ntwo\nthree\n", "Grow a");
        std::fs::write(cwd.join("b.bin"), [0_u8, 1, 2, 0]).unwrap();
        std::fs::write(cwd.join("c d.rs"), "x\n").unwrap();
        run(&cwd, &["add", "."]);
        run(&cwd, &["commit", "-m", "Add a binary and a spaced path"]);
        let git = turn_git(&cwd, &before).unwrap();
        assert_eq!(git.branch.as_deref(), Some("main"));
        let shown: Vec<_> = git
            .commits
            .iter()
            .map(|c| (c.subject.as_str(), c.files, c.additions, c.deletions))
            .collect();
        assert_eq!(
            shown,
            [
                ("Grow a", 1, 2, 0),
                ("Add a binary and a spaced path", 2, 1, 0)
            ]
        );
        assert!(git.commits.iter().all(|c| c.hash.len() == 40));
        assert!(git.staged.is_empty(), "the commit took what was staged");
        assert_eq!(git.push, None, "no upstream moved");
    }

    #[test]
    fn staging_alone_reports_the_index() {
        let (_temp, cwd, _) = repo();
        let before = mark(&cwd).unwrap();
        std::fs::write(cwd.join("a.rs"), "uno\n").unwrap();
        std::fs::write(cwd.join("new.rs"), "1\n2\n").unwrap();
        run(&cwd, &["add", "--", "a.rs", "new.rs"]);
        let git = turn_git(&cwd, &before).unwrap();
        assert!(git.commits.is_empty());
        assert_eq!(
            git.staged,
            [
                CommitFile {
                    path: "a.rs".into(),
                    additions: Some(1),
                    deletions: Some(1),
                },
                CommitFile {
                    path: "new.rs".into(),
                    additions: Some(2),
                    deletions: Some(0),
                },
            ]
        );
        // A later turn that leaves the same index reports nothing new.
        assert_eq!(turn_git(&cwd, &mark(&cwd).unwrap()), None);
    }

    #[test]
    fn pushes_are_the_upstream_moving_to_head() {
        let (_temp, cwd, _) = repo();
        let before = mark(&cwd).unwrap();
        commit(&cwd, "a.rs", "two\n", "Change a");
        run(&cwd, &["push", "-u", "origin", "main"]);
        let git = turn_git(&cwd, &before).unwrap();
        assert_eq!(git.commits.len(), 1);
        assert_eq!((git.ahead, git.behind), (0, 0));
        let push = git.push.unwrap();
        assert_eq!(push.upstream, "origin/main");
        assert_eq!(push.web, None, "a local remote has no web page");

        let before = mark(&cwd).unwrap();
        commit(&cwd, "a.rs", "three\n", "Change a again");
        let unpushed = turn_git(&cwd, &before).unwrap();
        assert_eq!((unpushed.ahead, unpushed.push), (1, None));
        run(&cwd, &["push"]);
        let pushed = turn_git(&cwd, &before).unwrap();
        assert_eq!(pushed.ahead, 0);
        assert_eq!(pushed.push.unwrap().upstream, "origin/main");

        // Pushing what was already committed: the upstream moves, HEAD does not.
        let before = mark(&cwd).unwrap();
        commit(&cwd, "a.rs", "four\n", "Committed earlier");
        let before_push = mark(&cwd).unwrap();
        run(&cwd, &["push"]);
        let git = turn_git(&cwd, &before_push).unwrap();
        assert!(git.commits.is_empty() && git.push.is_some());
        assert_ne!(before, before_push);
    }

    #[test]
    fn a_detached_head_has_no_branch() {
        let (_temp, cwd, _) = repo();
        run(&cwd, &["checkout", "--detach"]);
        let before = mark(&cwd).unwrap();
        commit(&cwd, "a.rs", "detached\n", "On no branch");
        let git = turn_git(&cwd, &before).unwrap();
        assert_eq!(git.branch, None);
        assert_eq!(git.commits[0].subject, "On no branch");
    }

    #[test]
    fn porcelain_headers_and_numstat_records_parse() {
        let branch = parse_branch(
            "# branch.oid (initial)\n# branch.head (detached)\n1 M. N... 100644 100644 100644 a b a.rs\n",
        );
        assert_eq!(branch, Branch::default());
        let branch = parse_branch(
            "# branch.oid abc\n# branch.head feature/x\n# branch.upstream origin/feature/x\n# branch.ab +3 -2\n",
        );
        assert_eq!(branch.oid.as_deref(), Some("abc"));
        assert_eq!(branch.head.as_deref(), Some("feature/x"));
        assert_eq!(branch.upstream.as_deref(), Some("origin/feature/x"));
        assert_eq!((branch.ahead, branch.behind), (3, 2));
        assert_eq!(
            parse_numstat_z("-\t-\tlogo.png\x004\t0\ta\tb.rs\x00"),
            [
                CommitFile {
                    path: "logo.png".into(),
                    additions: None,
                    deletions: None,
                },
                CommitFile {
                    path: "a\tb.rs".into(),
                    additions: Some(4),
                    deletions: Some(0),
                },
            ]
        );
        let log = parse_log("\x1enew\x1fSecond\n\n1\t2\ta.rs\n-\t-\tlogo.png\n\x1eold\x1fFirst\n");
        assert_eq!(
            log.iter()
                .map(|c| (c.hash.as_str(), c.files, c.additions, c.deletions))
                .collect::<Vec<_>>(),
            [("new", 2, 1, 2), ("old", 0, 0, 0)]
        );
    }

    #[test]
    fn hosted_remotes_have_web_pages() {
        assert_eq!(
            web_url("git@github.com:o/r.git"),
            Some(("https://github.com/o/r".into(), "o/r".into()))
        );
        assert_eq!(
            web_url("ssh://git@gitlab.com:2222/group/sub/r.git"),
            Some((
                "https://gitlab.com/group/sub/r".into(),
                "group/sub/r".into()
            ))
        );
        assert_eq!(
            web_url("https://token@github.com/o/r"),
            Some(("https://github.com/o/r".into(), "o/r".into()))
        );
        assert_eq!(web_url("/srv/git/r.git"), None);
        assert_eq!(web_url("../remote.git"), None);
    }
}
