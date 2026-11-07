//! Settings discovery runs on a worker; rendering never probes CLIs or Git.
use std::path::{Path, PathBuf};

use onicode_protocol::agents::{Harnesses, McpServers, Skills};
use onicode_protocol::git;
use onicode_protocol::git::Refs;
use onicode_protocol::provider::Kind;
use onicode_protocol::settings_pages::ReadWorktreeMarks;

use crate::engine_client::Engine;
#[derive(Clone, Debug)]
pub(super) struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub scope: String,
    pub provider: String,
}
#[derive(Clone, Debug)]
pub(super) struct McpServer {
    pub name: String,
    pub provider: String,
    pub scope: String,
    pub path: PathBuf,
    pub transport: String,
    pub status: String,
}
#[derive(Default)]
pub(super) struct Discovery {
    pub binaries: Vec<(Kind, Option<PathBuf>)>,
    pub skills: Vec<Skill>,
    pub mcp: Vec<McpServer>,
    pub mcp_error: Option<String>,
    pub worktrees: ResultTrees,
}
#[derive(Clone, Debug)]
pub(super) struct Tree {
    pub tree: git::Worktree,
    pub locked: bool,
    pub dirty: Option<bool>,
    pub unpushed: usize,
}
#[derive(Default)]
pub(super) struct ResultTrees {
    pub trees: Vec<Tree>,
    pub branches: Vec<git::Branch>,
    pub default_root: PathBuf,
    pub main: String,
    pub error: Option<String>,
}
/// Everything the Skills, MCP, Providers and Worktrees pages show for `cwd`, read through the
/// engine. The skills and MCP servers of a project on another machine are that machine's (its
/// personal ones and the project's own, which its agents use), read through its engine and
/// shown in the app's spelling.
pub(super) async fn discover(engine: Engine, cwd: String) -> Discovery {
    let harnesses = engine.call::<Harnesses>(None, Harnesses {}).await;
    let (project_engine, project) = engine.for_path(&cwd);
    let skills = project_engine.call::<Skills>(
        None,
        Skills {
            project: project.clone(),
            all: true,
        },
    );
    let mcp = project_engine.call::<McpServers>(None, McpServers { project });
    let spell = |path: String| {
        if path.is_empty() {
            path
        } else {
            project_engine.app_path(&path)
        }
    };
    let harnesses = harnesses.unwrap_or_default();
    let mut data = Discovery {
        binaries: Kind::ALL
            .into_iter()
            .map(|kind| {
                let path = harnesses
                    .iter()
                    .find(|harness| harness.provider == kind)
                    .and_then(|harness| harness.path.as_ref())
                    .map(PathBuf::from);
                (kind, path)
            })
            .collect(),
        ..Default::default()
    };
    data.skills = skills
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|s| Skill {
            name: s.name,
            description: s.description,
            path: PathBuf::from(spell(s.path)),
            scope: s.scope,
            provider: s.source,
        })
        .collect();
    match mcp.await {
        Ok(servers) => {
            data.mcp = servers
                .into_iter()
                .map(|s| McpServer {
                    name: s.name,
                    provider: s.provider,
                    scope: s.scope,
                    path: spell(s.config_path).into(),
                    transport: s.transport,
                    status: if s.enabled { "Configured" } else { "Disabled" }.into(),
                })
                .collect()
        }
        Err(e) => data.mcp_error = Some(e),
    }
    if !cwd.is_empty() {
        data.worktrees = worktrees(&engine, &cwd).await;
    }
    data
}

/// The Worktrees page for the project `cwd`, read through the engine that holds it (a project
/// on another machine's), its paths in the app's spelling.
async fn worktrees(engine: &Engine, cwd: &str) -> ResultTrees {
    let (engine, checkout) = engine.for_path(cwd);
    let snapshot = match engine
        .call::<Refs>(
            None,
            Refs {
                checkout: checkout.clone(),
            },
        )
        .await
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return ResultTrees {
                error: Some(error),
                ..ResultTrees::default()
            };
        }
    };
    let mut found = ResultTrees::default();
    if let Some(main) = snapshot.worktrees.first() {
        found.main = engine.app_path(&main.path);
        // Shown as the machine that makes them names it.
        found.default_root = onicode_core::worktree_management::default_root(Path::new(&main.path));
    }
    found.branches = snapshot.branches;
    let trees: Vec<_> = snapshot.worktrees.into_iter().filter(|t| !t.main).collect();
    let marks = engine
        .call::<ReadWorktreeMarks>(
            None,
            ReadWorktreeMarks {
                checkout,
                trees: trees.clone(),
            },
        )
        .await
        .unwrap_or_default();
    found.trees = trees
        .into_iter()
        .map(|mut tree| {
            let mark = marks
                .iter()
                .find(|mark| mark.path == tree.path)
                .cloned()
                .unwrap_or_default();
            tree.path = engine.app_path(&tree.path);
            Tree {
                tree,
                locked: mark.locked,
                dirty: mark.dirty,
                unpushed: mark.unpushed,
            }
        })
        .collect();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A project on another machine's skills and MCP servers are that machine's: Settings asks
    /// its engine for them, by the path it names the project by, never this Mac's disk.
    #[gpui::test]
    fn a_peer_projects_skills_and_mcp_servers_are_read_on_its_machine(
        cx: &mut gpui::TestAppContext,
    ) {
        let engine = cx.update(Engine::memory);
        let asked = engine.clone();
        let task =
            cx.spawn(async move |_| discover(asked, "peer://nStudio/Users/me/app".into()).await);
        cx.run_until_parked();
        let found = futures::executor::block_on(task);
        assert!(found.skills.is_empty());
        assert!(found.mcp_error.unwrap_or_default().contains("offline"));
        let calls = cx.update(|cx| crate::engine_client::calls_for_test(&engine, cx));
        for asked in ["Skills", "McpServers"] {
            let (_, params) = calls
                .iter()
                .find(|(name, _)| *name == asked)
                .unwrap_or_else(|| panic!("{asked} is asked of the studio"));
            assert_eq!(params["engine"], "nStudio", "{asked}");
            assert_eq!(params["body"]["project"], "/Users/me/app", "{asked}");
        }
        let (_, harnesses) = calls
            .iter()
            .find(|(name, _)| *name == "Harnesses")
            .expect("this Mac's providers");
        assert!(harnesses["engine"].is_null(), "the app's own engine");
    }

    /// A project on another machine lists its worktrees through that machine's engine, by the
    /// path it names the checkout by; one that cannot be reached says why on the page.
    #[gpui::test]
    fn a_peer_projects_worktrees_are_read_on_its_machine(cx: &mut gpui::TestAppContext) {
        let engine = cx.update(Engine::memory);
        let found = futures::executor::block_on(worktrees(&engine, "peer://nStudio/Users/me/app"));
        let error = found.error.unwrap_or_default();
        assert!(error.contains("offline"), "{error}");
        let calls = cx.update(|cx| crate::engine_client::calls_for_test(&engine, cx));
        let (_, refs) = calls
            .iter()
            .find(|(name, _)| *name == "Refs")
            .expect("the studio is asked");
        assert_eq!(refs["engine"], "nStudio");
        assert_eq!(refs["body"]["checkout"], "/Users/me/app");
    }
}
