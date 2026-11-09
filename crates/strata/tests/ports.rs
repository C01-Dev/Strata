//! PLAN 3.3: port registry, listener confirmation, unmanaged listeners.

mod common;

use common::{initialized, node_server, set_ports, start_dev, wait_until};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

fn port_of(e: &common::Env, agent: &str) -> u64 {
    e.agent(agent).unwrap()["port"].as_u64().unwrap()
}

#[test]
fn agents_get_distinct_free_ports_and_rm_frees_them() {
    let e = initialized();
    set_ports(&e, 26100, 26199, 26000);
    let _outsider = TcpListener::bind("127.0.0.1:26100").unwrap();
    for n in ["a", "b", "c"] {
        e.strata(&["new", n]);
    }
    assert_eq!(
        [port_of(&e, "a"), port_of(&e, "b"), port_of(&e, "c")],
        [26101, 26102, 26103],
        "26100 is taken by another program"
    );
    e.strata(&["rm", "b"]);
    e.strata(&["new", "d"]);
    assert_eq!(port_of(&e, "d"), 26102, "b's port came back");
}

#[test]
fn the_dev_server_gets_port_and_url_and_is_confirmed() {
    let e = initialized();
    set_ports(&e, 26200, 26299, 26001);
    e.strata(&["new", "a"]);
    let out = start_dev(&e, "a", &node_server("hello", None));
    assert_eq!(out["port"], 26200);
    assert_eq!(out["url"], "http://a.app.localhost:26001");
    assert_eq!(out["direct_url"], "http://localhost:26200");
    wait_until("route confirmed", || {
        e.status()["routes"]["a.app"]
            .as_str()
            .is_some_and(|a| a.ends_with(":26200"))
    });
    let body = std::process::Command::new("curl")
        .args(["-s", "http://127.0.0.1:26200"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&body.stdout), "hello");
    e.strata(&["rm", "a", "--force"]);
    assert!(
        e.status()["routes"].as_object().unwrap().is_empty(),
        "route removed with the server"
    );
}

#[test]
fn a_server_that_ignores_port_is_routed_where_it_listens() {
    let e = initialized();
    set_ports(&e, 26300, 26399, 26002);
    e.strata(&["new", "a"]);
    start_dev(&e, "a", &node_server("stubborn", Some(26377)));
    wait_until("route follows the real port", || {
        e.status()["routes"]["a.app"]
            .as_str()
            .is_some_and(|a| a.ends_with(":26377"))
    });
    assert_eq!(e.agent("a").unwrap()["processes"][0]["port"], 26377);
    let kinds: Vec<String> = e.status()["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(kinds.contains(&"route.updated".to_string()));
}

#[test]
fn servers_started_outside_strata_are_listed() {
    let e = initialized();
    set_ports(&e, 26400, 26499, 26003);
    let wt = PathBuf::from(e.strata(&["new", "a"]).trim());
    // Its own process group, so the cleanup reaches node too (Debian's dash forks it instead
    // of exec'ing, so killing `sh` alone would leave the server running).
    let mut outside = std::process::Command::new("sh")
        .args(["-c", &node_server("rogue", Some(26488))])
        .current_dir(&wt)
        .process_group(0)
        .spawn()
        .unwrap();
    wait_until("listed as unmanaged", || {
        e.status()["unmanaged"]
            .as_array()
            .is_some_and(|u| u.iter().any(|v| v["agent"] == "a" && v["port"] == 26488))
    });
    let hint = e.status()["unmanaged"][0]["hint"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(hint.contains("strata_start_dev_server"), "{hint}");
    common::kill_group(outside.id());
    outside.wait().unwrap();
}

fn through_proxy(proxy: u16, host: &str) -> String {
    let out = std::process::Command::new("curl")
        .args([
            "-s",
            "-H",
            &format!("Host: {host}"),
            &format!("http://127.0.0.1:{proxy}/"),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn the_proxy_routes_each_agent_to_its_dev_server() {
    let e = initialized();
    set_ports(&e, 26500, 26599, 26004);
    for (agent, text) in [("a", "from a"), ("b", "from b")] {
        e.strata(&["new", agent]);
        start_dev(&e, agent, &node_server(text, None));
    }
    wait_until("both routed", || {
        e.status()["routes"]
            .as_object()
            .is_some_and(|r| r.len() == 2)
    });
    assert_eq!(through_proxy(26004, "a.app.localhost:26004"), "from a");
    assert_eq!(through_proxy(26004, "b.app.localhost:26004"), "from b");
    let page = through_proxy(26004, "nobody.app.localhost:26004");
    assert!(
        page.contains("http://a.app.localhost:26004/")
            && page.contains("http://b.app.localhost:26004/"),
        "{page}"
    );
}

#[test]
fn a_held_proxy_port_fails_dev_with_the_holder_named() {
    let e = initialized();
    set_ports(&e, 26600, 26699, 26005);
    let _holder = TcpListener::bind("127.0.0.1:26005").unwrap();
    e.strata(&["new", "a"]);
    let out = start_dev(&e, "a", &node_server("x", None));
    let err = out["error"].as_str().unwrap_or_default();
    assert!(
        err.contains("proxy port 26005 is held by")
            && err.contains(&format!("pid {}", std::process::id())),
        "{out}"
    );
    assert!(
        e.agent("a").unwrap()["processes"]
            .as_array()
            .unwrap()
            .is_empty(),
        "nothing started"
    );
}

#[test]
fn a_port_taken_after_new_moves_the_dev_server() {
    // Another tool that picks ports in the same range (Portless uses 4000-4999) can take an
    // agent's reserved port between `strata new` and `strata dev`.
    let e = initialized();
    set_ports(&e, 26700, 26799, 26007);
    e.strata(&["new", "a"]);
    assert_eq!(port_of(&e, "a"), 26700);
    let _outsider = TcpListener::bind("127.0.0.1:26700").unwrap();
    let out = start_dev(&e, "a", &node_server("moved", None));
    assert_eq!(out["port"], 26701, "{out}");
    assert_eq!(out["direct_url"], "http://localhost:26701");
    assert_eq!(port_of(&e, "a"), 26701);
    wait_until("served on the new port", || {
        let body = std::process::Command::new("curl")
            .args(["-s", "http://127.0.0.1:26701"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&body.stdout) == "moved"
    });
    e.strata(&["rm", "a", "--force"]);
}
