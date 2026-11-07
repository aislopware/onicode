//! MCP servers as the providers configure them.
use serde::{Deserialize, Serialize};

/// One MCP server as a provider's configuration names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpConnection {
    pub provider: String,
    pub name: String,
    pub scope: String,
    pub config_path: String,
    pub transport: String,
    pub enabled: bool,
}
