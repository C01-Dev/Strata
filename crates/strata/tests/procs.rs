//! PLAN 3.1: supervised processes. Run, stop, logs, escaped children, crash re-adoption,
//! `dev.restart`, and cleanup when an agent goes away.

mod common;

use common::{Env, initialized, wait_until};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::Command;

fn post(e: &Env, path: &str, body: Value) -> Value {
    let out = Command::new("curl")
        .args(["-s", "--unix-socket"])
        .arg(e.home.join("strata.sock"))
        .args([
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "-d",
            &body.to_string(),
            &format!("http://strata{path}"),
        ])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

/// Starts `cmd` as the agent's dev process through the API; returns (process id, pid).
fn dev(e: &Env, agent: &str, cmd: &str) -> (i64, u32) {
    let out = post(
        e,
        &format!("/v1/agents/{agent}/procs"),
        json!({ "repo": e.root, "kind": "dev", "cmd": cmd }),
    );
    let p = &out["process"];
    (
        p["id"].as_i64().unwrap_or_else(|| panic!("{out}")),
        p["pid"].as_u64().unwrap() as u32,
    )
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn process(e: &Env, agent: &str, id: i64) -> Value {
    e.agent(agent).unwrap()["processes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .cloned()
        .unwrap()
}

#[test]
fn run_reports_output_exit_code_and_timeout() {
    let e = initialized();
    let wt = PathBuf::from(e.strata(&["new", "a"]).trim());
    let out = e.strata_in(
        &wt,
        &[
            "run",
            "a",
            "--",
            "sh",
            "-c",
            "pwd; echo agent=$STRATA_AGENT; exit 3",
        ],
    );
    assert_eq!(out.status.code(), Some(3));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!(
            "{}\nagent=a",
            wt.canonicalize().unwrap().display()
        )),
        "{stdout}"
    );
    let out = e.strata_in(&wt, &["run", "a", "--timeout", "1", "--", "sleep", "30"]);
    assert_eq!(
        out.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let newest = &e.agent("a").unwrap()["processes"][0];
    assert_eq!(newest["state"], "stopped");
    assert!(!alive(newest["pid"].as_u64().unwrap() as u32));
    let logs = e.strata(&[
        "logs",
        "a",
        &e.agent("a").unwrap()["processes"][1]["id"].to_string(),
    ]);
    assert!(logs.contains("agent=a"), "{logs}");
}

#[test]
fn rm_stops_the_whole_tree_including_escaped_children() {
    let e = initialized();
    e.strata(&["new", "a"]);
    // A child that leaves the process group (new session), as some dev tools do.
    let (id, pid) = dev(
        &e,
        "a",
        "perl -e 'use POSIX; setsid(); sleep 300' & sleep 300",
    );
    wait_until("escaped child recorded", || {
        e.agent("a").is_some() && descendants(pid).len() >= 2
    });
    let escaped: Vec<u32> = descendants(pid);
    std::thread::sleep(std::time::Duration::from_secs(3)); // one maintenance tick records them
    assert_eq!(process(&e, "a", id)["state"], "running");
    e.strata(&["rm", "a", "--force"]);
    assert!(!alive(pid), "group leader stopped");
    for p in escaped {
        assert!(!alive(p), "pid {p} left behind");
    }
}

fn descendants(pid: u32) -> Vec<u32> {
    let out = Command::new("pgrep")
        .args(["-P", &pid.to_string()])
        .output()
        .unwrap();
    let mut all: Vec<u32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect();
    for p in all.clone() {
        all.extend(descendants(p));
    }
    all
}

#[test]
fn stop_and_logs_through_the_api() {
    let e = initialized();
    e.strata(&["new", "a"]);
    let (id, pid) = dev(&e, "a", "echo serving; sleep 300");
    wait_until("log written", || {
        e.strata(&["logs", "a"]).contains("serving")
    });
    let again = post(
        &e,
        "/v1/agents/a/procs",
        json!({ "repo": e.root, "kind": "dev", "cmd": "sleep 1" }),
    );
    assert_eq!(again["already_running"], true, "one dev server per agent");
    let out = Command::new("curl")
        .args(["-s", "--unix-socket"])
        .arg(e.home.join("strata.sock"))
        .args([
            "-X",
            "DELETE",
            "-H",
            "content-type: application/json",
            "-d",
            r#"{"agent":"someone-else"}"#,
            &format!("http://strata/v1/procs/{id}"),
        ])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("no process"),
        "other agents cannot stop it"
    );
    Command::new("curl")
        .args(["-s", "--unix-socket"])
        .arg(e.home.join("strata.sock"))
        .args(["-X", "DELETE", &format!("http://strata/v1/procs/{id}")])
        .output()
        .unwrap();
    assert!(!alive(pid));
    assert_eq!(process(&e, "a", id)["state"], "stopped");
}

#[test]
fn a_restarted_daemon_re_adopts_running_processes() {
    let e = initialized();
    e.strata(&["new", "a"]);
    let (id, pid) = dev(&e, "a", "sleep 300");
    let daemon = e.status()["pid"].to_string();
    Command::new("kill").args(["-9", &daemon]).status().unwrap();
    assert!(alive(pid), "processes outlive the daemon");
    e.strata(&["daemon", "start"]);
    assert_eq!(
        process(&e, "a", id)["state"],
        "running",
        "re-adopted (pid and start time match)"
    );
    e.strata(&["rm", "a", "--force"]);
    assert!(!alive(pid), "and still stoppable");
}

#[test]
fn processes_gone_after_a_restart_are_dead_unless_dev_restart() {
    let e = initialized();
    let config = std::fs::read_to_string(e.root.join("strata.toml")).unwrap();
    std::fs::write(
        e.root.join("strata.toml"),
        config.replace("restart = false", "restart = true"),
    )
    .unwrap();
    e.strata(&["new", "a"]);
    let (id, pid) = dev(&e, "a", "sleep 300");
    let daemon = e.status()["pid"].to_string();
    Command::new("kill").args(["-9", &daemon]).status().unwrap();
    common::kill_group(pid); // "reboot"
    e.strata(&["daemon", "start"]);
    assert_eq!(process(&e, "a", id)["state"], "dead");
    let newest = &e.agent("a").unwrap()["processes"][0];
    assert_eq!(
        (
            newest["kind"].as_str(),
            newest["state"].as_str(),
            newest["cmd"].as_str()
        ),
        (Some("dev"), Some("running"), Some("sleep 300")),
        "restarted"
    );
    assert_ne!(newest["id"], id);
}

#[test]
fn an_orphaned_agent_loses_its_processes() {
    let e = initialized();
    let wt = PathBuf::from(e.strata(&["new", "a"]).trim());
    let (_, pid) = dev(&e, "a", "sleep 300");
    std::fs::remove_dir_all(&wt).unwrap();
    wait_until("process stopped", || !alive(pid));
    assert_eq!(e.agent("a").unwrap()["state"], "orphaned");
}
