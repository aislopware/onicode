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
/// engine.
pub(super) async fn discover(engine: Engine, cwd: String) -> Discovery {
    let harnesses = engine.call::<Harnesses>(None, Harnesses {}).await;
    let skills = engine.call::<Skills>(
        None,
        Skills {
            project: cwd.clone(),
            all: true,
        },
    );
    let mcp = engine.call::<McpServers>(
        None,
        McpServers {
            project: cwd.clone(),
        },
    );
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
            path: PathBuf::from(s.path),
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
                    path: s.config_path.into(),
                    transport: s.transport,
                    status: if s.enabled { "Configured" } else { "Disabled" }.into(),
                })
                .collect()
        }
        Err(e) => data.mcp_error = Some(e),
    }
    if !cwd.is_empty() {
        let refs = engine.call::<Refs>(
            None,
            Refs {
                checkout: cwd.clone(),
            },
        );
        match refs.await {
            Ok(s) => {
                if let Some(main) = s.worktrees.first() {
                    data.worktrees.main = main.path.clone();
                    data.worktrees.default_root =
                        onicode_core::worktree_management::default_root(Path::new(&main.path));
                }
                data.worktrees.branches = s.branches;
                let trees: Vec<_> = s.worktrees.into_iter().filter(|t| !t.main).collect();
                let marks = engine
                    .call::<ReadWorktreeMarks>(
                        None,
                        ReadWorktreeMarks {
                            checkout: cwd,
                            trees: trees.clone(),
                        },
                    )
                    .await
                    .unwrap_or_default();
                data.worktrees.trees = trees
                    .into_iter()
                    .map(|tree| {
                        let mark = marks
                            .iter()
                            .find(|mark| mark.path == tree.path)
                            .cloned()
                            .unwrap_or_default();
                        Tree {
                            tree,
                            locked: mark.locked,
                            dirty: mark.dirty,
                            unpushed: mark.unpushed,
                        }
                    })
                    .collect();
            }
            Err(e) => data.worktrees.error = Some(e),
        }
    }
    data
}
