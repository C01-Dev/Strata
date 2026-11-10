mod common;

use common::{project, write};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;
use strata_checker::broker::{Broker, Op, Request, Settings};
use strata_core::Error;

fn settings() -> Settings {
    Settings {
        tsconfigs: vec!["tsconfig.json".into()],
        extra_paths: vec![],
        idle_dispose: Duration::from_secs(900),
        huge_diff_ratio: 0.5,
        group_memory_bytes: u64::MAX,
        base_rescan: Duration::ZERO,
        recycle_after: Duration::from_secs(3600),
    }
}

struct Fixture {
    tmp: tempfile::TempDir,
    base: PathBuf,
    wt: PathBuf,
    broker: Broker,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let base = project(tmp.path(), "base");
    let wt = project(tmp.path(), "wt");
    let broker = Broker::new(&tmp.path().join("home"));
    Fixture {
        tmp,
        base,
        wt,
        broker,
    }
}

impl Fixture {
    async fn check(&self, op: Op) -> Result<Value, Error> {
        self.check_with(op, &settings()).await
    }

    async fn check_with(&self, op: Op, s: &Settings) -> Result<Value, Error> {
        let req = Request {
            repo: self.base.clone(),
            base: self.base.clone(),
            agent: "a".into(),
            worktree: self.wt.clone(),
            op,
        };
        self.broker.check(req, s).await
    }

    async fn diagnostics(&self) -> Value {
        self.check(Op::Diagnostics {
            file: None,
            limit: None,
        })
        .await
        .unwrap()
    }
}

fn codes(d: &Value) -> Vec<(String, u64, u64, u64)> {
    d["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["file"].as_str().unwrap().to_string(),
                i["line"].as_u64().unwrap(),
                i["column"].as_u64().unwrap(),
                i["code"].as_u64().unwrap(),
            )
        })
        .collect()
}

fn file(path: &str) -> String {
    path.to_string()
}

