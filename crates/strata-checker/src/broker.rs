//! The checker broker. One group per repo: a bridge (one tsgo process) holding the base snapshot.
//! Each agent gets a snapshot whose layer is its worktree's diff against base, at base paths:
//!
//! - shared: layered on the base (the normal case);
//! - unshared, same process: dependencies differ (`dep_key`) or the diff is huge
//!   (`huge_diff_ratio`); a snapshot at the worktree's own paths;
//! - unshared, own process: the worktree has a different TypeScript version.
//!
//! Before every query the agent's worktree is rescanned (cheap, see strata-overlay) and the
//! snapshot updated if its layer changed; the base is rescanned at most every `base_rescan`.
//! A dead bridge is restarted on the next query. Lock order: agent, then group.

use crate::bridge::{Bridge, install_script, node};
use crate::translate::Translator;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use strata_core::config::Config;
use strata_core::{Error, Result, detect};
use strata_overlay::{Tree, layer};
use tokio::sync::Mutex;

const GROUP: &str = "base";
const OPEN_TIMEOUT: Duration = Duration::from_secs(300);
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);
const PROJECT_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Relative to the checkout root.
    pub tsconfigs: Vec<PathBuf>,
    pub extra_paths: Vec<String>,
    pub idle_dispose: Duration,
    pub huge_diff_ratio: f64,
    pub group_memory_bytes: u64,
    /// How often the base checkout is rescanned (the daemon moves it at most every 2 s).
    pub base_rescan: Duration,
}

impl Settings {
    pub fn from_config(c: &Config) -> Self {
        Self {
            tsconfigs: c.checker.tsconfig.clone(),
            extra_paths: c.checker.extra_paths.clone(),
            idle_dispose: Duration::from_secs(c.checker.idle_dispose_secs),
            huge_diff_ratio: c.checker.huge_diff_ratio,
            group_memory_bytes: c.checker.group_memory_mb * 1024 * 1024,
            base_rescan: Duration::from_secs(2),
        }
    }

    fn tsconfigs_at(&self, root: &Path) -> Vec<PathBuf> {
        self.tsconfigs.iter().map(|t| root.join(t)).collect()
    }
}

/// Lines and columns are 1-based, columns in UTF-16 code units; `file` is relative to the
/// worktree or absolute.
#[derive(Debug, Clone)]
pub enum Op {
    Diagnostics {
        file: Option<String>,
        limit: Option<usize>,
    },
    Hover {
        file: String,
        line: u32,
        column: u32,
    },
    Definition {
        file: String,
        line: u32,
        column: u32,
    },
    References {
        file: String,
        line: u32,
        column: u32,
    },
    Symbols {
        file: String,
    },
}

#[derive(Debug, Clone)]
pub struct Request {
    pub repo: PathBuf,
    /// The base checkout (Strata's `.base` worktree).
    pub base: PathBuf,
    pub agent: String,
    pub worktree: PathBuf,
    pub op: Op,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum Sharing {
    Shared,
    Unshared { reason: String },
}

/// What the bridge currently holds for an agent.
#[derive(Debug, Clone, PartialEq)]
struct Held {
    bridge_pid: u32,
    base_version: u64,
    layer_hash: u64,
    sharing: Sharing,
}

struct Agent {
    worktree: PathBuf,
    tree: Tree,
    held: Option<Held>,
    /// A separate bridge when the worktree's TypeScript differs from base.
    own: Option<Arc<Bridge>>,
    sharing: Sharing,
    changed: usize,
    removed: usize,
    last_used: Instant,
}

struct GroupState {
    bridge: Option<Arc<Bridge>>,
    base: Tree,
    dep_key: String,
    typescript: String,
    program_files: u64,
    version: u64,
    scanned: Option<Instant>,
    restarts: u32,
}

struct Group {
    repo: PathBuf,
    base_root: PathBuf,
    settings: Settings,
    state: Mutex<GroupState>,
    /// Name -> (worktree, agent). The worktree sits outside the agent's lock so lookups never
    /// wait for a running query.
    agents: Mutex<HashMap<String, (PathBuf, SharedAgent)>>,
}

type SharedAgent = Arc<Mutex<Agent>>;

pub struct Broker {
    home: PathBuf,
    runtime: tokio::sync::OnceCell<String>,
    groups: Mutex<HashMap<PathBuf, Arc<Group>>>,
}

async fn scan(mut tree: Tree) -> Result<(Tree, bool)> {
    tokio::task::spawn_blocking(move || {
        let changed = tree.scan()?.changed;
        Ok((tree, changed))
    })
    .await
    .map_err(|e| Error::Checker(e.to_string()))?
}

fn typescript_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join("node_modules/typescript/package.json")).ok()?;
    serde_json::from_str::<Value>(&text).ok()?["version"]
        .as_str()
        .map(String::from)
}

