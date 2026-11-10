//! The shared checker end to end: `strata check`, status, and an MCP client talking to `strata mcp`.
// MCP roots are deprecated (SEP-2577) but still part of how `strata mcp` finds its agent.
#![allow(deprecated)]

mod common;

use common::{Env, git, initialized, ts_initialized, wait_until, write_file};
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ListRootsResult, Root,
};
use rmcp::service::RequestContext;
use rmcp::transport::TokioChildProcess;
use rmcp::{ClientHandler, ErrorData, RoleClient, ServiceExt};
use serde_json::json;
use std::path::{Path, PathBuf};

fn agent_with_error(e: &Env) -> PathBuf {
    let wt = PathBuf::from(e.strata(&["new", "a"]).trim());
    write_file(
        &wt,
        "src/b.ts",
        "import { addOne } from \"./a\";\nexport const total: string = addOne(2);\n",
    );
    wt
}

#[test]
fn check_prints_diagnostics_and_exits_like_tsc() {
    let e = ts_initialized();
    assert!(
        e.root
            .parent()
            .unwrap()
            .join(".strata/app/.base/node_modules")
            .exists(),
        "base links the checkout's node_modules"
    );
    let wt = agent_with_error(&e);
    let out = e.strata_in(&wt, &["check", "a"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.starts_with("src/b.ts:2:14 error TS2322: Type 'number' is not assignable to type 'string'.\n1 problem."), "{stdout}");
    assert_eq!(e.strata(&["check", "a", "src/a.ts"]).trim(), "No problems.");
    let status = e.status();
    let agent = &status["checker"][0]["agents"][0];
    assert_eq!(
        (
            agent["name"].as_str(),
            agent["sharing"]["mode"].as_str(),
            agent["changed_files"].as_u64()
        ),
        (Some("a"), Some("shared"), Some(1))
    );
    assert_eq!(status["checker"][0]["typescript"], "7.1.0-beta");
    e.strata(&["rm", "a", "--force"]);
    assert_eq!(
        e.status()["checker"][0]["agents"].as_array().unwrap().len(),
        0,
        "rm disposes the snapshot"
    );
}

#[test]
fn check_refuses_unknown_agents() {
    let e = ts_initialized();
    assert!(
        e.strata_err(&["check", "nobody"])
            .contains("no agent named \"nobody\"")
    );
}

#[derive(Clone)]
struct WithRoot(String);

impl ClientHandler for WithRoot {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::builder().enable_roots().build(),
            Implementation::from_build_env(),
        )
    }

    async fn list_roots(
        &self,
        _: RequestContext<RoleClient>,
    ) -> Result<ListRootsResult, ErrorData> {
        Ok(ListRootsResult::new(vec![Root::new(self.0.clone())]))
    }
}

fn mcp_command(e: &Env, cwd: &Path, envs: &[(&str, &str)]) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_strata"));
    cmd.arg("mcp")
        .current_dir(cwd)
        .env("STRATA_HOME", &e.home)
        .env_remove("STRATA_AGENT");
    cmd.envs(envs.iter().copied());
    cmd
}

