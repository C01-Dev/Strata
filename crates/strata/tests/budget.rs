//! PLAN 3.2: per-agent memory, budgets (warn, or stop the agent's processes).

mod common;

use common::{Env, initialized, wait_until};
use serde_json::{Value, json};
use std::process::Command;

fn start_dev(e: &Env, agent: &str, cmd: &str) -> Value {
    let body = json!({ "repo": e.root, "kind": "dev", "cmd": cmd }).to_string();
    let out = Command::new("curl")
        .args(["-s", "--unix-socket"])
        .arg(e.home.join("strata.sock"))
        .args([
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "-d",
            &body,
            &format!("http://strata/v1/agents/{agent}/procs"),
        ])
        .output()
        .unwrap();
    serde_json::from_slice::<Value>(&out.stdout).unwrap()["process"].clone()
}

fn with_budget(action: &str) -> Env {
    let e = initialized();
    let config = std::fs::read_to_string(e.root.join("strata.toml")).unwrap();
    let config = config
        .replace("per_agent_mb = 4096", "per_agent_mb = 10")
        .replace("action = \"warn\"", &format!("action = \"{action}\""));
    std::fs::write(e.root.join("strata.toml"), config).unwrap();
    e.strata(&["new", "a"]);
    e
}

const HUNGRY: &str = r#"perl -e '$x = "a" x 60_000_000; sleep 300'"#;

fn events(e: &Env) -> Vec<String> {
    e.status()["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ev| ev["kind"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn memory_is_metered_and_over_budget_warns() {
    let e = with_budget("warn");
    let p = start_dev(&e, "a", HUNGRY);
    wait_until("over budget", || e.agent("a").unwrap()["budget"] == "over");
    let agent = e.agent("a").unwrap();
    assert!(
        agent["memory_bytes"].as_u64().unwrap() > 50 * 1048576,
        "{agent}"
    );
    assert!(events(&e).contains(&"budget.exceeded".to_string()));
    assert_eq!(agent["processes"][0]["state"], "running", "warn only");
    assert!(e.strata(&["status"]).contains("(over budget)"));
    Command::new("kill")
        .args(["-9", &format!("-{}", p["pid"])])
        .status()
        .unwrap();
}

#[test]
fn over_budget_with_stop_stops_the_processes_not_the_agent() {
    let e = with_budget("stop");
    let p = start_dev(&e, "a", HUNGRY);
    wait_until("processes stopped", || {
        e.agent("a").unwrap()["processes"][0]["state"] == "stopped"
    });
    let pid = p["pid"].to_string();
    assert!(
        !Command::new("kill")
            .args(["-0", &pid])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
    // The event is written right after the stop completes.
    wait_until("budget.stopped event", || {
        events(&e).contains(&"budget.stopped".to_string())
    });
    assert_eq!(e.agent("a").unwrap()["state"], "ready");
}