/// Bridge locations are 0-based `line`/`character`; agents get 1-based `line`/`column`.
fn one_based(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if map.contains_key("character") {
                let renamed: Map<String, Value> = std::mem::take(map)
                    .into_iter()
                    .map(|(k, v)| match k.as_str() {
                        "line" | "endLine" => (k, json!(v.as_u64().unwrap_or(0) + 1)),
                        "character" => ("column".into(), json!(v.as_u64().unwrap_or(0) + 1)),
                        "endCharacter" => ("endColumn".into(), json!(v.as_u64().unwrap_or(0) + 1)),
                        _ => (k, v),
                    })
                    .collect();
                *map = renamed;
            }
            map.values_mut().for_each(one_based);
        }
        Value::Array(items) => items.iter_mut().for_each(one_based),
        _ => {}
    }
}

impl Broker {
    /// `home` is `$STRATA_HOME` (the bridge script is installed there).
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
            runtime: Default::default(),
            groups: Default::default(),
        }
    }

    /// Node ABI and platform, part of every `dep_key`.
    async fn runtime(&self) -> Result<&str> {
        self.runtime
            .get_or_try_init(|| async {
                let out = tokio::process::Command::new(node())
                    .args([
                        "-p",
                        "process.versions.modules + ' ' + process.platform + '-' + process.arch",
                    ])
                    .output()
                    .await
                    .map_err(|e| {
                        Error::Checker(format!(
                            "cannot run node ({e}); the shared checker needs Node.js"
                        ))
                    })?;
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            })
            .await
            .map(String::as_str)
    }

    async fn group(&self, repo: &Path, base: &Path, settings: &Settings) -> Result<Arc<Group>> {
        let mut groups = self.groups.lock().await;
        if let Some(g) = groups.get(repo)
            && g.base_root == base
            && g.settings == *settings
        {
            return Ok(g.clone());
        }
        if let Some(old) = groups.remove(repo) {
            old.shutdown().await;
        }
        if let Some(reason) = detect::detect(base).checker_off_reason() {
            return Err(Error::Refused(reason));
        }
        let g = Arc::new(Group {
            repo: repo.to_path_buf(),
            base_root: base.to_path_buf(),
            settings: settings.clone(),
            state: Mutex::new(GroupState {
                bridge: None,
                base: Tree::new(base, &settings.extra_paths),
                dep_key: String::new(),
                typescript: String::new(),
                program_files: 0,
                version: 0,
                scanned: None,
                restarts: 0,
            }),
            agents: Default::default(),
        });
        groups.insert(repo.to_path_buf(), g.clone());
        Ok(g)
    }

    pub async fn check(&self, req: Request, settings: &Settings) -> Result<Value> {
        let runtime = self.runtime().await?.to_string();
        let group = self.group(&req.repo, &req.base, settings).await?;
        let agent = group.agent(&req.agent, &req.worktree).await;
        let mut a = agent.lock().await;
        a.last_used = Instant::now();
        let tree = std::mem::replace(&mut a.tree, Tree::new(&req.worktree, &settings.extra_paths));
        a.tree = scan(tree).await?.0;
        let (bridge, tr) = group
            .prepare(&req.agent, &mut a, &runtime, &self.home)
            .await?;
        let file = |f: &str| tr.to_base(Path::new(f));
        let (method, params, timeout) = match &req.op {
            Op::Diagnostics {
                file: Some(f),
                limit,
            } => (
                "diagnostics",
                json!({ "file": file(f), "limit": limit }),
                QUERY_TIMEOUT,
            ),
            Op::Diagnostics { file: None, limit } => {
                ("diagnostics", json!({ "limit": limit }), PROJECT_TIMEOUT)
            }
            Op::Hover {
                file: f,
                line,
                column,
            }
            | Op::Definition {
                file: f,
                line,
                column,
            }
            | Op::References {
                file: f,
                line,
                column,
            } => {
                let method = match req.op {
                    Op::Hover { .. } => "hover",
                    Op::Definition { .. } => "definition",
                    _ => "references",
                };
                let at = json!({ "file": file(f), "line": line.saturating_sub(1), "character": column.saturating_sub(1) });
                (method, at, QUERY_TIMEOUT)
            }
            Op::Symbols { file: f } => ("symbols", json!({ "file": file(f) }), QUERY_TIMEOUT),
        };
        let mut params = params;
        params["agent"] = json!(req.agent);
        let mut result = bridge.call(method, params, timeout).await?;
        tr.response(&mut result);
        one_based(&mut result);
        if let Value::Object(map) = &mut result {
            map.insert("sharing".into(), json!(a.sharing));
        }
        Ok(result)
    }

    /// Drops an agent's snapshot (and its own bridge, if any).
    pub async fn dispose_agent(&self, repo: &Path, name: &str) {
        let Some(group) = self.groups.lock().await.get(repo).cloned() else {
            return;
        };
        let Some((_, agent)) = group.agents.lock().await.remove(name) else {
            return;
        };
        let mut a = agent.lock().await;
        group.release(name, &mut a).await;
    }

    /// Disposes snapshots idle longer than `idle_dispose`, and, while a group's process tree is
    /// above `group_memory_bytes`, the least recently used one per call. Busy agents are skipped.
    // ponytail: one eviction per call, since Go returns freed memory to the OS only minutes later.
    pub async fn maintain(&self) {
        let groups: Vec<_> = self.groups.lock().await.values().cloned().collect();
        for g in groups {
            let agents: Vec<_> = g
                .agents
                .lock()
                .await
                .iter()
                .map(|(n, (_, a))| (n.clone(), a.clone()))
                .collect();
            let mut open = vec![];
            for (name, agent) in &agents {
                let Ok(mut a) = agent.try_lock() else {
                    continue;
                };
                if a.held.is_none() {
                    continue;
                }
                if a.last_used.elapsed() >= g.settings.idle_dispose {
                    tracing::info!(agent = name, "disposing idle snapshot");
                    g.release(name, &mut a).await;
                } else {
                    open.push((a.last_used, name.clone(), agent.clone()));
                }
            }
            if g.memory().await > g.settings.group_memory_bytes {
                open.sort_by_key(|(used, ..)| *used);
                if let Some((_, name, agent)) = open.first()
                    && let Ok(mut a) = agent.try_lock()
                {
                    tracing::warn!(
                        agent = name,
                        "checker group above group_memory_mb; disposing least recently used snapshot"
                    );
                    g.release(name, &mut a).await;
                }
            }
        }
    }

    pub async fn status(&self) -> Value {
        let groups: Vec<_> = self.groups.lock().await.values().cloned().collect();
        let mut out = vec![];
        for g in groups {
            let (typescript, files, restarts, pid) = {
                let s = g.state.lock().await;
                let pid = s.bridge.as_ref().filter(|b| b.alive()).map(|b| b.pid);
                (s.typescript.clone(), s.program_files, s.restarts, pid)
            };
            let mut agents = vec![];
            for (name, (_, agent)) in g.agents.lock().await.iter() {
                let Ok(a) = agent.try_lock() else {
                    agents.push(json!({ "name": name, "busy": true }));
                    continue;
                };
                agents.push(json!({
                    "name": name,
                    "sharing": a.sharing,
                    "snapshot": a.held.is_some(),
                    "changed_files": a.changed,
                    "removed_files": a.removed,
                    "idle_secs": a.last_used.elapsed().as_secs(),
                }));
            }
            out.push(json!({
                "repo": g.repo,
                "pid": pid,
                "typescript": typescript,
                "program_files": files,
                "restarts": restarts,
                "memory_bytes": g.memory().await,
                "agents": agents,
            }));
        }
        json!(out)
    }

    pub async fn shutdown(&self) {
        for (_, g) in self.groups.lock().await.drain() {
            g.shutdown().await;
        }
    }
}

