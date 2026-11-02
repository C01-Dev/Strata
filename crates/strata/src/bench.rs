//! `strata bench`: Phase 0's memory protocol on Strata's own checker code, kept as a feature.
//! baseline = one broker per worktree (its own bridge and tsgo, a snapshot at its own paths);
//! shared = one broker on the base with every worktree layered on it. Memory is the process
//! tree's footprint (macOS `phys_footprint`, Linux PSS) sampled every 250 ms, this process
//! included on both sides; the baseline leaves out its bridges' Node processes, since a
//! separate tsgo per agent has none. CSV columns match bench/results/phase0.csv.

use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use strata_checker::broker::{Broker, Op, Request, Settings};
use strata_checker::sample;
use strata_core::{Error, Result};
use tokio::task::JoinSet;

pub struct Args {
    pub fixture: PathBuf,
    pub tsconfig: String,
    pub agents: usize,
    /// whole: every agent checks the whole project at once (Gate 0); files: each checks its edited files.
    pub workload: String,
    pub modes: Vec<String>,
    /// Extra readings this many seconds after the last query.
    pub idle: Vec<u64>,
    pub out: Option<PathBuf>,
}

const CSV_HEADER: &str = "fixture,workload,mode,agents,harness_mb,peak_mb,warm_mb,idle_60_mb,idle_900_mb,loaded_s,diagnostics_done_s,hover_done_s,typescript,cpu,ram_gb,os,fs";

struct Sampler {
    start: Instant,
    samples: Arc<Mutex<Vec<(u64, u64)>>>,
    exclude: Arc<Mutex<Vec<u32>>>,
    stop: Arc<AtomicBool>,
}