#[tokio::test]
async fn shared_answers_follow_the_worktree() {
    let f = fixture();
    let d = f.diagnostics().await;
    assert_eq!(d["total"], 0);
    assert_eq!(d["sharing"]["mode"], "shared");

    write(
        &f.wt,
        &[(
            "src/b.ts",
            "import { addOne } from \"./a\";\nexport const total: string = addOne(2);\n",
        )],
    );
    assert_eq!(
        codes(&f.diagnostics().await),
        [("src/b.ts".into(), 2, 14, 2322)]
    );

    write(&f.wt, &[("src/c.ts", "export const c: number = \"x\";\n")]);
    std::fs::remove_file(f.wt.join("src/a.ts")).unwrap();
    let d = f.diagnostics().await;
    assert_eq!(
        codes(&d),
        [
            ("src/b.ts".into(), 1, 24, 2307),
            ("src/c.ts".into(), 1, 14, 2322)
        ]
    );
    assert!(d["items"][0]["message"].as_str().unwrap().contains("'./a'"));

    write(&f.wt, &[("src/a.ts", common::A), ("src/b.ts", common::B)]);
    std::fs::remove_file(f.wt.join("src/c.ts")).unwrap();
    let hover = f
        .check(Op::Hover {
            file: file("src/b.ts"),
            line: 2,
            column: 22,
        })
        .await
        .unwrap();
    assert_eq!(
        hover["text"],
        "function addOne: (n: number) => number\n\nAdds one."
    );
    let def = f
        .check(Op::Definition {
            file: file("src/b.ts"),
            line: 2,
            column: 22,
        })
        .await
        .unwrap();
    assert_eq!(def["locations"][0]["file"], "src/a.ts");
    assert_eq!(
        (
            def["locations"][0]["line"].as_u64(),
            def["locations"][0]["column"].as_u64()
        ),
        (Some(2), Some(17))
    );
    let refs = f
        .check(Op::References {
            file: file("src/a.ts"),
            line: 2,
            column: 17,
        })
        .await
        .unwrap();
    assert_eq!(refs["locations"].as_array().unwrap().len(), 3);
    let syms = f
        .check(Op::Symbols {
            file: file("src/a.ts"),
        })
        .await
        .unwrap();
    assert_eq!(syms["symbols"][0]["name"], "addOne");
    let one = f
        .check(Op::Diagnostics {
            file: Some(file("src/b.ts")),
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(one["total"], 0);
    f.broker.shutdown().await;
}

#[tokio::test]
async fn base_changes_reach_agents() {
    let f = fixture();
    assert_eq!(f.diagnostics().await["total"], 0);
    write(
        &f.base,
        &[(
            "src/a.ts",
            "export function addOne(n: number): string { return String(n); }\n",
        )],
    );
    let hover = f
        .check(Op::Hover {
            file: file("src/a.ts"),
            line: 2,
            column: 17,
        })
        .await
        .unwrap();
    assert_eq!(
        hover["text"], "function addOne: (n: number) => number\n\nAdds one.",
        "the agent's own a.ts wins"
    );
    std::fs::remove_file(f.wt.join("src/a.ts")).unwrap();
    std::fs::copy(f.base.join("src/a.ts"), f.wt.join("src/a.ts")).unwrap();
    let hover = f
        .check(Op::Hover {
            file: file("src/b.ts"),
            line: 2,
            column: 22,
        })
        .await
        .unwrap();
    assert_eq!(hover["text"], "function addOne: (n: number) => string");
    f.broker.shutdown().await;
}

#[tokio::test]
async fn unshared_when_dependencies_differ_or_the_diff_is_huge() {
    let f = fixture();
    write(
        &f.wt,
        &[
            (
                "package.json",
                "{\"dependencies\":{\"left-pad\":\"1.0.0\"}}",
            ),
            ("src/b.ts", "export const x: string = 1;\n"),
        ],
    );
    let d = f.diagnostics().await;
    assert_eq!(
        d["sharing"],
        serde_json::json!({ "mode": "unshared", "reason": "dependencies differ" })
    );
    assert_eq!(
        codes(&d),
        [("src/b.ts".into(), 1, 14, 2322)],
        "own paths, still relative"
    );
    std::fs::remove_file(f.wt.join("package.json")).unwrap();
    let tight = Settings {
        huge_diff_ratio: 0.01,
        ..settings()
    };
    let d = f
        .check_with(
            Op::Diagnostics {
                file: None,
                limit: None,
            },
            &tight,
        )
        .await
        .unwrap();
    assert!(
        d["sharing"]["reason"]
            .as_str()
            .unwrap()
            .starts_with("large diff (1 of "),
        "{d}"
    );
    f.broker.shutdown().await;
}

#[tokio::test]
async fn a_different_typescript_gets_its_own_process() {
    let f = fixture();
    // node_modules/typescript in the worktree: the same files, reporting another version.
    let real = common::workspace().join("packages/tsbridge/node_modules");
    let ts = f.wt.join("node_modules/typescript");
    std::fs::remove_file(&ts).unwrap();
    std::fs::create_dir(&ts).unwrap();
    for entry in std::fs::read_dir(real.join("typescript")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() != "package.json" {
            std::os::unix::fs::symlink(entry.path(), ts.join(entry.file_name())).unwrap();
        }
    }
    let pkg = std::fs::read_to_string(real.join("typescript/package.json"))
        .unwrap()
        .replace("\"7.1.0-beta\"", "\"7.1.0-other\"");
    std::fs::write(ts.join("package.json"), pkg).unwrap();
    std::os::unix::fs::symlink(
        real.join("@typescript"),
        f.wt.join("node_modules/@typescript"),
    )
    .unwrap();
    let d = f.diagnostics().await;
    assert_eq!(
        d["sharing"]["reason"],
        "TypeScript 7.1.0-other differs from the base's 7.1.0-beta"
    );
    assert_eq!(d["total"], 0);
    f.broker.shutdown().await;
}

#[tokio::test]
async fn an_old_checker_is_recycled_between_queries() {
    let f = fixture();
    let s = Settings {
        recycle_after: Duration::ZERO,
        ..settings()
    };
    let all = || Op::Diagnostics {
        file: None,
        limit: None,
    };
    assert_eq!(f.check_with(all(), &s).await.unwrap()["total"], 0);
    let pid = f.broker.status().await[0]["pid"].clone();
    f.broker.maintain().await;
    let status = f.broker.status().await;
    assert_eq!(
        status[0]["pid"],
        Value::Null,
        "stopped until the next query"
    );
    assert_eq!(status[0]["agents"][0]["snapshot"], false);
    write(&f.wt, &[("src/b.ts", "export const x: string = 1;\n")]);
    assert_eq!(f.check_with(all(), &s).await.unwrap()["total"], 1);
    let status = f.broker.status().await;
    assert_ne!(status[0]["pid"], pid);
    assert_eq!(status[0]["restarts"], 0, "a recycle is not a crash");
    f.broker.shutdown().await;
}

#[tokio::test]
async fn restarts_after_the_bridge_dies() {
    let f = fixture();
    assert_eq!(f.diagnostics().await["total"], 0);
    let pid = f.broker.status().await[0]["pid"].to_string();
    std::process::Command::new("kill")
        .args(["-9", &pid])
        .status()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    write(&f.wt, &[("src/b.ts", "export const x: string = 1;\n")]);
    assert_eq!(f.diagnostics().await["total"], 1);
    let status = f.broker.status().await;
    assert_eq!(status[0]["restarts"], 1);
    assert_ne!(status[0]["pid"].to_string(), pid);
    f.broker.shutdown().await;
}

#[tokio::test]
async fn idle_snapshots_are_disposed_and_come_back() {
    let f = fixture();
    let s = Settings {
        idle_dispose: Duration::ZERO,
        ..settings()
    };
    f.check_with(
        Op::Diagnostics {
            file: None,
            limit: None,
        },
        &s,
    )
    .await
    .unwrap();
    assert_eq!(f.broker.status().await[0]["agents"][0]["snapshot"], true);
    f.broker.maintain().await;
    assert_eq!(f.broker.status().await[0]["agents"][0]["snapshot"], false);
    assert_eq!(
        f.check_with(
            Op::Diagnostics {
                file: None,
                limit: Some(0)
            },
            &s
        )
        .await
        .unwrap()["total"],
        0
    );
    f.broker.dispose_agent(&f.base, "a").await;
    assert_eq!(
        f.broker.status().await[0]["agents"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    f.broker.shutdown().await;
}

#[tokio::test]
async fn errors_are_clear() {
    let f = fixture();
    let err = f
        .check(Op::Hover {
            file: "/elsewhere/x.ts".into(),
            line: 1,
            column: 1,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err}");
    std::fs::remove_file(f.base.join("node_modules/typescript")).unwrap();
    let other = Broker::new(&f.tmp.path().join("home2"));
    let req = Request {
        repo: f.base.clone(),
        base: f.base.clone(),
        agent: "a".into(),
        worktree: f.wt.clone(),
        op: Op::Symbols {
            file: "src/a.ts".into(),
        },
    };
    let err = other.check(req, &settings()).await.unwrap_err();
    assert!(
        err.to_string().contains("TypeScript is not installed"),
        "{err}"
    );
    f.broker.shutdown().await;
}

async fn diagnostics_as(f: &Fixture, agent: &str, wt: &std::path::Path, s: &Settings) -> Value {
    let req = Request {
        repo: f.base.clone(),
        base: f.base.clone(),
        agent: agent.into(),
        worktree: wt.to_path_buf(),
        op: Op::Diagnostics {
            file: None,
            limit: None,
        },
    };
    f.broker.check(req, s).await.unwrap()
}

async fn has_snapshot(f: &Fixture, agent: &str) -> bool {
    let status = f.broker.status().await;
    let agents = status[0]["agents"].as_array().unwrap();
    agents.iter().find(|a| a["name"] == agent).unwrap()["snapshot"] == true
}

#[tokio::test]
async fn two_agents_editing_the_same_file_each_see_their_own() {
    let f = fixture();
    let wt2 = common::project(f.tmp.path(), "wt2");
    write(&f.wt, &[("src/b.ts", "export const total: string = 1;\n")]);
    write(
        &wt2,
        &[("src/b.ts", "\n\nexport const total: boolean = \"x\";\n")],
    );
    let a = diagnostics_as(&f, "a", &f.wt, &settings()).await;
    let b = diagnostics_as(&f, "b", &wt2, &settings()).await;
    assert_eq!(codes(&a), [("src/b.ts".into(), 1, 14, 2322)]);
    assert_eq!(codes(&b), [("src/b.ts".into(), 3, 14, 2322)]);
    assert!(
        b["items"][0]["message"]
            .as_str()
            .unwrap()
            .contains("'boolean'")
    );
    assert_eq!(
        (&a["sharing"]["mode"], &b["sharing"]["mode"]),
        (&"shared".into(), &"shared".into())
    );
    let again = diagnostics_as(&f, "a", &f.wt, &settings()).await;
    assert_eq!(
        codes(&again),
        [("src/b.ts".into(), 1, 14, 2322)],
        "b's edit never reaches a"
    );
    f.broker.shutdown().await;
}

#[tokio::test]
async fn a_worktree_tsconfig_change_applies_to_that_agent_only() {
    let f = fixture();
    let wt2 = common::project(f.tmp.path(), "wt2");
    // An untyped parameter is an error (TS7006) only under strict.
    for wt in [&f.wt, &wt2] {
        write(wt, &[("src/c.ts", "export function f(x) { return x; }\n")]);
    }
    let loose = common::TSCONFIG.replace("\"strict\": true", "\"strict\": false");
    write(&f.wt, &[("tsconfig.json", &loose)]);
    let a = diagnostics_as(&f, "a", &f.wt, &settings()).await;
    let b = diagnostics_as(&f, "b", &wt2, &settings()).await;
    assert!(codes(&a).is_empty(), "{a}");
    assert_eq!(codes(&b), [("src/c.ts".into(), 1, 19, 7006)]);
    assert_eq!(a["sharing"]["mode"], "shared");
    f.broker.shutdown().await;
}

#[tokio::test]
async fn above_the_memory_limit_the_least_recently_used_snapshot_goes() {
    let f = fixture();
    let wt2 = common::project(f.tmp.path(), "wt2");
    let s = Settings {
        group_memory_bytes: 1,
        ..settings()
    };
    diagnostics_as(&f, "a", &f.wt, &s).await;
    diagnostics_as(&f, "b", &wt2, &s).await;
    f.broker.maintain().await;
    assert!(!has_snapshot(&f, "a").await, "a was used least recently");
    assert!(has_snapshot(&f, "b").await, "one eviction per pass");
    assert_eq!(diagnostics_as(&f, "a", &f.wt, &s).await["total"], 0);
    assert!(
        has_snapshot(&f, "a").await,
        "and it comes back on the next query"
    );
    f.broker.shutdown().await;
}

#[tokio::test]
async fn clashes_between_agents_are_found_before_either_merges() {
    let f = fixture();
    let rename = f.wt.clone();
    let caller = common::project(f.tmp.path(), "caller");
    let other = common::project(f.tmp.path(), "other");
    // rename: addOne becomes plusOne, with its one caller updated. Clean on its own.
    write(
        &rename,
        &[
            ("src/a.ts", &common::A.replace("addOne", "plusOne")),
            ("src/b.ts", &common::B.replace("addOne", "plusOne")),
        ],
    );
    // caller: a new file calling addOne, plus an error of its own that is not a clash.
    write(
        &caller,
        &[(
            "src/c.ts",
            "import { addOne } from \"./a\";\nexport const bad: string = addOne(2);\n",
        )],
    );
    // other: an unrelated file, and a.ts's comment, next to rename's edit (git cannot merge).
    write(
        &other,
        &[
            ("src/d.ts", "export const d = 1;\n"),
            ("src/a.ts", &common::A.replace("Adds one.", "Adds 1.")),
        ],
    );
    let agents = [
        ("rename".to_string(), rename.clone()),
        ("caller".to_string(), caller.clone()),
        ("other".to_string(), other.clone()),
    ];
    let round = |only| {
        let agents = agents.clone();
        let f = &f;
        async move {
            f.broker
                .check_clashes(&f.base, &f.base, &agents, &settings(), only)
                .await
                .unwrap()
        }
    };
    round(None).await;
    assert_eq!(
        f.broker.status().await[0]["pairs_checked"],
        0,
        "layers must hold still for a round first"
    );
    round(None).await;
    let status = f.broker.status().await;
    assert_eq!(status[0]["pairs_checked"], 3, "{status}");

    let mine = f.broker.clashes(&f.base, "caller").await;
    let with = |name: &str| mine.iter().find(|p| p["with"] == name).unwrap().clone();
    let clash = &with("rename")["clashes"];
    assert_eq!(clash.as_array().unwrap().len(), 1, "{clash}");
    assert_eq!(clash[0]["file"], "src/c.ts");
    assert_eq!(clash[0]["line"], 1);
    assert!(
        clash[0]["message"].as_str().unwrap().contains("addOne"),
        "{clash}"
    );
    assert_eq!(with("other")["clashes"], serde_json::json!([]));
    let unmerged = f.broker.clashes(&f.base, "other").await;
    let unmerged = unmerged.iter().find(|p| p["with"] == "rename").unwrap();
    assert_eq!(unmerged["unmerged"], serde_json::json!(["src/a.ts"]));
    assert_eq!(unmerged["clashes"], serde_json::json!([]));

    // Diagnostics answers carry the clash; the agent's own error stays its own.
    let d = diagnostics_as(&f, "caller", &caller, &settings()).await;
    assert_eq!(codes(&d), [("src/c.ts".into(), 2, 14, 2322)]);
    assert_eq!(d["conflicts"][0]["with"], "rename", "{d}");
    assert_eq!(
        d["conflicts"].as_array().unwrap().len(),
        1,
        "clean pairs stay out"
    );

    // Fixed in caller: an on-demand check sees it at once, without waiting to settle.
    write(&caller, &[("src/c.ts", "export const fine: number = 1;\n")]);
    round(Some("caller")).await;
    let now = f.broker.clashes(&f.base, "caller").await;
    assert!(
        now.iter().all(|p| p["clashes"] == serde_json::json!([])),
        "{now:?}"
    );
    f.broker.shutdown().await;
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Commits what is in `base` on `main` and returns a way to make worktrees of it.
fn repo(base: &std::path::Path) -> impl Fn(&str) -> (String, PathBuf) + '_ {
    git(base, &["init", "-q", "-b", "main"]);
    git(base, &["add", "."]);
    git(base, &["commit", "-q", "-m", "init"]);
    move |name| {
        let wt = base.parent().unwrap().join(name);
        git(
            base,
            &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()],
        );
        (name.to_string(), wt.canonicalize().unwrap())
    }
}

/// The agent's pair with `with`.
async fn pair(broker: &Broker, repo: &std::path::Path, agent: &str, with: &str) -> Value {
    let pairs = broker.clashes(repo, agent).await;
    let found = pairs.iter().find(|p| p["with"] == with);
    found
        .unwrap_or_else(|| panic!("no pair {agent}+{with}"))
        .clone()
}

#[tokio::test]
async fn in_a_git_repo_git_says_what_merges_and_the_checker_what_type_checks() {
    if !strata_git::merges_trees() {
        return; // git older than 2.38: covered by the plain-folder test above
    }
    let f = fixture();
    let old = "export const one = 1;\nexport const two = 2;\nexport const three = 3;\nexport const four = 4;\n";
    write(
        &f.base,
        &[
            (".gitignore", "node_modules\n"),
            ("styles.css", "a { color: red; }\n"),
            ("src/old.ts", old),
        ],
    );
    let agent = repo(&f.base);
    let agents = ["rename", "caller", "blue", "green", "mover", "editor"].map(|name| {
        let (name, wt) = agent(name);
        common::link_typescript(&wt);
        (name, wt)
    });
    let wt = |name: &str| agents.iter().find(|(n, _)| n == name).unwrap().1.clone();
    // A type clash between two agents that git merges: the rename and a new caller.
    write(
        &wt("rename"),
        &[
            ("src/a.ts", &common::A.replace("addOne", "plusOne")),
            ("src/b.ts", &common::B.replace("addOne", "plusOne")),
        ],
    );
    let c = "import { addOne } from \"./a\";\nexport const three = addOne(2);\n";
    write(&wt("caller"), &[("src/c.ts", c)]);
    // The same line of a file the checker never reads.
    write(&wt("blue"), &[("styles.css", "a { color: blue; }\n")]);
    write(&wt("green"), &[("styles.css", "a { color: green; }\n")]);
    // A rename, and an edit of the file under its old name.
    std::fs::rename(
        wt("mover").join("src/old.ts"),
        wt("mover").join("src/new.ts"),
    )
    .unwrap();
    write(&wt("editor"), &[("src/old.ts", &old.replace("4", "40"))]);

    let round = |only| {
        let (f, agents) = (&f, agents.to_vec());
        async move {
            f.broker
                .check_clashes(&f.base, &f.base, &agents, &settings(), only)
                .await
                .unwrap()
        }
    };
    round(None).await;
    round(None).await;
    let status = f.broker.status().await;
    assert_eq!(status[0]["pairs_checked"], 15, "{status}");
    let open = status[0]["conflicts"].as_array().unwrap();
    assert_eq!(open.len(), 3, "{status}");

    let clash = pair(&f.broker, &f.base, "caller", "rename").await;
    assert_eq!(clash["clashes"][0]["file"], "src/c.ts", "{clash}");
    assert_eq!(clash["unmerged"], serde_json::json!([]));
    let css = pair(&f.broker, &f.base, "blue", "green").await;
    assert_eq!(css["unmerged"], serde_json::json!(["styles.css"]), "{css}");
    let moved = pair(&f.broker, &f.base, "editor", "mover").await;
    assert_eq!(moved["unmerged"], serde_json::json!([]), "git follows it");
    let skipped = moved["skipped"].as_str().unwrap();
    assert!(
        skipped.contains("src/old.ts") && skipped.contains("rename"),
        "{skipped}"
    );

    // Each agent has an index in Strata's own folder. A removed agent's goes, with its pairs.
    let store = std::fs::read_dir(f.tmp.path().join("home/clash")).unwrap();
    let store = store.map(|e| e.unwrap().path()).next().unwrap();
    assert!(store.join("blue.index").exists() && store.join("objects").is_dir());
    assert!(!store.join("tmp/caller+rename").exists(), "merged files go");
    f.broker.dispose_agent(&f.base, "blue").await;
    assert!(!store.join("blue.index").exists());
    assert!(
        f.broker
            .clashes(&f.base, "green")
            .await
            .iter()
            .all(|p| p["with"] != "blue")
    );

    // The folder can go at any time: the next round rebuilds it and answers the same.
    assert!(f.broker.empty_clash_store(&f.base).await);
    assert!(!store.exists());
    round(Some("caller")).await;
    let clash = pair(&f.broker, &f.base, "caller", "rename").await;
    assert_eq!(clash["clashes"][0]["file"], "src/c.ts", "{clash}");
    assert!(store.join("caller.index").exists());
    f.broker.shutdown().await;
}

#[tokio::test]
async fn without_the_shared_checker_git_still_says_what_cannot_merge() {
    if !strata_git::merges_trees() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().join("base");
    write(&base, &[("README.md", "one\ntwo\n"), ("notes.txt", "x\n")]);
    let base = base.canonicalize().unwrap();
    let agent = repo(&base);
    let agents = ["a", "b", "c"].map(agent);
    write(&agents[0].1, &[("README.md", "ONE\ntwo\n")]);
    write(&agents[1].1, &[("README.md", "uno\ntwo\n")]);
    write(&agents[2].1, &[("notes.txt", "y\n")]);
    let broker = Broker::new(&tmp.path().join("home"));
    for _ in 0..2 {
        broker
            .check_clashes(&base, &base, &agents, &settings(), None)
            .await
            .unwrap();
    }
    let status = broker.status().await;
    assert_eq!(status[0]["typescript"], "", "{status}");
    assert_eq!(status[0]["pairs_checked"], 3, "{status}");
    let open = status[0]["conflicts"].as_array().unwrap();
    assert_eq!(open.len(), 1, "{status}");
    assert_eq!(open[0]["agents"], serde_json::json!(["a", "b"]));
    assert_eq!(open[0]["unmerged"], serde_json::json!(["README.md"]));

    // Resolved in b: seen at its next look, without waiting for the back-off.
    write(
        &agents[1].1,
        &[("README.md", "one\ntwo\n"), ("b.txt", "b\n")],
    );
    broker
        .check_clashes(&base, &base, &agents, &settings(), Some("b"))
        .await
        .unwrap();
    assert_eq!(broker.status().await[0]["conflicts"], serde_json::json!([]));
}
