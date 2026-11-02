//! PLAN 4.5: the warm pool. After a `strata new`, the daemon seeds `seed.warm_pool` detached
//! worktrees in the background; the next `strata new` moves one into place.

mod common;

use common::{git, wait_secs};
use std::path::PathBuf;

/// Pool worktrees from `git worktree list`, with their lock reason.
fn entries(e: &common::Env) -> Vec<(PathBuf, String)> {
    let out = git(&e.root, &["worktree", "list", "--porcelain"]);
    let mut all: Vec<(PathBuf, String)> = vec![];
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            all.push((PathBuf::from(p), String::new()));
        } else if let Some(r) = line.strip_prefix("locked ") {
            all.last_mut().unwrap().1 = r.to_string();
        }
    }
    all.retain(|(p, _)| p.parent().is_some_and(|d| d.ends_with(".pool")));
    all
}

fn ready(e: &common::Env) -> Vec<PathBuf> {
    entries(e)
        .into_iter()
        .filter(|(_, r)| r.starts_with("strata pool: ready "))
        .map(|(p, _)| p)
        .collect()
}

fn new(e: &common::Env, args: &[&str]) -> String {
    let out = e.strata_in(&e.root, &[&["new"], args].concat());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn new_takes_a_seeded_entry_and_the_pool_refills() {
    let e = common::npm_repo();
    e.strata(&["init", "--no-service"]);
    assert!(entries(&e).is_empty(), "the pool fills after a first `new`");
    new(&e, &["a"]);
    wait_secs("an entry is ready", 60, || ready(&e).len() == 1);
    let entry = ready(&e).remove(0);
    assert!(entry.join("node_modules/local/package.json").exists());

    let stderr = new(&e, &["b"]);
    assert!(stderr.contains("(warm pool)"), "{stderr}");
    let b = e.wt("b");
    assert!(!entry.exists(), "moved, not copied");
    assert!(e.is_locked(&b));
    assert_eq!(git(&b, &["branch", "--show-current"]), "b");
    assert!(git(&b, &["status", "--porcelain"]).is_empty());
    assert!(b.join("node_modules/local/package.json").exists());
    wait_secs("refilled", 60, || ready(&e).len() == 1);
    assert_eq!(e.status()["repos"][0]["warm_pool"], 1);
    e.strata(&["rm", "b"]); // clean, so no --force
}

#[test]
fn an_entry_is_not_taken_when_dependencies_differ() {
    let e = common::npm_repo();
    e.strata(&["init", "--no-service"]);
    new(&e, &["a"]);
    common::commit_file(
        &e.wt("a"),
        "package.json",
        r#"{"name":"app","version":"2.0.0","private":true,"dependencies":{"local":"file:./local"}}"#,
    );
    wait_secs("an entry is ready", 60, || ready(&e).len() == 1);
    let entry = ready(&e).remove(0);
    let stderr = new(&e, &["c", "--from", "a"]);
    assert!(!stderr.contains("(warm pool)"), "{stderr}");
    assert!(entry.exists(), "left for an agent it fits");
}

#[test]
fn repos_with_submodules_have_no_pool() {
    let e = common::env(&[
        (".gitmodules", ""),
        ("package.json", r#"{"name":"app","private":true}"#),
    ]);
    e.strata(&["init", "--no-service"]);
    new(&e, &["a"]);
    std::thread::sleep(std::time::Duration::from_secs(3));
    assert!(entries(&e).is_empty());
}

#[test]
fn an_entry_left_mid_seeding_is_removed_at_start() {
    let e = common::npm_repo();
    e.strata(&["init", "--no-service"]);
    let leftover = e.root.parent().unwrap().join(".strata/app/.pool/1");
    git(
        &e.root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            "--lock",
            "--reason",
            "strata pool: seeding",
            leftover.to_str().unwrap(),
            "HEAD",
        ],
    );
    e.strata(&["daemon", "stop"]);
    e.strata(&["daemon", "start"]);
    assert!(!leftover.exists());
    assert!(entries(&e).is_empty());
}
