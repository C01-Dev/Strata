//! The git-level part of every Git row in PLAN's edge-case table (Gate 1).

mod common;

use common::{commit_file, git, git_out, initialized, wait_until};
use std::path::PathBuf;

fn path_of(out: &str) -> PathBuf {
    PathBuf::from(out.trim())
}

#[test]
fn new_creates_a_locked_worktree_and_rm_removes_it() {
    let e = initialized();
    let base = e.wt(".base");
    assert!(e.is_locked(&base), "base worktree exists and is locked");
    assert_eq!(
        git(&base, &["rev-parse", "HEAD"]),
        git(&e.root, &["rev-parse", "main"])
    );
    assert_eq!(
        git_out(&base, &["symbolic-ref", "-q", "HEAD"])
            .status
            .code(),
        Some(1),
        "base is detached"
    );

    let path = path_of(&e.strata(&["new", "fix-login"]));
    assert_eq!(path, e.wt("fix-login"));
    assert!(e.is_locked(&path));
    assert_eq!(git(&path, &["branch", "--show-current"]), "fix-login");
    let agent = e.agent("fix-login").unwrap();
    assert_eq!(
        (agent["state"].as_str(), agent["branch"].as_str()),
        (Some("ready"), Some("fix-login"))
    );
    // The note for the agent: dev server only (no TypeScript here), invisible to git, and no
    // obstacle to `rm`.
    let note = std::fs::read_to_string(path.join(".claude/rules/strata.md")).unwrap();
    assert!(note.contains("strata_start_dev_server") && !note.contains("strata_diagnostics"));
    assert_eq!(git(&path, &["status", "--porcelain"]), "");

    assert!(e.strata(&["rm", "fix-login"]).contains("removed fix-login"));
    assert!(!path.exists() && !e.is_worktree(&path));
    assert!(
        git_out(&e.root, &["rev-parse", "--verify", "refs/heads/fix-login"])
            .status
            .code()
            != Some(0),
        "merged branch deleted"
    );
    assert!(e.agent("fix-login").is_none());
}

#[test]
fn agent_note_false_writes_no_note() {
    let e = initialized();
    let toml = e.root.join("strata.toml");
    let config = std::fs::read_to_string(&toml).unwrap();
    assert!(config.contains("agent_note = true"));
    std::fs::write(
        &toml,
        config.replace("agent_note = true", "agent_note = false"),
    )
    .unwrap();
    let path = path_of(&e.strata(&["new", "quiet"]));
    assert!(path.exists() && !path.join(".claude/rules/strata.md").exists());
}

#[test]
fn new_from_a_ref_and_on_an_existing_branch() {
    let e = initialized();
    git(&e.root, &["branch", "old-work"]);
    git(&e.root, &["branch", "spare"]);
    commit_file(&e.root, "later.txt", "x");
    let a = path_of(&e.strata(&["new", "a", "--from", "HEAD~1"]));
    assert!(!a.join("later.txt").exists());
    let b = path_of(&e.strata(&["new", "b", "--branch", "old-work"]));
    assert_eq!(git(&b, &["branch", "--show-current"]), "old-work");
    assert!(
        e.strata_err(&["new", "c", "--branch", "a-new", "--from", "nope"])
            .contains("nope")
    );
    assert!(e.agent("c").is_none(), "a failed create leaves no record");
    assert!(
        e.strata_err(&["new", "d", "--branch", "spare", "--from", "main"])
            .contains("--from does not apply")
    );
}

#[test]
fn refuses_a_branch_checked_out_in_another_worktree() {
    let e = initialized();
    let err = e.strata_err(&["new", "x", "--branch", "main"]);
    assert!(
        err.contains("\"main\" is already checked out in") && err.contains("e.g. \"main-2\""),
        "{err}"
    );
    e.strata(&["new", "a"]);
    let err = e.strata_err(&["new", "b", "--branch", "a"]);
    assert!(err.contains("\"a\" is already checked out in"), "{err}");
    assert!(e.agent("x").is_none() && e.agent("b").is_none());
}

#[test]
fn refuses_an_existing_name_or_path() {
    let e = initialized();
    e.strata(&["new", "a"]);
    let err = e.strata_err(&["new", "a"]);
    assert!(
        err.contains("an agent named \"a\" already exists; try \"a-2\""),
        "{err}"
    );
    std::fs::create_dir_all(e.wt("b")).unwrap();
    let err = e.strata_err(&["new", "b"]);
    assert!(
        err.contains("already exists; try the name \"b-2\""),
        "{err}"
    );
    let err = e.strata_err(&["new", "Fix_Login"]);
    assert!(err.contains("lowercase letters"), "{err}");
}

