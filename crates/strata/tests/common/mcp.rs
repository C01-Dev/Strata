//! An MCP client on `strata mcp`, for tests that talk to Strata the way agents do.

use super::Env;
use rmcp::model::CallToolRequestParams;
use std::path::Path;

/// Calls `tool`; returns whether it was an error, and its text.
pub async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    tool: &'static str,
    args: serde_json::Value,
) -> (bool, String) {
    let r = client
        .call_tool(
            CallToolRequestParams::new(tool).with_arguments(args.as_object().cloned().unwrap()),
        )
        .await
        .unwrap();
    (
        r.is_error == Some(true),
        r.content[0]
            .as_text()
            .map(|t| t.text.clone())
            .unwrap_or_default(),
    )
}

/// `strata mcp` in `cwd`, the way an agent's MCP config starts it.
pub fn mcp(e: &Env, cwd: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_strata"));
    cmd.arg("mcp")
        .current_dir(cwd)
        .env("STRATA_HOME", &e.home)
        .env_remove("STRATA_AGENT");
    cmd
}
