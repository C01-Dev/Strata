//! Task 1.7: SIGKILL the daemon in the middle of an operation, restart it, check the state.

mod common;

use common::{Env, initialized, wait_until};
use std::path::PathBuf;

/// Restarts the daemon with `STRATA_FAULT=<point>`, runs `op` until the daemon stalls at the
/// point, SIGKILLs it, then starts a normal daemon (which runs crash recovery).
fn crash_during(e: &Env, point: &str, op: &[&str]) {
    e.strata(&["daemon", "stop"]);
    let out = e.strata_with(&e.root, &["daemon", "start"], &[("STRATA_FAULT", point)]);
    assert!(out.status.success());
    let pid = e.status()["pid"].to_string();
    std::thread::scope(|s| {
        let caller = s.spawn(|| e.strata_in(&e.root, op));
        let log = e.home.join("logs/daemon.log");
        wait_until(&format!("daemon reached {point}"), || {
            std::fs::read_to_string(&log).is_ok_and(|l| l.contains(&format!("fault point {point}")))
        });
        assert!(
            std::process::Command::new("kill")
                .args(["-9", &pid])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            !caller.join().unwrap().status.success(),
            "the caller sees the failure"
        );
    });
    e.strata(&["daemon", "start"]);
}

fn path(out: &str) -> PathBuf {
    PathBuf::from(out.trim())
}

#[test]
fn killed_before_git_add_undoes_the_create() {
    let e = initialized();
    crash_during(&e, "new:before_git", &["new", "a"]);
    assert!(e.agent("a").is_none());
    assert!(!e.wt("a").exists() && !e.is_worktree(&e.wt("a")));
    assert_eq!(
        path(&e.strata(&["new", "a"])),
        e.wt("a"),
        "the name is free again"
    );
}

#[test]
fn killed_after_git_add_undoes_the_create() {
    let e = initialized();
    crash_during(&e, "new:after_git", &["new", "a"]);
    assert!(e.agent("a").is_none());
    assert!(
        !e.wt("a").exists() && !e.is_worktree(&e.wt("a")),
        "worktree the caller never saw is removed"
    );
    e.strata(&["new", "a"]);
    assert!(e.is_locked(&e.wt("a")));
}

#[test]
fn killed_before_git_remove_keeps_the_agent() {
    let e = initialized();
    let a = path(&e.strata(&["new", "a"]));
    std::fs::write(a.join("work.txt"), "keep me").unwrap();
    crash_during(&e, "rm:before_git", &["rm", "a", "--force"]);
    assert_eq!(e.agent("a").unwrap()["state"], "ready");
    assert!(e.is_locked(&a), "re-locked");
    assert_eq!(
        std::fs::read_to_string(a.join("work.txt")).unwrap(),
        "keep me"
    );
    e.strata(&["rm", "a", "--force"]);
    assert!(e.agent("a").is_none());
}

#[test]
fn killed_after_git_remove_finishes_the_remove() {
    let e = initialized();
    let a = path(&e.strata(&["new", "a"]));
    let b = path(&e.strata(&["new", "b"]));
    crash_during(&e, "rm:after_git", &["rm", "a"]);
    assert!(e.agent("a").is_none());
    assert!(!a.exists() && !e.is_worktree(&a));
    assert_eq!(
        e.agent("b").unwrap()["state"],
        "ready",
        "other agents untouched"
    );
    assert!(e.is_locked(&b));
    let events = std::fs::read_to_string(e.home.join("logs/daemon.log")).unwrap();
    assert!(events.contains("interrupted remove finished"), "{events}");
}

#[test]
fn worktree_deleted_while_the_daemon_was_down_is_orphaned() {
    let e = initialized();
    let a = path(&e.strata(&["new", "a"]));
    e.strata(&["new", "b"]);
    let pid = e.status()["pid"].to_string();
    std::process::Command::new("kill")
        .args(["-9", &pid])
        .status()
        .unwrap();
    std::fs::remove_dir_all(&a).unwrap();
    e.strata(&["daemon", "start"]);
    assert_eq!(e.agent("a").unwrap()["state"], "orphaned");
    assert!(!e.is_worktree(&a));
    assert_eq!(e.agent("b").unwrap()["state"], "ready");
}
