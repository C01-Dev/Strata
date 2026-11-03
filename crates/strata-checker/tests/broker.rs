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