impl Group {
    async fn agent(&self, name: &str, worktree: &Path) -> SharedAgent {
        let mut agents = self.agents.lock().await;
        if let Some((wt, a)) = agents.get(name)
            && wt == worktree
        {
            return a.clone();
        }
        let a = Arc::new(Mutex::new(Agent {
            worktree: worktree.to_path_buf(),
            tree: Tree::new(worktree, &self.settings.extra_paths),
            held: None,
            own: None,
            sharing: Sharing::Shared,
            changed: 0,
            removed: 0,
            last_used: Instant::now(),
        }));
        agents.insert(name.to_string(), (worktree.to_path_buf(), a.clone()));
        a
    }

    async fn memory(&self) -> u64 {
        let pids: Vec<u32> = self
            .state
            .lock()
            .await
            .bridge
            .iter()
            .map(|b| b.pid)
            .collect();
        let own: Vec<u32> = {
            let agents = self.agents.lock().await;
            let mut pids = vec![];
            for (_, a) in agents.values() {
                if let Ok(a) = a.try_lock()
                    && let Some(b) = &a.own
                {
                    pids.push(b.pid);
                }
            }
            pids
        };
        pids.into_iter()
            .chain(own)
            .map(strata_proc::tree_memory)
            .sum()
    }

    /// Starts (or restarts) the bridge and opens the base.
    async fn start(&self, s: &mut GroupState, home: &Path, runtime: &str) -> Result<()> {
        if let Some(old) = s.bridge.take() {
            old.shutdown().await;
            s.restarts += 1;
        }
        let bridge = Bridge::spawn(&node(), &install_script(home)?, &self.base_root).await?;
        let tree = std::mem::replace(
            &mut s.base,
            Tree::new(&self.base_root, &self.settings.extra_paths),
        );
        s.base = scan(tree).await?.0;
        s.scanned = Some(Instant::now());
        s.dep_key = s.base.dep_key(runtime)?;
        s.typescript = bridge.typescript.clone();
        let opened = json!({ "group": GROUP, "root": self.base_root, "tsconfigs": self.settings.tsconfigs_at(&self.base_root) });
        s.program_files = bridge.call("openBase", opened, OPEN_TIMEOUT).await?["files"]
            .as_u64()
            .unwrap_or(0);
        s.version += 1;
        s.bridge = Some(Arc::new(bridge));
        tracing::info!(repo = %self.repo.display(), typescript = s.typescript, files = s.program_files, "checker group started");
        Ok(())
    }

