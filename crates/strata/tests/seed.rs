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

fn real_dir(p: &Path) -> bool {
    p.symlink_metadata().is_ok_and(|m| m.is_dir())
}

#[test]
fn the_base_installs_and_agents_clone_from_it() {
    let e = common::npm_repo();
    let init = e.strata(&["init", "--no-service"]);
    common::no_pool(&e);
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
    common::no_pool(&e);
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
    let e = common::npm_repo();
    e.strata(&["init", "--no-service"]);
    common::no_pool(&e);
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
    common::no_pool(&e);
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
        (
            "strata.toml",
            "[seed]\nfallback = \"copy\"\nwarm_pool = 0\n",
        ),
    ]);
    std::fs::create_dir_all(e.root.join("node_modules/pkg")).unwrap();
    std::fs::write(e.root.join("node_modules/pkg/index.js"), "main's").unwrap();
    // Every clone fails from the start, as on ext4: the base is copied too, and still donates.
    let out = e.strata_with(
        &e.root,
        &["daemon", "start"],
        &[("STRATA_FAULT", "no-clone")],
    );
    assert!(out.status.success());
    let init = e.strata(&["init", "--no-service"]);
    assert!(init.contains("dependencies: copy from"), "{init}");
    let base = e.root.parent().unwrap().join(".strata/app/.base");
    let file = |p: &Path| p.join("node_modules/pkg/index.js");
    assert!(file(&base).exists());
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

fn have(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn pnpm_state_points_at_the_new_worktree() {
    if !have("pnpm") || !CLONES {
        eprintln!("skipped: needs pnpm and clones");
        return;
    }
    let e = common::env(&[
        (".gitignore", "node_modules\n"),
        (
            "package.json",
            r#"{"name":"root","private":true,"scripts":{"hi":"tool"},"dependencies":{"tool":"workspace:*"}}"#,
        ),
        ("pnpm-workspace.yaml", "packages:\n  - \"packages/*\"\n"),
        (
            "packages/tool/package.json",
            r#"{"name":"tool","version":"1.0.0","bin":{"tool":"bin/tool.js"}}"#,
        ),
        (
            "packages/tool/bin/tool.js",
            "#!/usr/bin/env node\nconsole.log('tool ok')\n",
        ),
    ]);
    std::fs::set_permissions(
        e.root.join("packages/tool/bin/tool.js"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    common::git(&e.root, &["add", "."]);
    common::git(&e.root, &["commit", "-q", "-m", "exec"]);
    e.strata(&["init", "--no-service"]);
    common::no_pool(&e);
    let (a, stderr, _) = new(&e, &["a"]);
    assert!(stderr.contains("dependencies: clone from"), "{stderr}");
    let state =
        std::fs::read_to_string(a.join("node_modules/.pnpm-workspace-state-v1.json")).unwrap();
    let base = e.root.parent().unwrap().join(".strata/app/.base");
    assert!(state.contains(&format!("\"{}\"", a.display())), "{state}");
    assert!(!state.contains(&base.display().to_string()), "{state}");
    let out = std::process::Command::new("pnpm")
        .args(["--config.verify-deps-before-run=error", "run", "hi"])
        .current_dir(&a)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("tool ok"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn addons_built_in_the_donor_are_rebuilt() {
    if !CLONES {
        return;
    }
    let e = common::env(&[
        (".gitignore", "node_modules\n"),
        (
            "package.json",
            r#"{"name":"app","private":true,"dependencies":{"addon":"1.0.0"}}"#,
        ),
    ]);
    // A node-gyp addon built in the main checkout: its build files name that folder. Its
    // install script stands in for the compile and records where it ran.
    let addon = e.root.join("node_modules/addon");
    std::fs::create_dir_all(addon.join("build")).unwrap();
    std::fs::write(
        addon.join("package.json"),
        r#"{"name":"addon","version":"1.0.0","scripts":{"install":"node -e \"require('fs').writeFileSync('build/config.gypi', JSON.stringify(process.cwd()))\""}}"#,
    )
    .unwrap();
    std::fs::write(
        addon.join("build/config.gypi"),
        format!("\"{}\"", addon.display()),
    )
    .unwrap();
    let init = e.strata(&["init", "--no-service"]);
    common::no_pool(&e);
    assert!(init.contains(", rebuilt addon)"), "{init}");
    let (a, stderr, _) = new(&e, &["a"]);
    assert!(stderr.contains(", rebuilt addon"), "{stderr}");
    let built = std::fs::read_to_string(a.join("node_modules/addon/build/config.gypi")).unwrap();
    assert_eq!(
        built,
        format!("\"{}\"", a.join("node_modules/addon").display())
    );
    assert!(e.home.join("logs/a/rebuild.log").exists());
}

#[test]
fn a_global_virtual_store_is_installed_and_reported_as_shared() {
    let files = [
        (".gitignore", "node_modules\npnpm-lock.yaml\n"),
        (
            "package.json",
            r#"{"name":"app","private":true,"dependencies":{"is-number":"7.0.0"}}"#,
        ),
        ("pnpm-workspace.yaml", "enableGlobalVirtualStore: true\n"),
        (".npmrc", "offline=true\n"),
    ];
    let e = common::env(&files);
    // Needs pnpm, and is-number in its store already (`offline=true`): skip otherwise.
    let probe = std::process::Command::new("pnpm")
        .arg("install")
        .current_dir(&e.root)
        .output();
    if !probe.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: needs pnpm with is-number@7.0.0 in its store");
        return;
    }
    let init = e.strata(&["init", "--no-service"]);
    common::no_pool(&e);
    assert!(
        init.contains("(dependencies: shared store, not isolated)"),
        "{init}"
    );
    let (a, stderr, agent) = new(&e, &["a"]);
    assert!(
        stderr.contains("dependencies: shared store, not isolated"),
        "{stderr}"
    );
    assert_eq!(agent["seed_tier"], "shared-store");
    assert!(a.join("node_modules/is-number/package.json").exists());
}
