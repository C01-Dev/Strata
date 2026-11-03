mod common;

#[test]
fn init_writes_config_from_detected_setup() {
    let e = common::env(&[
        ("package.json", r#"{"scripts":{"dev":"next dev"}}"#),
        ("pnpm-lock.yaml", ""),
        (
            "node_modules/typescript/package.json",
            r#"{"version":"7.1.0-beta"}"#,
        ),
    ]);
    let out = e.strata(&["init", "--no-service"]);
    assert!(out.contains("package manager: pnpm"), "{out}");
    assert!(out.contains("checker: on"), "{out}");
    let config = std::fs::read_to_string(e.root.join("strata.toml")).unwrap();
    assert!(config.contains(r#"command = "pnpm dev""#), "{config}");
    assert!(config.contains(r#"base = "main""#), "{config}");
    assert!(config.contains(r#"app = "app""#), "{config}");
    // A second init keeps the user's file.
    std::fs::write(e.root.join("strata.toml"), "[repo]\nbase = \"main\"\n").unwrap();
    assert!(e.strata(&["init", "--no-service"]).contains("kept"));
    assert_eq!(
        std::fs::read_to_string(e.root.join("strata.toml")).unwrap(),
        "[repo]\nbase = \"main\"\n"
    );
}

#[test]
fn init_turns_the_checker_off_for_yarn_pnp() {
    let e = common::env(&[
        ("package.json", "{}"),
        ("yarn.lock", ""),
        (".yarnrc.yml", "enableTelemetry: false\n"),
    ]);
    let out = e.strata(&["init", "--no-service"]);
    assert!(out.contains("checker: off. Yarn Plug'n'Play"), "{out}");
    common::own_ports(&e);
    the_rest_still_works(&e, "Plug'n'Play");
}

/// Worktrees and processes work when the checker is off; `check` says why it is off.
fn the_rest_still_works(e: &common::Env, why: &str) {
    let wt = std::path::PathBuf::from(e.strata(&["new", "a"]).trim());
    assert!(wt.join("package.json").exists());
    let err = e.strata_err(&["check", "a"]);
    assert!(err.contains(why), "{err}");
    let out = e.strata_in(&wt, &["run", "a", "--", "echo", "still works"]);
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("still works"),
        "{out:?}"
    );
    // --force: the install rewrote the fixture's empty yarn.lock.
    e.strata(&["rm", "a", "--force"]);
    assert!(!wt.exists());
}

#[test]
fn typescript_older_than_7_1_turns_only_the_checker_off() {
    let e = common::env(&[
        (".gitignore", "node_modules\n"),
        ("package.json", r#"{"name":"app","private":true}"#),
        ("tsconfig.json", "{}"),
        ("src/a.ts", "export const a: number = 1;\n"),
    ]);
    std::fs::create_dir_all(e.root.join("node_modules/typescript")).unwrap();
    common::write_file(
        &e.root,
        "node_modules/typescript/package.json",
        r#"{"version":"5.9.3"}"#,
    );
    let out = e.strata(&["init", "--no-service"]);
    assert!(
        out.contains("checker: off. TypeScript 5.9.3 is older than 7.1"),
        "{out}"
    );
    common::own_ports(&e);
    the_rest_still_works(&e, "older than 7.1");
}

#[test]
fn init_from_inside_a_linked_worktree_uses_the_main_checkout() {
    let e = common::env(&[]);
    let wt = e.tmp.path().join("wt");
    common::git(
        &e.root,
        &["worktree", "add", "-q", "-b", "x", wt.to_str().unwrap()],
    );
    let out = e.strata_in(&wt, &["init", "--no-service"]);
    assert!(out.status.success());
    assert!(e.root.join("strata.toml").exists());
    assert!(!wt.join("strata.toml").exists());
}

#[test]
fn init_outside_a_repo_fails_clearly() {
    let e = common::env(&[]);
    let out = e.strata_in(e.tmp.path(), &["init", "--no-service"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a git repository"));
}
