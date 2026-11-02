//! PLAN 3.3: port registry, listener confirmation, unmanaged listeners.

mod common;

use common::{initialized, node_server, set_ports, start_dev, wait_until};
use std::net::TcpListener;
use std::path::PathBuf;

fn port_of(e: &common::Env, agent: &str) -> u64 {
    e.agent(agent).unwrap()["port"].as_u64().unwrap()
}

#[test]
fn agents_get_distinct_free_ports_and_rm_frees_them() {
    let e = initialized();
    set_ports(&e, 46100, 46199, 46000);
    let _outsider = TcpListener::bind("127.0.0.1:46100").unwrap();
    for n in ["a", "b", "c"] {
        e.strata(&["new", n]);
    }
    assert_eq!(
        [port_of(&e, "a"), port_of(&e, "b"), port_of(&e, "c")],
        [46101, 46102, 46103],
        "46100 is taken by another program"
    );
    e.strata(&["rm", "b"]);
    e.strata(&["new", "d"]);
    assert_eq!(port_of(&e, "d"), 46102, "b's port came back");
}

#[test]
fn the_dev_server_gets_port_and_url_and_is_confirmed() {
    let e = initialized();
    set_ports(&e, 46200, 46299, 46001);
    e.strata(&["new", "a"]);
    let out = start_dev(&e, "a", &node_server("hello", None));
    assert_eq!(out["port"], 46200);
    assert_eq!(out["url"], "http://a.app.localhost:46001");
    assert_eq!(out["direct_url"], "http://localhost:46200");
    wait_until("route confirmed", || {
        e.status()["routes"]["a.app"]
            .as_str()
            .is_some_and(|a| a.ends_with(":46200"))
    });
    let body = std::process::Command::new("curl")
        .args(["-s", "http://127.0.0.1:46200"])
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
    set_ports(&e, 46300, 46399, 46002);
    e.strata(&["new", "a"]);
    start_dev(&e, "a", &node_server("stubborn", Some(46377)));
    wait_until("route follows the real port", || {
        e.status()["routes"]["a.app"]
            .as_str()
            .is_some_and(|a| a.ends_with(":46377"))
    });
    assert_eq!(e.agent("a").unwrap()["processes"][0]["port"], 46377);
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
    set_ports(&e, 46400, 46499, 46003);
    let wt = PathBuf::from(e.strata(&["new", "a"]).trim());
    let mut outside = std::process::Command::new("sh")
        .args(["-c", &node_server("rogue", Some(46488))])
        .current_dir(&wt)
        .spawn()
        .unwrap();
    wait_until("listed as unmanaged", || {
        e.status()["unmanaged"]
            .as_array()
            .is_some_and(|u| u.iter().any(|v| v["agent"] == "a" && v["port"] == 46488))
    });
    let hint = e.status()["unmanaged"][0]["hint"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(hint.contains("strata_start_dev_server"), "{hint}");
    outside.kill().unwrap();
    outside.wait().unwrap();
    std::process::Command::new("pkill")
        .args(["-f", "listen(46488)"])
        .status()
        .unwrap();
}