#[test]
fn rm_refuses_uncommitted_changes_unless_forced() {
    let e = initialized();
    let path = path_of(&e.strata(&["new", "a"]));
    std::fs::write(path.join("scratch.txt"), "untracked").unwrap();
    let err = e.strata_err(&["rm", "a"]);
    assert!(
        err.contains("uncommitted changes in 1 file(s)") && err.contains("--force"),
        "{err}"
    );
    assert!(
        path.join("scratch.txt").exists() && e.is_locked(&path),
        "nothing touched"
    );
    std::fs::write(path.join("README.md"), "edited").unwrap();
    assert!(e.strata_err(&["rm", "a"]).contains("2 file(s)"));
    e.strata(&["rm", "a", "--force"]);
    assert!(!path.exists());
}

#[test]
fn rm_keeps_unmerged_branches_and_honours_keep_branch() {
    let e = initialized();
    let a = path_of(&e.strata(&["new", "a"]));
    commit_file(&a, "work.txt", "work");
    let out = e.strata(&["rm", "a"]);
    assert!(out.contains("kept branch \"a\": it is not merged"), "{out}");
    assert_eq!(
        git(&e.root, &["log", "-1", "--format=%s", "a"]),
        "work.txt",
        "the commit survives"
    );
    e.strata(&["new", "b"]);
    e.strata(&["rm", "b", "--keep-branch"]);
    git(&e.root, &["rev-parse", "--verify", "refs/heads/b"]);
}

#[test]
fn a_hand_deleted_folder_is_orphaned_unlocked_and_pruned() {
    let e = initialized();
    let path = path_of(&e.strata(&["new", "a"]));
    std::fs::remove_dir_all(&path).unwrap();
    wait_until("agent marked orphaned", || {
        e.agent("a").is_some_and(|a| a["state"] == "orphaned")
    });
    assert!(!e.is_worktree(&path), "git record pruned");
    let out = e.strata(&["rm", "a"]);
    assert!(out.contains("already gone"), "{out}");
    assert!(e.agent("a").is_none());
}

#[test]
fn base_worktree_follows_the_base_branch_not_the_main_checkout() {
    let e = initialized();
    let base = e.wt(".base");
    let a = path_of(&e.strata(&["new", "a"]));
    commit_file(&e.root, "pulled.txt", "from upstream");
    let main = git(&e.root, &["rev-parse", "main"]);
    wait_until("base advanced to main", || {
        git(&base, &["rev-parse", "HEAD"]) == main
    });
    assert!(base.join("pulled.txt").exists());

    // The user switches branches and commits in their own checkout: base and agents stay put.
    git(&e.root, &["switch", "-q", "-c", "experiment"]);
    commit_file(&e.root, "experiment.txt", "x");
    std::thread::sleep(std::time::Duration::from_secs(3)); // > one maintenance tick
    assert_eq!(git(&base, &["rev-parse", "HEAD"]), main);
    assert_eq!(git(&a, &["branch", "--show-current"]), "a");
    assert_eq!(e.agent("a").unwrap()["state"], "ready");
}

#[test]
fn rebase_merge_and_branch_switch_inside_a_worktree() {
    let e = initialized();
    let a = path_of(&e.strata(&["new", "a"]));
    commit_file(&a, "feature.txt", "f");
    commit_file(&e.root, "upstream.txt", "u");
    git(&a, &["rebase", "-q", "main"]);
    assert!(a.join("upstream.txt").exists());
    git(&a, &["merge", "-q", "--no-edit", "main"]);
    git(&a, &["switch", "-q", "-c", "a-renamed"]);
    wait_until("branch switch picked up", || {
        e.agent("a").is_some_and(|x| x["branch"] == "a-renamed")
    });
    assert_eq!(e.agent("a").unwrap()["state"], "ready");
    let out = e.strata(&["rm", "a"]);
    assert!(out.contains("kept branch \"a-renamed\""), "{out}");
}

