//! PLAN 3.0: `strata new` installs dependencies in the new worktree.

mod common;

use std::path::PathBuf;

fn new(e: &common::Env, name: &str) -> (PathBuf, String, serde_json::Value) {
    let out = e.strata_in(&e.root, &["new", name]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (
        path,
        String::from_utf8_lossy(&out.stderr).into_owned(),
        e.agent(name).unwrap(),
    )
}

#[test]
fn npm_install_runs_in_the_new_worktree() {
    let e = common::env(&[
        (".gitignore", "node_modules\n"),
        (
            "package.json",
            r#"{"name":"app","private":true,"dependencies":{"local":"file:./local"}}"#,
        ),
        (
            "local/package.json",
            r#"{"name":"local","version":"1.0.0"}"#,
        ),
    ]);
    e.strata(&["init", "--no-service"]);
    let (path, warnings, agent) = new(&e, "a");
    assert!(warnings.is_empty(), "{warnings}");
    assert_eq!(agent["seed_tier"], "install");
    assert!(path.join("node_modules/local").exists());
    assert!(
        !path.join("package-lock.json").exists(),
        "no lockfile written into the worktree"
    );
    assert!(e.home.join("logs/a/install.log").exists());
    e.strata(&["rm", "a"]); // still clean, so no --force
}

#[test]
fn a_failed_install_leaves_a_usable_worktree_and_a_warning() {
    // Malformed package.json: npm fails at once (EJSONPARSE), without touching the network.
    let e = common::env(&[("package.json", "{ not json")]);
    e.strata(&["init", "--no-service"]);
    let (path, warnings, agent) = new(&e, "a");
    assert!(
        warnings.contains("`npm install --no-package-lock` failed")
            && warnings.contains("install.log"),
        "{warnings}"
    );
    assert_eq!(
        (agent["seed_tier"].as_str(), agent["state"].as_str()),
        (Some("install-failed"), Some("ready"))
    );
    assert!(path.join("package.json").exists());
}

#[test]
fn repos_without_package_json_skip_the_install() {
    let e = common::initialized();
    let (_, _, agent) = new(&e, "a");
    assert_eq!(agent["seed_tier"], "none");
}
