//! PLAN task 2.7: the shared checker's answers must equal a dedicated snapshot per worktree.
//! Shared = one broker on the base, each worktree layered on it at base paths. Dedicated = for
//! each worktree, its own broker (own bridge, own tsgo) whose base *is* the worktree.
//! Compared: whole-project diagnostics, and per sampled file its diagnostics and symbols, and per
//! sampled position hover, definition and references.
//!
//! `monorepo` runs by default. `excalidraw` and `calcom` need `AGENTS=5 bench/setup.sh` and run
//! with `cargo test --release -p strata-checker --test parity -- --ignored --nocapture`.

mod common;

use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use strata_checker::broker::{Broker, Op, Request, Settings};
use strata_checker::sample;

fn settings(tsconfigs: &[&str]) -> Settings {
    Settings {
        tsconfigs: tsconfigs.iter().map(PathBuf::from).collect(),
        extra_paths: vec![],
        idle_dispose: Duration::from_secs(3600),
        huge_diff_ratio: 1.0,
        group_memory_bytes: u64::MAX,
        base_rescan: Duration::from_secs(3600),
        recycle_after: Duration::from_secs(86_400),
    }
}

/// The agent's changed source files plus `untouched` evenly spread unchanged ones.
fn sample_files(base: &Path, wt: &Path, scopes: &[String], untouched: usize) -> Vec<String> {
    let mut files = sample::changed_sources(base, wt).unwrap();
    let changed: HashSet<String> = files.iter().cloned().collect();
    files.extend(sample::spread(wt, scopes, &changed, untouched).unwrap());
    files
}

type Answer = Result<Value, String>;