#[test]
fn concurrent_creates_serialize_and_locks_survive_a_stray_prune() {
    let e = initialized();
    let paths: Vec<PathBuf> = std::thread::scope(|s| {
        let handles: Vec<_> = (1..=5)
            .map(|i| {
                s.spawn({
                    let e = &e;
                    move || path_of(&e.strata(&["new", &format!("a{i}")]))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(
        paths.iter().all(|p| e.is_locked(p)),
        "all 5 created and locked"
    );

    // Another tool (or a user) runs gc and prune while a worktree's folder is temporarily
    // unavailable (e.g. an unmounted volume). The daemon is stopped so it does not react first.
    e.strata(&["daemon", "stop"]);
    let away = e.tmp.path().join("away");
    std::fs::rename(&paths[0], &away).unwrap();
    git(&e.root, &["worktree", "prune"]);
    git(&e.root, &["gc", "-q", "--prune=now"]);
    std::fs::rename(&away, &paths[0]).unwrap();
    assert!(
        e.is_locked(&paths[0]),
        "locked worktree record survived prune"
    );
    e.strata(&["daemon", "start"]);
    for i in 1..=5 {
        e.strata(&["rm", &format!("a{i}")]);
    }
    assert_eq!(
        e.worktrees().len(),
        2,
        "only the main checkout and base remain"
    );
}

#[test]
fn adopt_a_worktree_nested_inside_the_repo() {
    let e = initialized();
    let nested = e.root.join(".claude/worktrees/task-1");
    git(
        &e.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "task-1",
            nested.to_str().unwrap(),
        ],
    );
    let out = e.strata(&["adopt", nested.to_str().unwrap()]);
    assert!(out.contains("as task-1"), "{out}");
    assert!(e.is_locked(&nested));
    let agent = e.agent("task-1").unwrap();
    assert_eq!(agent["branch"], "task-1");
    e.strata(&["rm", "task-1"]);
    assert!(!nested.exists());
}

#[test]
fn adopt_a_worktree_made_by_plain_git() {
    let e = initialized();
    let other = e.tmp.path().join("Other Tool");
    git(
        &e.root,
        &["worktree", "add", "-q", "--detach", other.to_str().unwrap()],
    );
    let ts = other.join("node_modules/typescript");
    std::fs::create_dir_all(&ts).unwrap();
    std::fs::write(ts.join("package.json"), r#"{"version":"7.1.0-beta"}"#).unwrap();
    let out = e.strata(&["adopt", other.to_str().unwrap()]);
    assert!(out.contains("as other-tool"), "{out}");
    let note = std::fs::read_to_string(other.join(".claude/rules/strata.md")).unwrap();
    assert!(note.contains("instead of running `tsc`"), "{note}");
    assert!(note.contains("run `strata_conflicts`"), "{note}");
    assert_eq!(e.agent("other-tool").unwrap()["branch"], "(detached)");
    assert!(
        e.strata_err(&["adopt", other.to_str().unwrap()])
            .contains("already adopted as \"other-tool\"")
    );
    assert!(
        e.strata_err(&["adopt", e.root.to_str().unwrap()])
            .contains("main checkout")
    );
    std::fs::create_dir(e.root.join("src")).unwrap();
    assert!(
        e.strata_err(&["adopt", e.root.join("src").to_str().unwrap()])
            .contains("not the top folder")
    );
    assert!(
        e.strata_err(&["adopt", e.wt(".base").to_str().unwrap()])
            .contains("base worktree")
    );
    // Same folder name as an existing agent gets a suffix.
    let again = e.tmp.path().join("x/other-tool");
    git(
        &e.root,
        &["worktree", "add", "-q", "--detach", again.to_str().unwrap()],
    );
    assert!(
        e.strata(&["adopt", again.to_str().unwrap()])
            .contains("as other-tool-2")
    );
}

#[test]
fn new_warns_about_uninitialized_submodules() {
    let e = initialized();
    let lib = e.tmp.path().join("lib");
    std::fs::create_dir(&lib).unwrap();
    git(&lib, &["init", "-q", "-b", "main"]);
    commit_file(&lib, "lib.txt", "lib");
    git(
        &e.root,
        &[
            "submodule",
            "add",
            "-q",
            lib.to_str().unwrap(),
            "vendor/lib",
        ],
    );
    git(&e.root, &["commit", "-q", "-m", "add submodule"]);
    let out = e.strata_in(&e.root, &["new", "a"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("submodule vendor/lib is not initialized"),
        "{stderr}"
    );
}

#[test]
fn commands_need_a_registered_repo() {
    let e = common::env(&[]);
    e.strata(&["daemon", "start"]);
    let err = e.strata_err(&["new", "a"]);
    assert!(
        err.contains("not registered with Strata; run `strata init`"),
        "{err}"
    );
}

#[test]
fn rm_takes_the_worktree_path_or_dot() {
    let e = initialized();
    e.strata(&["new", "a"]);
    e.strata(&["new", "b"]);
    let out = e.strata_in(&e.wt("a"), &["rm", "."]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("removed a"));
    e.strata(&["rm", e.wt("b").to_str().unwrap()]);
    assert!(e.agent("a").is_none() && e.agent("b").is_none());
    let err = e.strata_err(&["rm", e.root.to_str().unwrap()]);
    assert!(err.contains("is not an agent's worktree"), "{err}");
}

#[test]
fn a_repo_beside_stratas_home_keeps_its_worktrees_apart() {
    // A repo in the home folder named like one of Strata's own folders: by the default rule its
    // worktrees would go in ~/.strata/logs, which is where Strata keeps every agent's logs.
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("logs");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("README.md"), "hi\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let e = common::Env {
        root: root.clone(),
        home: base.join(".strata"),
        tmp,
    };
    e.strata(&["init", "--no-service"]);
    common::own_ports(&e);
    let toml = std::fs::read_to_string(root.join("strata.toml")).unwrap();
    assert!(
        toml.contains(r#"worktree_dir = "../.strata/worktrees/logs""#),
        "{toml}"
    );
    let wt = path_of(&e.strata(&["new", "a"]));
    assert_eq!(wt, base.join(".strata/worktrees/logs/a"));
    assert!(
        !base.join(".strata/logs/a").join("README.md").exists(),
        "not among the logs"
    );
    e.strata(&["rm", "a"]);
}
