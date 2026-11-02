//! PLAN 3.0 and 4.1: dependencies in new worktrees. The base worktree installs (or clones the
//! main checkout's) and is the donor; agents with the same dep_key clone from it, agents with
//! another key install and become the donor for theirs.

mod common;

use std::path::{Path, PathBuf};

/// Cloning lands on macOS in 4.1; elsewhere the clone fails and the worktree installs.
const CLONES: bool = cfg!(target_os = "macos");

fn new(e: &common::Env, args: &[&str]) -> (PathBuf, String, serde_json::Value) {
    let out = e.strata_in(&e.root, &[&["new"], args].concat());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (
        path,
        String::from_utf8_lossy(&out.stderr).into_owned(),
        e.agent(args[0]).unwrap(),
    )
}

fn npm_repo() -> common::Env {
    common::env(&[
        (".gitignore", "node_modules\n"),
        (
            "package.json",
            r#"{"name":"app","private":true,"dependencies":{"local":"file:./local"}}"#,
        ),
        (
            "local/package.json",
            r#"{"name":"local","version":"1.0.0"}"#,
        ),
    ])
}

fn real_dir(p: &Path) -> bool {
    p.symlink_metadata().is_ok_and(|m| m.is_dir())
}

#[test]
fn the_base_installs_and_agents_clone_from_it() {
    let e = npm_repo();
    let init = e.strata(&["init", "--no-service"]);
    assert!(init.contains("(dependencies: install)"), "{init}");
    assert!(e.home.join("logs/.base/install.log").exists());
    let base = e.root.parent().unwrap().join(".strata/app/.base");
    assert!(
        real_dir(&base.join("node_modules")),
        "the base has its own copy"
    );

    let (path, stderr, agent) = new(&e, &["a"]);
    if CLONES {
        assert!(
            stderr.contains(&format!("dependencies: clone from {}", base.display())),
            "{stderr}"
        );
        assert_eq!(agent["seed_tier"], "clone");
    }
    assert!(real_dir(&path.join("node_modules")));
    assert!(path.join("node_modules/local/package.json").exists());
    assert!(agent["dep_key"].is_string());
    assert!(
        !path.join("package-lock.json").exists(),
        "no lockfile written into the worktree"
    );
    e.strata(&["rm", "a"]); // still clean, so no --force
}

