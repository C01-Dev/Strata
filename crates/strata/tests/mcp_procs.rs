//! PLAN 3.5: dev servers and commands through `strata dev`, `strata run` and the MCP process tools.

mod common;

use common::mcp::{call, mcp};
use common::{Env, initialized, node_server, wait_until};
use rmcp::ServiceExt;
use rmcp::transport::TokioChildProcess;
use serde_json::json;
use std::path::PathBuf;

fn proxy_port(e: &Env) -> String {
    let config = std::fs::read_to_string(e.root.join("strata.toml")).unwrap();
    config
        .lines()
        .find_map(|l| l.strip_prefix("proxy_port = "))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

fn through_proxy(port: &str, host: &str) -> String {
    let out = std::process::Command::new("curl")
        .args([
            "-s",
            "-H",
            &format!("Host: {host}"),
            &format!("http://127.0.0.1:{port}/"),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn strata_dev_prints_the_urls_and_serves_through_the_proxy() {
    let e = initialized();
    e.strata(&["new", "a"]);
    let out = e.strata(&["dev", "a", "--cmd", &node_server("dev a", None)]);
    let proxy = proxy_port(&e);
    assert!(
        out.starts_with(&format!(
            "http://a.app.localhost:{proxy}\ndirect: http://localhost:"
        )),
        "{out}"
    );
    wait_until("routed", || {
        through_proxy(&proxy, &format!("a.app.localhost:{proxy}")) == "dev a"
    });
    assert!(e.strata(&["dev", "a"]).contains("(already running)"));
    e.strata(&["rm", "a", "--force"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_process_tools_act_on_the_callers_own_processes() {
    let e = initialized();
    let a = PathBuf::from(e.strata(&["new", "a"]).trim());
    let b = PathBuf::from(e.strata(&["new", "b"]).trim());
    let client = ().serve(TokioChildProcess::new(mcp(&e, &a)).unwrap()).await.unwrap();

    let (err, text) = call(
        &client,
        "strata_start_dev_server",
        json!({ "command": node_server("mcp a", None) }),
    )
    .await;
    let proxy = proxy_port(&e);
    assert!(
        !err && text.starts_with(&format!(
            "Dev server started: http://a.app.localhost:{proxy}"
        )),
        "{text}"
    );
    let id: i64 = text
        .split("Process ")
        .nth(1)
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    wait_until("served", || {
        through_proxy(&proxy, &format!("a.app.localhost:{proxy}")) == "mcp a"
    });

    let (err, text) = call(
        &client,
        "strata_run",
        json!({ "command": "echo from-run; exit 4" }),
    )
    .await;
    assert!(!err && text.starts_with("Exit code 4.\nfrom-run"), "{text}");
    let (_, text) = call(
        &client,
        "strata_run",
        json!({ "command": "sleep 30", "timeout_s": 1 }),
    )
    .await;
    assert!(text.starts_with("Stopped after 1 s"), "{text}");

    let (_, text) = call(&client, "strata_status", json!({})).await;
    assert!(text.contains(&format!("process {id} (dev)")), "{text}");

    // Another agent's MCP server cannot touch a's processes.
    let other = ().serve(TokioChildProcess::new(mcp(&e, &b)).unwrap()).await.unwrap();
    let (err, text) = call(&other, "strata_stop", json!({ "process_id": id })).await;
    assert!(err && text.contains("no process"), "{text}");
    let (err, _) = call(&other, "strata_logs", json!({ "process_id": id })).await;
    assert!(err);
    other.cancel().await.unwrap();

    let (err, text) = call(&client, "strata_stop", json!({ "process_id": id })).await;
    assert!(!err && text == format!("Stopped process {id}."), "{text}");
    let (_, text) = call(
        &client,
        "strata_logs",
        json!({ "process_id": id, "lines": 5 }),
    )
    .await;
    assert!(text == "No output yet." || !text.is_empty());
    client.cancel().await.unwrap();
}