    /// Brings the group up to date and the agent's snapshot in line with its worktree.
    /// Returns the bridge to query and the path translator for this agent.
    async fn prepare(
        &self,
        name: &str,
        a: &mut Agent,
        runtime: &str,
        home: &Path,
    ) -> Result<(Arc<Bridge>, Translator)> {
        let mut s = self.state.lock().await;
        if !s.bridge.as_ref().is_some_and(|b| b.alive()) {
            self.start(&mut s, home, runtime).await?;
        } else if s
            .scanned
            .is_none_or(|t| t.elapsed() >= self.settings.base_rescan)
        {
            let tree = std::mem::replace(
                &mut s.base,
                Tree::new(&self.base_root, &self.settings.extra_paths),
            );
            let (tree, changed) = scan(tree).await?;
            s.base = tree;
            s.scanned = Some(Instant::now());
            if changed {
                if s.base.dep_key(runtime)? != s.dep_key {
                    tracing::info!("base dependencies changed; restarting the checker group");
                    self.start(&mut s, home, runtime).await?;
                } else {
                    let bridge = s.bridge.clone().expect("alive");
                    let opened = json!({ "group": GROUP, "root": self.base_root, "tsconfigs": self.settings.tsconfigs_at(&self.base_root) });
                    s.program_files = bridge.call("openBase", opened, OPEN_TIMEOUT).await?["files"]
                        .as_u64()
                        .unwrap_or(0);
                    s.version += 1;
                }
            }
        }
        let bridge = s.bridge.clone().expect("started");
        let diff = layer(&s.base, &a.tree);
        (a.changed, a.removed) = (diff.changed.len(), diff.removed.len());
        let ts = typescript_version(&a.worktree).filter(|v| *v != s.typescript);
        a.sharing = if let Some(v) = &ts {
            Sharing::Unshared {
                reason: format!("TypeScript {v} differs from the base's {}", s.typescript),
            }
        } else if a.tree.dep_key(runtime)? != s.dep_key {
            Sharing::Unshared {
                reason: "dependencies differ".into(),
            }
        } else if diff.changed.len() as f64 > self.settings.huge_diff_ratio * s.base.len() as f64 {
            Sharing::Unshared {
                reason: format!(
                    "large diff ({} of {} files changed)",
                    diff.changed.len(),
                    s.base.len()
                ),
            }
        } else {
            Sharing::Shared
        };
        let target = if ts.is_some() {
            if !a.own.as_ref().is_some_and(|b| b.alive()) {
                if let Some(h) = a.held.take()
                    && h.bridge_pid == bridge.pid
                {
                    let _ = bridge
                        .call("dispose", json!({ "agent": name }), QUERY_TIMEOUT)
                        .await;
                }
                a.own = Some(Arc::new(
                    Bridge::spawn(&node(), &install_script(home)?, &a.worktree).await?,
                ));
            }
            a.own.clone().expect("spawned")
        } else {
            if let Some(own) = a.own.take() {
                own.shutdown().await;
                a.held = None;
            }
            bridge
        };
        let want = Held {
            bridge_pid: target.pid,
            base_version: if a.sharing == Sharing::Shared {
                s.version
            } else {
                0
            },
            layer_hash: diff.hash,
            sharing: a.sharing.clone(),
        };
        if a.held.as_ref() != Some(&want) {
            if a.sharing == Sharing::Shared {
                let files: Map<String, Value> = diff
                    .changed
                    .iter()
                    .map(|rel| {
                        (
                            self.base_root.join(rel).to_string_lossy().into_owned(),
                            json!(a.worktree.join(rel)),
                        )
                    })
                    .collect();
                let removed: Vec<PathBuf> = diff
                    .removed
                    .iter()
                    .map(|rel| self.base_root.join(rel))
                    .collect();
                let params =
                    json!({ "group": GROUP, "agent": name, "files": files, "removed": removed });
                target.call("createSnapshot", params, OPEN_TIMEOUT).await?;
            } else {
                let params = json!({ "agent": name, "root": a.worktree, "tsconfigs": self.settings.tsconfigs_at(&a.worktree) });
                target.call("openUnshared", params, OPEN_TIMEOUT).await?;
            }
            a.held = Some(want);
        }
        let tr = if a.sharing == Sharing::Shared {
            Translator::new(&self.base_root, &a.worktree)
        } else {
            Translator::identity(&a.worktree)
        };
        Ok((target, tr))
    }

    async fn release(&self, name: &str, a: &mut Agent) {
        if let Some(own) = a.own.take() {
            own.shutdown().await;
        } else if a.held.is_some()
            && let Some(bridge) = self.state.lock().await.bridge.clone()
        {
            let _ = bridge
                .call("dispose", json!({ "agent": name }), QUERY_TIMEOUT)
                .await;
        }
        a.held = None;
    }

    async fn shutdown(&self) {
        for (_, a) in self.agents.lock().await.values() {
            if let Some(own) = a.lock().await.own.take() {
                own.shutdown().await;
            }
        }
        if let Some(b) = self.state.lock().await.bridge.take() {
            b.shutdown().await;
        }
    }
}