impl Sampler {
    fn start() -> Self {
        let s = Self {
            start: Instant::now(),
            samples: Arc::default(),
            exclude: Arc::default(),
            stop: Arc::default(),
        };
        let (samples, exclude, stop, start) = (
            s.samples.clone(),
            s.exclude.clone(),
            s.stop.clone(),
            s.start,
        );
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let left_out: u64 = exclude
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|p| strata_proc::process_memory(*p))
                    .sum();
                let total = strata_proc::tree_memory(std::process::id()).saturating_sub(left_out);
                samples
                    .lock()
                    .unwrap()
                    .push((start.elapsed().as_millis() as u64, total));
                std::thread::sleep(Duration::from_millis(250));
            }
        });
        s
    }

    fn ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn at(&self, ms: u64) -> u64 {
        self.samples
            .lock()
            .unwrap()
            .iter()
            .min_by_key(|(t, _)| t.abs_diff(ms))
            .map_or(0, |(_, b)| *b)
    }

    fn peak(&self, until: u64) -> u64 {
        self.samples
            .lock()
            .unwrap()
            .iter()
            .filter(|(t, _)| *t <= until)
            .map(|(_, b)| *b)
            .max()
            .unwrap_or(0)
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

struct Agent {
    broker: Arc<Broker>,
    repo: PathBuf,
    name: String,
    worktree: PathBuf,
    edited: Vec<String>,
}

impl Agent {
    async fn check(&self, op: Op, s: &Settings) -> Result<Value> {
        let req = Request {
            repo: self.repo.clone(),
            base: self.repo.clone(),
            agent: self.name.clone(),
            worktree: self.worktree.clone(),
            op,
        };
        self.broker.check(req, s).await
    }
}

type Work = std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>;

/// Runs `work` for every agent concurrently; the first error wins.
async fn each(agents: &[Arc<Agent>], work: impl Fn(Arc<Agent>) -> Work) -> Result<()> {
    let mut set = JoinSet::new();
    for a in agents {
        set.spawn(work(a.clone()));
    }
    while let Some(done) = set.join_next().await {
        done.map_err(|e| Error::Checker(e.to_string()))??;
    }
    Ok(())
}

fn sh(cmd: &str) -> String {
    Command::new("sh")
        .args(["-c", cmd])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// cpu, RAM in GiB, OS, filesystem of `path`.
fn machine(path: &Path) -> (String, f64, String, String) {
    let p = path.display();
    if cfg!(target_os = "macos") {
        let ram: f64 = sh("sysctl -n hw.memsize").parse().unwrap_or(0.0);
        let fs = sh(&format!(
            "diskutil info \"$(df '{p}' | tail -1 | cut -d' ' -f1)\" | sed -n 's/^ *Type (Bundle): *//p'"
        ));
        (
            sh("sysctl -n machdep.cpu.brand_string"),
            ram / 1073741824.0,
            sh("uname -sr"),
            fs,
        )
    } else {
        let ram: f64 = sh("awk '/MemTotal/ {print $2}' /proc/meminfo")
            .parse()
            .unwrap_or(0.0);
        let cpu = sh("awk -F': ' '/model name|^Model/ {print $2; exit}' /proc/cpuinfo");
        (
            cpu,
            ram / 1048576.0,
            sh("uname -sr"),
            sh(&format!("stat -f -c %T '{p}'")),
        )
    }
}

pub async fn run(args: Args) -> Result<()> {
    let fixture = args
        .fixture
        .canonicalize()
        .map_err(|_| Error::NotFound(format!("{} does not exist", args.fixture.display())))?;
    let name = fixture
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let base = fixture.join("base");
    let worktrees: Vec<PathBuf> = (1..=args.agents)
        .map(|i| fixture.join(format!("wt{i}")))
        .collect();
    if let Some(missing) = std::iter::once(&base)
        .chain(&worktrees)
        .find(|p| !p.is_dir())
    {
        return Err(Error::NotFound(format!(
            "{} is missing; run `AGENTS={} bench/setup.sh {name}`",
            missing.display(),
            args.agents
        )));
    }
    let settings = Settings {
        tsconfigs: vec![PathBuf::from(&args.tsconfig)],
        extra_paths: vec![],
        idle_dispose: Duration::from_secs(86_400),
        huge_diff_ratio: 1.0,
        group_memory_bytes: u64::MAX,
        base_rescan: Duration::from_secs(86_400),
    };
    // 20 hover targets: 2 identifiers in each of 10 source files no agent edited (as in Phase 0).
    let mut edited = vec![];
    for wt in &worktrees {
        edited.push(sample::changed_sources(&base, wt)?);
    }
    let all_edited: HashSet<String> = edited.iter().flatten().cloned().collect();
    let scope = Path::new(&args.tsconfig)
        .parent()
        .unwrap_or(Path::new(""))
        .to_string_lossy()
        .into_owned();
    let mut hovers = vec![];
    for f in sample::spread(&base, &[scope], &all_edited, 10)? {
        let text = std::fs::read_to_string(base.join(&f))?;
        hovers.extend(
            sample::identifiers(&text, 2)
                .into_iter()
                .map(|(line, column)| (f.clone(), line, column)),
        );
    }
    let hovers = Arc::new(hovers);
    let (cpu, ram, os, fs) = machine(&fixture);
    let mut results = vec![];
    for mode in &args.modes {
        let home = std::env::temp_dir().join(format!("strata-bench-{}-{mode}", std::process::id()));
        let row = run_mode(
            mode,
            &args,
            &base,
            &worktrees,
            &edited,
            hovers.clone(),
            &settings,
            &home,
        )
        .await;
        let _ = std::fs::remove_dir_all(&home);
        let (mb, marks, typescript) = row?;
        let get = |k: &str| mb.iter().find(|(n, _)| n == k).map(|(_, v)| *v);
        println!(
            "{name} {} {mode} x{}: peak {:.2} GiB, warm {:.2} GiB",
            args.workload,
            args.agents,
            get("peak").unwrap_or(0.0) / 1024.0,
            get("warm").unwrap_or(0.0) / 1024.0
        );
        let cell = |k: &str| get(k).map(|v| format!("{v:.0}")).unwrap_or_default();
        let mark = |k: &str| {
            marks
                .iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| format!("{v:.3}"))
                .unwrap_or_default()
        };
        results.push((
            mode.clone(),
            get("peak").unwrap_or(0.0),
            get("warm").unwrap_or(0.0),
        ));
        let line = [
            name.clone(),
            args.workload.clone(),
            mode.clone(),
            args.agents.to_string(),
            cell("harness"),
            cell("peak"),
            cell("warm"),
            cell("idle_60"),
            cell("idle_900"),
            mark("loaded"),
            mark("diagnostics_done"),
            mark("hover_done"),
            typescript,
            format!("\"{cpu}\""),
            format!("{ram:.0}"),
            format!("\"{os}\""),
            fs.clone(),
        ]
        .join(",");
        if let Some(out) = &args.out {
            let fresh = !out.exists();
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(out)?;
            if fresh {
                writeln!(f, "{CSV_HEADER}")?;
            }
            writeln!(f, "{line}")?;
        }
    }
    if let (Some(b), Some(s)) = (
        results.iter().find(|r| r.0 == "baseline"),
        results.iter().find(|r| r.0 == "shared"),
    ) {
        println!(
            "S peak {:.0}%, S warm {:.0}%",
            (1.0 - s.1 / b.1) * 100.0,
            (1.0 - s.2 / b.2) * 100.0
        );
    }
    Ok(())
}

type Mb = Vec<(String, f64)>;
type Marks = Vec<(String, f64)>;

#[allow(clippy::too_many_arguments)]
async fn run_mode(
    mode: &str,
    args: &Args,
    base: &Path,
    worktrees: &[PathBuf],
    edited: &[Vec<String>],
    hovers: Arc<Vec<(String, u32, u32)>>,
    settings: &Settings,
    home: &Path,
) -> Result<(Mb, Marks, String)> {
    let sampler = Sampler::start();
    let mut marks: Marks = vec![];
    let mark = |marks: &mut Marks, name: &str, s: &Sampler| {
        marks.push((name.into(), s.ms() as f64 / 1000.0))
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    let shared = Arc::new(Broker::new(&home.join("shared")));
    let agents: Vec<Arc<Agent>> = worktrees
        .iter()
        .enumerate()
        .map(|(i, wt)| {
            let (broker, repo, name) = match mode {
                "baseline" => (
                    Arc::new(Broker::new(&home.join(format!("wt{}", i + 1)))),
                    wt.clone(),
                    "ref".to_string(),
                ),
                _ => (shared.clone(), base.to_path_buf(), format!("wt{}", i + 1)),
            };
            Arc::new(Agent {
                broker,
                repo,
                name,
                worktree: wt.clone(),
                edited: edited[i].clone(),
            })
        })
        .collect();
    let s = Arc::new(settings.clone());
    // Load: every agent's snapshot exists (an outline of its first edited file creates it).
    let st = s.clone();
    each(&agents, move |a| {
        let st = st.clone();
        Box::pin(async move {
            let file = a.edited.first().cloned().ok_or_else(|| {
                Error::NotFound(format!("{} edits no source file", a.worktree.display()))
            })?;
            a.check(Op::Symbols { file }, &st).await.map(drop)
        })
    })
    .await?;
    if mode == "baseline" {
        let mut bridges = vec![];
        for a in &agents {
            bridges.extend(
                a.broker
                    .status()
                    .await
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|g| g["pid"].as_u64())
                    .map(|p| p as u32),
            );
        }
        *sampler.exclude.lock().unwrap() = bridges;
    }
    mark(&mut marks, "loaded", &sampler);
    let (whole, st) = (args.workload == "whole", s.clone());
    each(&agents, move |a| {
        let st = st.clone();
        Box::pin(async move {
            if whole {
                a.check(
                    Op::Diagnostics {
                        file: None,
                        limit: None,
                    },
                    &st,
                )
                .await?;
            } else {
                for f in &a.edited {
                    a.check(
                        Op::Diagnostics {
                            file: Some(f.clone()),
                            limit: None,
                        },
                        &st,
                    )
                    .await?;
                }
            }
            Ok(())
        })
    })
    .await?;
    mark(&mut marks, "diagnostics_done", &sampler);
    let (st, skipped) = (s.clone(), Arc::new(std::sync::atomic::AtomicUsize::new(0)));
    let counter = skipped.clone();
    each(&agents, move |a| {
        let (st, hovers, skipped) = (st.clone(), hovers.clone(), counter.clone());
        Box::pin(async move {
            for (file, line, column) in hovers.iter() {
                let op = Op::Hover {
                    file: file.clone(),
                    line: *line,
                    column: *column,
                };
                match a.check(op, &st).await {
                    // A sampled file outside the project (as Phase 0's harness, keep going).
                    Err(Error::NotFound(_)) => skipped.fetch_add(1, Ordering::SeqCst),
                    r => r.map(|_| 0)?,
                };
            }
            Ok(())
        })
    })
    .await?;
    mark(&mut marks, "hover_done", &sampler);
    let skipped = skipped.load(Ordering::SeqCst);
    if skipped > 0 {
        println!("  {skipped} hovers skipped: file not in the project");
    }
    let done = sampler.ms();
    tokio::time::sleep(Duration::from_secs(10)).await;
    let mut mb: Mb = vec![];
    let to_mb = |b: u64| b as f64 / 1048576.0;
    mb.push(("harness".into(), to_mb(sampler.at(400))));
    mb.push(("peak".into(), to_mb(sampler.peak(done + 10_000))));
    mb.push(("warm".into(), to_mb(sampler.at(sampler.ms()))));
    for secs in &args.idle {
        let wait = (done + secs * 1000).saturating_sub(sampler.ms());
        tokio::time::sleep(Duration::from_millis(wait)).await;
        mb.push((format!("idle_{secs}"), to_mb(sampler.at(sampler.ms()))));
    }
    // The first agent's broker is the shared one, or that worktree's own in the baseline.
    let typescript = agents[0].broker.status().await[0]["typescript"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    for a in &agents {
        a.broker.shutdown().await;
    }
    Ok((mb, marks, typescript))
}