async fn ask(broker: &Broker, repo: &Path, agent: &str, wt: &Path, op: Op, s: &Settings) -> Answer {
    let req = Request {
        repo: repo.to_path_buf(),
        base: repo.to_path_buf(),
        agent: agent.into(),
        worktree: wt.to_path_buf(),
        op,
    };
    match broker.check(req, s).await {
        Ok(mut v) => {
            if let Value::Object(map) = &mut v {
                map.remove("sharing");
            }
            Ok(v)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn ops(wt: &Path, files: &[String]) -> Vec<(String, Op)> {
    let mut out = vec![(
        "diagnostics (project)".to_string(),
        Op::Diagnostics {
            file: None,
            limit: None,
        },
    )];
    for f in files {
        let text = std::fs::read_to_string(wt.join(f)).unwrap_or_default();
        out.push((
            format!("diagnostics {f}"),
            Op::Diagnostics {
                file: Some(f.clone()),
                limit: None,
            },
        ));
        out.push((format!("symbols {f}"), Op::Symbols { file: f.clone() }));
        for (i, (line, column)) in sample::identifiers(&text, 3).into_iter().enumerate() {
            let at = |kind: &str| format!("{kind} {f}:{line}:{column}");
            out.push((
                at("hover"),
                Op::Hover {
                    file: f.clone(),
                    line,
                    column,
                },
            ));
            out.push((
                at("definition"),
                Op::Definition {
                    file: f.clone(),
                    line,
                    column,
                },
            ));
            if i == 0 {
                out.push((
                    at("references"),
                    Op::References {
                        file: f.clone(),
                        line,
                        column,
                    },
                ));
            }
        }
    }
    out
}

/// Returns mismatch descriptions; prints a summary line.
async fn parity(
    name: &str,
    base: &Path,
    worktrees: &[PathBuf],
    tsconfigs: &[&str],
    untouched: usize,
) -> Vec<String> {
    let s = settings(tsconfigs);
    let home = tempfile::tempdir().unwrap();
    let shared = Broker::new(&home.path().join("shared"));
    let scopes: Vec<String> = tsconfigs
        .iter()
        .map(|t| {
            Path::new(t)
                .parent()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let (mut compared, mut mismatches, mut shared_time, mut dedicated_time) =
        (0, vec![], Duration::ZERO, Duration::ZERO);
    // Equal answers can be trivially equal; count errors and empty answers to show they are not.
    let (mut errors, mut empty, mut problems) = (0, 0, 0);
    let mut kinds = std::collections::BTreeMap::<String, usize>::new();
    for (i, wt) in worktrees.iter().enumerate() {
        let agent = format!("wt{}", i + 1);
        let plan = ops(wt, &sample_files(base, wt, &scopes, untouched));
        let dedicated = Broker::new(&home.path().join(format!("dedicated-{i}")));
        for (what, op) in plan {
            let t = Instant::now();
            let a = ask(&shared, base, &agent, wt, op.clone(), &s).await;
            shared_time += t.elapsed();
            let t = Instant::now();
            let b = ask(&dedicated, wt, "ref", wt, op, &s).await;
            dedicated_time += t.elapsed();
            compared += 1;
            match &a {
                Err(e) => {
                    errors += 1;
                    // Group by message shape: drop paths and positions.
                    let shape: String = e
                        .split(':')
                        .next()
                        .unwrap_or(e)
                        .chars()
                        .filter(|c| !c.is_ascii_digit())
                        .take(80)
                        .collect();
                    *kinds.entry(shape).or_default() += 1;
                }
                Ok(v)
                    if v.is_null()
                        || v["locations"].as_array().is_some_and(Vec::is_empty)
                        || v["symbols"].as_array().is_some_and(Vec::is_empty) =>
                {
                    empty += 1
                }
                Ok(v) => problems += v["total"].as_u64().unwrap_or(0),
            }
            if a != b {
                let show = |x: &Answer| {
                    x.as_ref()
                        .map(|v| v.to_string())
                        .unwrap_or_else(|e| format!("error: {e}"))
                        .chars()
                        .take(400)
                        .collect::<String>()
                };
                mismatches.push(format!(
                    "{agent} {what}\n  shared:    {}\n  dedicated: {}",
                    show(&a),
                    show(&b)
                ));
            }
        }
        dedicated.shutdown().await;
    }
    shared.shutdown().await;
    println!(
        "parity {name}: {} worktrees, {compared} answers compared, {} mismatches ({errors} errors, {empty} empty, {problems} diagnostics in total); shared {:.1} s, dedicated {:.1} s",
        worktrees.len(),
        mismatches.len(),
        shared_time.as_secs_f64(),
        dedicated_time.as_secs_f64()
    );
    for (kind, n) in kinds {
        println!("  {n} x error: {kind}");
    }
    mismatches
}

fn assert_parity(mismatches: Vec<String>) {
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn monorepo() {
    let tmp = tempfile::tempdir().unwrap();
    let fixture = common::workspace().join("tests/fixtures/monorepo");
    let checkout = |name: &str| {
        let dir = tmp.path().join(name);
        copy_dir(&fixture, &dir);
        common::link_typescript(&dir);
        dir.canonicalize().unwrap()
    };
    let base = checkout("base");
    let (wt1, wt2, wt3) = (checkout("wt1"), checkout("wt2"), checkout("wt3"));
    // wt1: a signature change that breaks callers in other packages, plus an added file.
    common::write(
        &wt1,
        &[
            (
                "packages/core/src/math.ts",
                "/** Sum, as text now. */\nexport function sum(values: readonly number[]): string {\n  return String(values.length);\n}\n\nexport function mean(values: readonly number[]): number | undefined {\n  return values.length ? 1 : undefined;\n}\n\nexport const GOLDEN_RATIO = 1.618;\n",
            ),
            (
                "apps/web/src/extra.ts",
                "import { Circle } from \"@mono/core\";\nexport const big: Circle = new Circle(\"9\");\n",
            ),
        ],
    );
    // wt2: a deleted module, so imports fail across packages.
    std::fs::remove_file(wt2.join("packages/core/src/shapes.ts")).unwrap();
    // wt3: a tsconfig change (a separate program inside the snapshot) and an unused local.
    common::write(
        &wt3,
        &[
            (
                "apps/web/tsconfig.json",
                "{ \"extends\": \"../../tsconfig.base.json\", \"compilerOptions\": { \"noUnusedLocals\": true }, \"include\": [\"src\"] }",
            ),
            (
                "apps/web/src/main.ts",
                &std::fs::read_to_string(base.join("apps/web/src/main.ts"))
                    .unwrap()
                    .replace(
                        "export function page",
                        "const unused = 1;\nexport function page",
                    ),
            ),
        ],
    );
    let tsconfigs = [
        "apps/web/tsconfig.json",
        "packages/core/tsconfig.json",
        "packages/ui/tsconfig.json",
    ];
    assert_parity(parity("monorepo", &base, &[wt1, wt2, wt3], &tsconfigs, 3).await);
}

/// A Phase 0 fixture from bench/setup.sh: its base, wt1..wt5 and its tsconfig.
async fn bench_fixture(name: &str) {
    let dir = common::workspace().join("bench/fixtures").join(name);
    assert!(
        dir.join("wt5").exists(),
        "run `AGENTS=5 bench/setup.sh {name}` first"
    );
    let setup = std::fs::read_to_string(common::workspace().join("bench/setup.sh")).unwrap();
    let tsconfig = setup
        .lines()
        .find(|l| l.trim_start().starts_with(&format!("\"{name} ")))
        .unwrap()
        .trim()
        .trim_matches('"')
        .split_whitespace()
        .nth(3)
        .unwrap()
        .to_string();
    let worktrees: Vec<PathBuf> = (1..=5).map(|i| dir.join(format!("wt{i}"))).collect();
    assert_parity(parity(name, &dir.join("base"), &worktrees, &[&tsconfig], 5).await);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs bench/fixtures (bench/setup.sh)"]
async fn excalidraw() {
    bench_fixture("excalidraw").await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs bench/fixtures (bench/setup.sh)"]
async fn calcom() {
    bench_fixture("calcom").await;
}
