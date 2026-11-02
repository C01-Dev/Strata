//! PLAN 5.2: `strata gc`, `rm` deleting dependencies in the background, the status table.

mod common;

use common::{git, initialized, wait_until};

#[test]
fn rm_sets_dependencies_aside_and_deletes_them_after() {
    let e = common::npm_repo();
    e.strata(&["init", "--no-service"]);
    common::no_pool(&e);
    e.strata(&["new", "a"]);
    let a = e.wt("a");
    assert!(a.join("node_modules/local/package.json").exists());
    e.strata(&["rm", "a"]);
    assert!(!a.exists());
    let trash = e.root.parent().unwrap().join(".strata/app/.trash");
    wait_until("the trash is deleted", || {
        std::fs::read_dir(&trash).map_or(true, |mut d| d.next().is_none())
    });
}

#[test]
fn gc_removes_orphans_their_logs_and_seeding_leftovers() {
    let e = initialized();
    e.strata(&["new", "a"]);
    e.strata(&["new", "b"]);
    e.strata(&["run", "a", "--", "true"]);
    assert!(e.home.join("logs/a").exists());
    std::fs::remove_dir_all(e.wt("a")).unwrap();
    wait_until("a orphaned", || {
        e.agent("a").is_some_and(|a| a["state"] == "orphaned")
    });
    // A clone a crash left behind in b.
    std::fs::create_dir_all(e.wt("b").join(".strata-seed-node_modules/x")).unwrap();
    std::fs::write(e.wt("b").join(".strata-seed-node_modules/x/index.js"), "").unwrap();
    assert!(!git(&e.wt("b"), &["status", "--porcelain"]).is_empty());

    let out = e.strata(&["gc"]);
    assert!(out.contains("orphaned agents removed: a"), "{out}");
    assert!(out.contains("seeding leftovers removed: 1"), "{out}");
    assert!(out.contains("old agent logs removed: a"), "{out}");
    assert!(e.agent("a").is_none());
    assert!(
        !e.home.join("logs/a").exists(),
        "an orphan's logs go with it"
    );
    assert!(git(&e.wt("b"), &["status", "--porcelain"]).is_empty());
    assert!(e.agent("b").is_some(), "b stays");
}

#[test]
fn status_is_a_table() {
    let e = initialized();
    e.strata(&["new", "a"]);
    let out = e.strata(&["status"]);
    let header = out
        .lines()
        .find(|l| l.trim_start().starts_with("AGENT"))
        .unwrap();
    for col in ["STATE", "BRANCH", "PORT", "DEPS", "MEMORY", "URL"] {
        assert!(header.contains(col), "{out}");
    }
    let row = out
        .lines()
        .find(|l| l.trim_start().starts_with("a "))
        .unwrap();
    assert!(row.contains("ready") && row.contains("none"), "{out}");
}
