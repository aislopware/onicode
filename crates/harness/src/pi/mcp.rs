//! Load the existing Onicode delegation tools into this Pi process only.
use crate::HarnessError;
use crate::process::Command;
use crate::scratch::ScratchDir;

/// The bridge extension reads its server from this variable.
const CONFIG_ENV: &str = "ONICODE_PI_MCP";

pub(super) fn configure(
    cmd: &mut Command,
    config: &onicode_proto::McpServer,
) -> Result<ScratchDir, HarnessError> {
    let scratch = ScratchDir::new("pi-mcp")?;
    let extension = scratch.path().join("onicode-mcp.mjs");
    std::fs::write(&extension, include_str!("mcp.mjs"))?;
    cmd.arg("--extension").arg(extension).env(
        CONFIG_ENV,
        serde_json::to_string(config).expect("serializable MCP config"),
    );
    Ok(scratch)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn configure_loads_the_bridge_with_onicode_s_server() {
        assert!(
            include_str!("mcp.mjs").contains("process.env.ONICODE_PI_MCP)"),
            "the bridge reads its config from ONICODE_PI_MCP"
        );
        let onicode = onicode_proto::McpServer {
            name: "onicode".into(),
            command: "/opt/onicode/onicode".into(),
            args: vec!["mcp".into()],
            env: Default::default(),
        };
        let mut cmd = Command::new("pi");
        let scratch = configure(&mut cmd, &onicode).unwrap();
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.iter().filter(|a| *a == "--extension").count(), 1);
        assert!(scratch.path().join("onicode-mcp.mjs").exists());
        let config = cmd
            .as_std()
            .get_envs()
            .find(|(k, _)| k.to_str() == Some(CONFIG_ENV))
            .and_then(|(_, v)| v)
            .map(|v| v.to_string_lossy().into_owned())
            .unwrap();
        let config: onicode_proto::McpServer = serde_json::from_str(&config).unwrap();
        assert_eq!(config, onicode);
    }
}