#[test]
fn the_main_checkout_seeds_the_base_when_its_key_matches() {
    let e = common::env(&[
        (".gitignore", "node_modules\n"),
        ("package.json", r#"{"name":"app","private":true}"#),
    ]);
    std::fs::create_dir_all(e.root.join("node_modules/pkg")).unwrap();
    std::fs::write(e.root.join("node_modules/pkg/index.js"), "main's").unwrap();
    let init = e.strata(&["init", "--no-service"]);
    let base = e.root.parent().unwrap().join(".strata/app/.base");
    if CLONES {
        assert!(
            init.contains(&format!("(dependencies: clone from {})", e.root.display())),
            "{init}"
        );
        assert!(real_dir(&base.join("node_modules")));
        assert_eq!(
            std::fs::read_to_string(base.join("node_modules/pkg/index.js")).unwrap(),
            "main's"
        );
    }
}

#[test]
fn a_new_dep_key_installs_and_becomes_the_donor() {
    let e = npm_repo();
    e.strata(&["init", "--no-service"]);
    let (a, _, _) = new(&e, &["a"]);
    common::commit_file(
        &a,
        "package.json",
        r#"{"name":"app","version":"2.0.0","private":true,"dependencies":{"local":"file:./local"}}"#,
    );
    let tier = |stderr: &str| {
        stderr
            .lines()
            .find_map(|l| l.strip_prefix("dependencies: "))
            .unwrap()
            .to_string()
    };

    let (b, stderr, _) = new(&e, &["b", "--from", "a"]);
    assert_eq!(tier(&stderr), "install", "no donor has a's new key");
    let (_, stderr, _) = new(&e, &["c", "--from", "a"]);
    if CLONES {
        assert_eq!(tier(&stderr), format!("clone from {}", b.display()));
    }

    // A donor whose dependencies changed since is stale: never cloned from.
    std::fs::write(b.join("package.json"), r#"{"name":"changed"}"#).unwrap();
    let (_, stderr, _) = new(&e, &["d", "--from", "a"]);
    assert_eq!(tier(&stderr), "install", "{stderr}");

    // d is the donor now; removing it removes the donor.
    e.strata(&["rm", "d"]);
    let (_, stderr, _) = new(&e, &["f", "--from", "a"]);
    assert_eq!(tier(&stderr), "install", "{stderr}");
}

#[test]
fn a_failed_install_leaves_a_usable_worktree_and_a_warning() {
    // Malformed package.json: npm fails at once (EJSONPARSE), without touching the network.
    let e = common::env(&[("package.json", "{ not json")]);
    let init = e.strata_in(&e.root, &["init", "--no-service"]);
    assert!(
        String::from_utf8_lossy(&init.stderr).contains("failed"),
        "the base's install failed too"
    );
    let (path, warnings, agent) = new(&e, &["a"]);
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
    let (_, _, agent) = new(&e, &["a"]);
    assert_eq!(agent["seed_tier"], "none");
}

#[test]
fn without_clones_the_configured_fallback_runs() {
    let e = common::env(&[
        (".gitignore", "node_modules\n"),
        ("package.json", r#"{"name":"app","private":true}"#),
        ("strata.toml", "[seed]\nfallback = \"copy\"\n"),
    ]);
    std::fs::create_dir_all(e.root.join("node_modules/pkg")).unwrap();
    std::fs::write(e.root.join("node_modules/pkg/index.js"), "main's").unwrap();
    e.strata(&["init", "--no-service"]);
    let base = e.root.parent().unwrap().join(".strata/app/.base");
    let file = |p: &Path| p.join("node_modules/pkg/index.js");
    assert!(file(&base).exists(), "cloned, or copied where clones fail");
    // From here every clone fails, as on ext4.
    e.strata(&["daemon", "stop"]);
    let out = e.strata_with(
        &e.root,
        &["daemon", "start"],
        &[("STRATA_FAULT", "no-clone")],
    );
    assert!(out.status.success());
    let ino = |p: &Path| std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(p).unwrap());
    let set_fallback = |tier: &str| {
        let toml = std::fs::read_to_string(e.root.join("strata.toml")).unwrap();
        let toml: String = toml
            .lines()
            .map(|l| match l.starts_with("fallback") {
                true => format!("fallback = \"{tier}\"\n"),
                false => format!("{l}\n"),
            })
            .collect();
        std::fs::write(e.root.join("strata.toml"), toml).unwrap();
    };

    let (a, stderr, agent) = new(&e, &["a"]);
    assert!(
        stderr.contains(&format!("dependencies: copy from {}", base.display())),
        "{stderr}"
    );
    assert_eq!(agent["seed_tier"], "copy");
    assert_eq!(std::fs::read_to_string(file(&a)).unwrap(), "main's");
    assert_ne!(ino(&file(&a)), ino(&file(&base)), "a copy is its own file");

    set_fallback("hardlink");
    let (b, stderr, _) = new(&e, &["b"]);
    assert!(stderr.contains("dependencies: hardlink from"), "{stderr}");
    assert!(stderr.contains("the files are shared"), "{stderr}");
    assert_eq!(
        ino(&file(&b)),
        ino(&file(&base)),
        "a hardlink is the same file"
    );

    set_fallback("install");
    let (_, stderr, agent) = new(&e, &["c"]);
    assert!(
        stderr.contains("cannot clone dependencies from") && stderr.contains("installing instead"),
        "{stderr}"
    );
    assert_eq!(agent["seed_tier"], "install");
}