async fn call<S: ServiceExt<RoleClient> + ClientHandler>(
    client: &rmcp::service::RunningService<RoleClient, S>,
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

#[tokio::test(flavor = "multi_thread")]
async fn mcp_tools_answer_for_the_worktree_they_run_in() {
    let e = ts_initialized();
    let wt = agent_with_error(&e);
    let client =
        ().serve(TokioChildProcess::new(mcp_command(&e, &wt, &[])).unwrap())
            .await
            .unwrap();
    let mut tools: Vec<String> = client
        .list_tools(None)
        .await
        .unwrap()
        .tools
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    tools.sort();
    assert_eq!(
        tools,
        [
            "strata_conflicts",
            "strata_definition",
            "strata_diagnostics",
            "strata_hover",
            "strata_logs",
            "strata_references",
            "strata_run",
            "strata_start_dev_server",
            "strata_status",
            "strata_stop",
            "strata_symbols"
        ]
    );
    let (err, text) = call(&client, "strata_diagnostics", json!({})).await;
    assert!(
        !err && text.contains("src/b.ts:2:14 error TS2322"),
        "{text}"
    );
    let (_, text) = call(
        &client,
        "strata_hover",
        json!({ "file": "src/b.ts", "line": 2, "column": 30 }),
    )
    .await;
    assert_eq!(text, "function addOne: (n: number) => number\n\nAdds one.");
    let (_, text) = call(
        &client,
        "strata_definition",
        json!({ "file": "src/b.ts", "line": 2, "column": 30 }),
    )
    .await;
    assert_eq!(text, "src/a.ts:2:17");
    let (_, text) = call(
        &client,
        "strata_references",
        json!({ "file": "src/a.ts", "line": 2, "column": 17 }),
    )
    .await;
    assert_eq!(text.lines().count(), 3, "{text}");
    let (_, text) = call(&client, "strata_symbols", json!({ "file": "src/a.ts" })).await;
    assert_eq!(text, "addOne (Function) 2:17");
    let (_, text) = call(&client, "strata_status", json!({})).await;
    assert!(
        text.contains("agent: a\n") && text.contains("checker: shared, TypeScript 7.1.0-beta"),
        "{text}"
    );
    let (err, text) = call(
        &client,
        "strata_hover",
        json!({ "file": "../outside.ts", "line": 1, "column": 1 }),
    )
    .await;
    assert!(
        err && text.contains("not in any configured project"),
        "{text}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn conflicts_names_the_agents_it_could_not_check_yet() {
    let e = ts_initialized();
    let wt = agent_with_error(&e);
    let c = PathBuf::from(e.strata(&["new", "c"]).trim());
    write_file(&c, "src/c.ts", "export const c = 1;\n");
    e.strata(&["new", "b"]);
    let client =
        ().serve(TokioChildProcess::new(mcp_command(&e, &wt, &[])).unwrap())
            .await
            .unwrap();
    let (err, text) = call(&client, "strata_conflicts", json!({})).await;
    assert!(!err, "{text}");
    assert_eq!(
        text,
        "No clashes: your changes type-check together with the other agent's.\nNot checked yet: b has no changes so far. A clash with it can still appear after this check."
    );
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_finds_the_agent_from_roots_then_the_environment() {
    let e = ts_initialized();
    let wt = agent_with_error(&e);
    let uri = format!("file://{}", wt.display());
    let outside = e.tmp.path().to_path_buf();
    let client = WithRoot(uri)
        .serve(TokioChildProcess::new(mcp_command(&e, &outside, &[])).unwrap())
        .await
        .unwrap();
    let (err, text) = call(&client, "strata_diagnostics", json!({ "file": "src/b.ts" })).await;
    assert!(!err && text.contains("TS2322"), "{text}");
    client.cancel().await.unwrap();

    let client =
        ().serve(
            TokioChildProcess::new(mcp_command(&e, &outside, &[("STRATA_AGENT", "a")])).unwrap(),
        )
        .await
        .unwrap();
    let (err, text) = call(&client, "strata_diagnostics", json!({})).await;
    assert!(!err && text.contains("TS2322"), "{text}");
    client.cancel().await.unwrap();

    let client =
        ().serve(TokioChildProcess::new(mcp_command(&e, &outside, &[])).unwrap())
            .await
            .unwrap();
    let (err, text) = call(&client, "strata_diagnostics", json!({})).await;
    assert!(
        err && text.contains("cannot tell which agent is calling"),
        "never answers from base: {text}"
    );
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_checker_conflicts_still_names_files_git_cannot_merge() {
    if !strata_git::merges_trees() {
        return; // git older than 2.38
    }
    let e = initialized(); // no TypeScript here, so no shared checker
    let a = PathBuf::from(e.strata(&["new", "a"]).trim());
    let b = PathBuf::from(e.strata(&["new", "b"]).trim());
    write_file(&a, "README.md", "from a\n");
    write_file(&b, "README.md", "from b\n");
    let note = std::fs::read_to_string(a.join(".claude/rules/strata.md")).unwrap();
    assert!(
        note.contains("run `strata_conflicts`") && note.contains("whether git can merge"),
        "{note}"
    );
    let client = ().serve(TokioChildProcess::new(mcp_command(&e, &a, &[])).unwrap()).await.unwrap();
    let (err, text) = call(&client, "strata_conflicts", json!({})).await;
    assert!(!err, "{text}");
    assert_eq!(
        text,
        "You and b both changed README.md and git cannot merge the two versions.\nTypes were not checked together: TypeScript is not installed in node_modules, so the shared checker is off (it needs 7.1 or later)."
    );
    client.cancel().await.unwrap();

    let status = e.status();
    let conflict = &status["checker"][0]["conflicts"][0];
    assert_eq!(conflict["agents"], json!(["a", "b"]), "{status}");
    assert_eq!(conflict["unmerged"], json!(["README.md"]), "{status}");
    let text = e.strata(&["status"]);
    assert!(
        text.contains("no shared checker")
            && text.contains("README.md: both changed it; git cannot merge the two versions"),
        "{text}"
    );
    // The agents' own git state is as they left it, and nothing of Strata's is in the repo.
    assert_eq!(git(&a, &["status", "--porcelain"]), " M README.md");
    assert_eq!(git(&e.root, &["status", "--porcelain"]), "?? strata.toml");

    // A removed agent takes its pairs with it, and the last one Strata's git folder.
    e.strata(&["rm", "b", "--force"]);
    assert_eq!(e.status()["checker"], json!([]));
    e.strata(&["rm", "a", "--force"]);
    wait_until(
        "Strata's git folder for the repo goes with its last agent",
        || std::fs::read_dir(e.home.join("clash")).is_ok_and(|d| d.count() == 0),
    );
}
