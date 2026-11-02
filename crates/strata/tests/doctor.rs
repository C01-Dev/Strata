//! PLAN 5.1: `strata doctor` and `--report`.

mod common;

use common::initialized;
use std::net::TcpListener;

fn doctor(e: &common::Env, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
    let out = e.strata_with(&e.root, &[&["doctor"], args].concat(), envs);
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

fn line<'a>(out: &'a str, name: &str) -> &'a str {
    out.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no {name} line in:\n{out}"))
}

#[test]
fn a_set_up_repo_passes() {
    let e = initialized();
    let (ok, out) = doctor(&e, &[], &[]);
    assert!(ok, "{out}");
    for name in ["daemon", "git", "node", "socket", "proxy"] {
        assert!(line(&out, name).starts_with("ok"), "{out}");
    }
    let clone = line(&out, "clone");
    if cfg!(target_os = "macos") {
        assert!(clone.starts_with("ok"), "APFS clones: {out}");
    }
    assert!(!out.contains("FAIL"), "{out}");
}

#[test]
fn a_held_proxy_port_fails_with_its_holder() {
    let e = initialized();
    let config = std::fs::read_to_string(e.root.join("strata.toml")).unwrap();
    let port: u16 = config
        .lines()
        .find_map(|l| l.strip_prefix("proxy_port = "))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
        .unwrap();
    let _holder = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let (ok, out) = doctor(&e, &[], &[]);
    assert!(!ok, "a failed check exits non-zero");
    let proxy = line(&out, "proxy");
    assert!(
        proxy.starts_with("FAIL") && proxy.contains(&format!("pid {}", std::process::id())),
        "{out}"
    );
}

#[test]
fn a_socket_path_over_the_limit_fails() {
    let e = initialized();
    let long = e.tmp.path().join("h".repeat(110));
    let (ok, out) = doctor(&e, &[], &[("STRATA_HOME", long.to_str().unwrap())]);
    assert!(!ok);
    assert!(line(&out, "socket").starts_with("FAIL"), "{out}");
    assert!(
        line(&out, "daemon").starts_with("warn"),
        "no daemon there: {out}"
    );
}

#[test]
fn the_report_is_redacted_markdown() {
    let e = initialized();
    let (_, out) = doctor(&e, &["--report"], &[]);
    assert!(out.contains("| check | result | detail |"), "{out}");
    let root = e.root.to_string_lossy();
    assert!(!out.contains(root.as_ref()), "{out}");
    assert!(out.contains("<repo"), "paths become placeholders: {out}");
    let home = std::env::var("HOME").unwrap();
    assert!(!out.contains(&home), "{out}");
}
