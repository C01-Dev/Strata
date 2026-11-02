//! Worktree lifecycle. Every operation holds the daemon's git lock for its whole run, so
//! Strata's own git calls never overlap; the store lock is only held for single statements.

use crate::api::Daemon;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use strata_checker::broker::{Op, Request, Settings};
use strata_core::config::{Config, is_dns_label, to_dns_label};
use strata_core::store::{Agent, AgentState, Repo};
use strata_core::{Error, Result, detect};
use strata_git as git;

const DETACHED: &str = "(detached)";

/// Test hook for task 1.7, debug builds only: with `STRATA_FAULT=<point>` the daemon stalls
/// at that point so a test can SIGKILL it mid-operation.
fn fault(point: &str) {
    if cfg!(debug_assertions) && std::env::var("STRATA_FAULT").is_ok_and(|p| p == point) {
        tracing::warn!("fault point {point}: stalling");
        std::thread::sleep(Duration::from_secs(600));
    }
}

fn base_path(root: &Path, cfg: &Config) -> PathBuf {
    cfg.worktree_dir(root).join(".base")
}

/// `name-2`, `name-3`, ... : the first one `taken` says is free.
fn suggest(name: &str, taken: impl Fn(&str) -> bool) -> String {
    (2..)
        .map(|n| format!("{name}-{n}"))
        .find(|c| !taken(c))
        .expect("unbounded")
}

fn repo_for(d: &Daemon, root: &Path) -> Result<(Repo, Config)> {
    let root = root.canonicalize()?;
    let repo = d.store().repo_by_root(&root)?.ok_or_else(|| {
        Error::NotFound(format!(
            "{} is not registered with Strata; run `strata init` there",
            root.display()
        ))
    })?;
    Ok((repo, Config::load(&root)?))
}

/// Creates Strata's base worktree (detached at the base branch) or moves it to the branch's
/// current commit. Returns the path and whether it moved.
fn ensure_base(root: &Path, cfg: &Config) -> Result<(PathBuf, bool)> {
    let path = base_path(root, cfg);
    let commit = git::resolve(root, &cfg.repo.base).map_err(|_| {
        Error::Refused(format!(
            "base branch {:?} not found; set repo.base in strata.toml",
            cfg.repo.base
        ))
    })?;
    if git::list(root)?
        .iter()
        .any(|w| git::same_path(&w.path, &path))
    {
        if path.exists() {
            let moved = git::resolve(&path, "HEAD")? != commit;
            if moved {
                git::checkout_detached(&path, &commit)?;
            }
            link_dependencies(root, &path, &cfg.seed.paths)?;
            return Ok((path, moved));
        }
        git::forget(root, &path)?;
    }
    fs::create_dir_all(cfg.worktree_dir(root))?;
    git::add_detached(root, &path, &commit)?;
    link_dependencies(root, &path, &cfg.seed.paths)?;
    Ok((path, true))
}

/// Phase 2 stopgap until Phase 4 seeds `.base` by clone: symlink each `seed.paths` folder
/// (`node_modules`, `apps/*/node_modules`, ...) that exists in the main checkout into the base
/// worktree, so the shared checker can resolve packages. Agents share the base's resolution.
fn link_dependencies(root: &Path, base: &Path, patterns: &[String]) -> Result<()> {
    for pattern in patterns {
        let mut matches = vec![PathBuf::new()];
        for part in pattern.split('/') {
            let mut next = vec![];
            for m in &matches {
                if part == "*" {
                    for entry in fs::read_dir(root.join(m)).into_iter().flatten().flatten() {
                        if entry.file_type().is_ok_and(|t| t.is_dir()) {
                            next.push(m.join(entry.file_name()));
                        }
                    }
                } else {
                    next.push(m.join(part));
                }
            }
            matches = next;
        }
        for rel in matches {
            let (from, to) = (root.join(&rel), base.join(&rel));
            if from.is_dir()
                && !to.exists()
                && to.symlink_metadata().is_err()
                && to.parent().is_some_and(Path::exists)
            {
                std::os::unix::fs::symlink(&from, &to)?;
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct RegisterRepo {
    pub root: PathBuf,
}

pub fn register_repo(d: &Daemon, req: RegisterRepo) -> Result<Value> {
    let _git = d.git.lock().unwrap();
    git::check_version()?;
    let root = req.root.canonicalize()?;
    if git::repo_root(&root)? != root {
        return Err(Error::Refused(format!(
            "{} is not a repo's main checkout",
            root.display()
        )));
    }
    let cfg = Config::load(&root)?;
    let ts = detect::detect(&root).typescript;
    let repo = d
        .store()
        .upsert_repo(&root, &cfg.repo.base, ts.as_deref())?;
    let (base, _) = ensure_base(&root, &cfg)?;
    d.store()
        .event(None, "repo.registered", &root.to_string_lossy())?;
    Ok(json!({ "repo": repo, "base_worktree": base }))
}

#[derive(Deserialize)]
pub struct NewAgent {
    pub repo: PathBuf,
    pub name: String,
    pub from: Option<String>,
    pub branch: Option<String>,
}

pub fn new_agent(d: &Daemon, req: NewAgent) -> Result<Value> {
    let git_lock = d.git.lock().unwrap();
    let (repo, cfg) = repo_for(d, &req.repo)?;
    let (root, name) = (&repo.root, req.name);
    if !is_dns_label(&name) {
        return Err(Error::Refused(format!(
            "agent name {name:?} must be lowercase letters, digits and hyphens, at most 63 characters (it becomes a hostname)"
        )));
    }
    let dir = cfg.worktree_dir(root);
    let taken =
        |n: &str| d.store().agent(repo.id, n).ok().flatten().is_some() || dir.join(n).exists();
    let path = dir.join(&name);
    if d.store().agent(repo.id, &name)?.is_some() {
        return Err(Error::Refused(format!(
            "an agent named {name:?} already exists; try {:?}",
            suggest(&name, taken)
        )));
    }
    if path.exists() {
        return Err(Error::Refused(format!(
            "{} already exists; try the name {:?}",
            path.display(),
            suggest(&name, taken)
        )));
    }
    let branch = req.branch.unwrap_or_else(|| name.clone());
    let from = if git::branch_exists(root, &branch) {
        if let Some(w) = git::list(root)?
            .iter()
            .find(|w| w.branch.as_deref() == Some(branch.as_str()))
        {
            let free = suggest(&branch, |b| git::branch_exists(root, b));
            return Err(Error::Refused(format!(
                "branch {branch:?} is already checked out in {}; pass --branch with a new name, e.g. {free:?}",
                w.path.display()
            )));
        }
        if req.from.is_some() {
            return Err(Error::Refused(format!(
                "branch {branch:?} already exists, so --from does not apply; drop --from to use it, or pass a new --branch"
            )));
        }
        None
    } else {
        Some(req.from.unwrap_or_else(|| cfg.repo.base.clone()))
    };
    fs::create_dir_all(&dir)?;
    // The row goes in first and reserves the name; `creating` tells crash recovery to check git.
    let agent = d
        .store()
        .insert_agent(repo.id, &name, &path, &branch, AgentState::Creating)?;
    fault("new:before_git");
    if let Err(e) = git::add(root, &path, &branch, from.as_deref()) {
        d.store().delete_agent(agent.id)?;
        return Err(e);
    }
    fault("new:after_git");
    let warnings: Vec<String> = git::uninitialized_submodules(&path)?
        .into_iter()
        .map(|s| format!("submodule {s} is not initialized; run `git submodule update --init {s}` in the worktree"))
        .collect();
    // Installing can take minutes; other agents' git operations need not wait for it. The agent
    // stays `creating` until it is done, so `rm` and the maintenance pass leave it alone.
    drop(git_lock);
    let (tier, failed) = install_dependencies(&path, &name);
    let mut warnings = warnings;
    warnings.extend(failed);
    d.store().set_agent_seed_tier(agent.id, tier)?;
    d.store().set_agent_state(agent.id, AgentState::Ready)?;
    d.store().event(
        Some(agent.id),
        "agent.created",
        &format!("{name} (dependencies: {tier})"),
    )?;
    Ok(json!({ "agent": d.store().agent(repo.id, &name)?, "warnings": warnings }))
}

/// `$STRATA_HOME/logs/<agent>/`.
pub fn log_dir(agent: &str) -> PathBuf {
    strata_core::strata_home().join("logs").join(agent)
}

/// PLAN 3.0, the plain-install tier (Phase 4 puts cloning in front of it): the worktree's
/// package manager installs into it. Returns the tier and, if the install failed, a warning;
/// the worktree is usable either way.
fn install_dependencies(path: &Path, agent: &str) -> (&'static str, Option<String>) {
    let Some(pm) = detect::detect(path)
        .package_manager
        .filter(|_| path.join("package.json").exists())
    else {
        return ("none", None);
    };
    let args = pm.install(path);
    let log = log_dir(agent).join("install.log");
    let run = || -> std::io::Result<std::process::ExitStatus> {
        fs::create_dir_all(log_dir(agent))?;
        let out = fs::File::create(&log)?;
        std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(path)
            .stdin(std::process::Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .status()
    };
    match run() {
        Ok(status) if status.success() => ("install", None),
        Ok(status) => (
            "install-failed",
            Some(format!(
                "`{}` failed ({status}); see {}",
                args.join(" "),
                log.display()
            )),
        ),
        Err(e) => (
            "install-failed",
            Some(format!("could not run `{}`: {e}", args.join(" "))),
        ),
    }
}

#[derive(Deserialize)]
pub struct Adopt {
    pub path: PathBuf,
}

pub fn adopt(d: &Daemon, req: Adopt) -> Result<Value> {
    let _git = d.git.lock().unwrap();
    let path = req
        .path
        .canonicalize()
        .map_err(|_| Error::NotFound(format!("{} does not exist", req.path.display())))?;
    let root = git::repo_root(&path)?;
    let (repo, cfg) = repo_for(d, &root)?;
    if path == root {
        return Err(Error::Refused(
            "that is the repo's main checkout; adopt a linked worktree (`git worktree add`)".into(),
        ));
    }
    if git::toplevel(&path)? != path
        || !git::list(&root)?
            .iter()
            .any(|w| git::same_path(&w.path, &path))
    {
        return Err(Error::Refused(format!(
            "{} is not the top folder of a worktree of {}",
            path.display(),
            root.display()
        )));
    }
    if git::same_path(&path, &base_path(&root, &cfg)) {
        return Err(Error::Refused("that is Strata's own base worktree".into()));
    }
    if let Some(a) = d.store().agent_by_path(&path)? {
        return Err(Error::Refused(format!(
            "{} is already adopted as {:?}",
            path.display(),
            a.name
        )));
    }
    let label = to_dns_label(&path.file_name().unwrap_or_default().to_string_lossy());
    let taken = |n: &str| d.store().agent(repo.id, n).ok().flatten().is_some();
    let name = if taken(&label) {
        suggest(&label, taken)
    } else {
        label
    };
    let branch = git::current_branch(&path)?.unwrap_or_else(|| DETACHED.into());
    git::lock(&root, &path)?;
    let agent = d
        .store()
        .insert_agent(repo.id, &name, &path, &branch, AgentState::Ready)?;
    d.store()
        .event(Some(agent.id), "agent.adopted", &path.to_string_lossy())?;
    Ok(json!({ "agent": agent }))
}

#[derive(Deserialize)]
pub struct Remove {
    pub repo: PathBuf,
    #[serde(skip)]
    pub name: String,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub keep_branch: bool,
}

/// Everything `remove` refuses for, checked without changing anything, so the caller can stop
/// the agent's processes first.
pub fn removable(d: &Daemon, req: &Remove) -> Result<Agent> {
    let (repo, _) = repo_for(d, &req.repo)?;
    let agent = d.store().agent(repo.id, &req.name)?.ok_or_else(|| {
        Error::NotFound(format!(
            "no agent named {:?}; see `strata status`",
            req.name
        ))
    })?;
    if agent.state == AgentState::Creating {
        return Err(Error::Refused(format!(
            "{} is still being created; try again when `strata new` returns",
            agent.name
        )));
    }
    if agent.path.exists() && !req.force {
        let changed = git::changes(&agent.path)?;
        if changed > 0 {
            return Err(Error::Refused(format!(
                "{} has uncommitted changes in {changed} file(s); commit or stash them, or pass --force to discard them",
                agent.name
            )));
        }
    }
    Ok(agent)
}

pub fn remove(d: &Daemon, req: Remove) -> Result<Value> {
    let _git = d.git.lock().unwrap();
    let (repo, cfg) = repo_for(d, &req.repo)?;
    let root = &repo.root;
    let agent = removable(d, &req)?;
    let mut notes = vec![];
    if agent.path.exists() {
        d.store().set_agent_state(agent.id, AgentState::Removing)?;
        fault("rm:before_git");
        if let Err(e) = git::remove(root, &agent.path, req.force) {
            d.store().set_agent_state(agent.id, AgentState::Ready)?;
            return Err(e);
        }
        fault("rm:after_git");
    } else {
        git::forget(root, &agent.path)?;
        notes.push("its folder was already gone".to_string());
    }
    let ours = agent.branch != DETACHED && agent.branch != cfg.repo.base;
    if !req.keep_branch
        && ours
        && git::branch_exists(root, &agent.branch)
        && git::delete_merged_branch(root, &agent.branch).is_err()
    {
        notes.push(format!(
            "kept branch {:?}: it is not merged (delete it with `git branch -D {}` if you are sure)",
            agent.branch, agent.branch
        ));
    }
    d.store().delete_agent(agent.id)?;
    d.store()
        .event(Some(agent.id), "agent.removed", &agent.name)?;
    Ok(json!({ "removed": agent.name, "notes": notes }))
}

/// At startup, before serving: settle operations a crash interrupted, and orphan ready agents
/// whose worktree git no longer knows. An interrupted `new` is undone (its caller got an error and
/// never saw the path); an interrupted `rm` is finished if git already removed the worktree,
/// otherwise the agent is ready again so the caller can retry.
pub fn recover(d: &Daemon) -> Result<()> {
    let _git = d.git.lock().unwrap();
    let repos = d.store().repos()?;
    for repo in repos {
        let listed = git::list(&repo.root)?;
        let agents = d.store().agents(repo.id)?;
        for a in agents {
            let present =
                a.path.exists() && listed.iter().any(|w| git::same_path(&w.path, &a.path));
            let detail = match (a.state, present) {
                (AgentState::Creating, true) => {
                    git::remove(&repo.root, &a.path, true)?;
                    d.store().delete_agent(a.id)?;
                    "interrupted create undone"
                }
                (AgentState::Creating, false) => {
                    git::forget(&repo.root, &a.path)?;
                    d.store().delete_agent(a.id)?;
                    if a.path.exists() {
                        tracing::warn!(path = %a.path.display(), "left a partial folder in place");
                    }
                    "interrupted create undone"
                }
                (AgentState::Removing, true) => {
                    git::lock(&repo.root, &a.path)?;
                    d.store().set_agent_state(a.id, AgentState::Ready)?;
                    "interrupted remove undone; run `strata rm` again"
                }
                (AgentState::Removing, false) => {
                    git::forget(&repo.root, &a.path)?;
                    d.store().delete_agent(a.id)?;
                    "interrupted remove finished"
                }
                (AgentState::Ready, false) => {
                    git::forget(&repo.root, &a.path)?;
                    d.store().set_agent_state(a.id, AgentState::Orphaned)?;
                    "worktree gone while the daemon was down; orphaned"
                }
                _ => continue,
            };
            tracing::warn!(agent = a.name, "{detail}");
            d.store().event(
                Some(a.id),
                "agent.recovered",
                &format!("{}: {detail}", a.name),
            )?;
        }
    }
    Ok(())
}

/// Every 2 s: move the base worktree when the base branch moves, mark agents whose folder was
/// deleted by hand as orphaned (unlock + prune), and pick up branch switches inside worktrees.
// ponytail: polling git; Phase 2's file watcher can trigger this instead.
pub async fn maintain(d: Arc<Daemon>) {
    let mut every = tokio::time::interval(Duration::from_secs(2));
    let mut n = 0u64;
    loop {
        every.tick().await;
        n += 1;
        let dd = d.clone();
        match tokio::task::spawn_blocking(move || tick(&dd)).await {
            Ok(Ok(orphaned)) => {
                for (repo, agent) in orphaned {
                    crate::procs::stop_agent(&d, &agent).await;
                    d.checker.dispose_agent(&repo, &agent.name).await;
                }
            }
            Ok(Err(e)) => tracing::warn!("maintenance: {e}"),
            Err(e) => tracing::warn!("maintenance: {e}"),
        }
        crate::procs::meter(&d).await;
        if n.is_multiple_of(15) {
            d.checker.maintain().await;
        }
    }
}

/// Returns agents it marked orphaned, with their repo root, so their processes and snapshots can go.
fn tick(d: &Daemon) -> Result<Vec<(PathBuf, Agent)>> {
    crate::procs::refresh(d);
    let mut orphaned = vec![];
    let _git = d.git.lock().unwrap();
    // Bind first: a `for` keeps its iterator expression's temporaries (the store guard) alive
    // for the whole loop, and the body takes the store lock again.
    let repos = d.store().repos()?;
    for repo in repos {
        let cfg = match Config::load(&repo.root) {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::warn!("{e}");
                continue;
            }
        };
        match ensure_base(&repo.root, &cfg) {
            Ok((path, true)) => {
                tracing::info!(base = %path.display(), "base worktree moved to {}", cfg.repo.base);
                d.store().event(None, "base.advanced", &cfg.repo.base)?;
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("base worktree for {}: {e}", repo.root.display()),
        }
        let agents = d.store().agents(repo.id)?;
        for agent in agents {
            if agent.state != AgentState::Ready {
                continue;
            }
            if !agent.path.exists() {
                git::forget(&repo.root, &agent.path)?;
                d.store().set_agent_state(agent.id, AgentState::Orphaned)?;
                d.store()
                    .event(Some(agent.id), "agent.orphaned", &agent.name)?;
                tracing::warn!(
                    agent = agent.name,
                    "worktree folder deleted by hand; marked orphaned"
                );
                orphaned.push((repo.root.clone(), agent));
                continue;
            }
            let branch = git::current_branch(&agent.path)?.unwrap_or_else(|| DETACHED.into());
            if branch != agent.branch {
                d.store().set_agent_branch(agent.id, &branch)?;
            }
        }
    }
    Ok(orphaned)
}

#[derive(Deserialize)]
pub struct Check {
    pub repo: PathBuf,
    /// diagnostics | hover | definition | references | symbols
    pub op: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub limit: Option<usize>,
}

/// A checker query for an agent, answered from the shared checker (see strata-checker).
/// The repo, a ready agent and the repo's config, for requests that act inside a worktree.
pub fn ready_agent(d: &Daemon, repo: &Path, name: &str) -> Result<(Repo, Agent, Config)> {
    let (repo, cfg) = repo_for(d, repo)?;
    let agent = d
        .store()
        .agent(repo.id, name)?
        .ok_or_else(|| Error::NotFound(format!("no agent named {name:?}; see `strata status`")))?;
    if agent.state != AgentState::Ready {
        return Err(Error::Refused(format!(
            "agent {name:?} is {}, not ready",
            agent.state.as_str()
        )));
    }
    Ok((repo, agent, cfg))
}

pub async fn check(d: &Daemon, name: String, req: Check) -> Result<Value> {
    let (repo, agent, cfg) = ready_agent(d, &req.repo, &name)?;
    let need = |what: &str| Error::Refused(format!("{} needs {what}", req.op));
    let file = || req.file.clone().ok_or_else(|| need("file"));
    let at = || -> Result<(String, u32, u32)> {
        Ok((
            file()?,
            req.line.ok_or_else(|| need("line"))?,
            req.column.ok_or_else(|| need("column"))?,
        ))
    };
    let op = match req.op.as_str() {
        "diagnostics" => Op::Diagnostics {
            file: req.file.clone(),
            limit: req.limit,
        },
        "hover" => at().map(|(file, line, column)| Op::Hover { file, line, column })?,
        "definition" => at().map(|(file, line, column)| Op::Definition { file, line, column })?,
        "references" => at().map(|(file, line, column)| Op::References { file, line, column })?,
        "symbols" => Op::Symbols { file: file()? },
        other => return Err(Error::Refused(format!("unknown check {other:?}"))),
    };
    let request = Request {
        base: base_path(&repo.root, &cfg),
        repo: repo.root,
        agent: name,
        worktree: agent.path,
        op,
    };
    d.checker.check(request, &Settings::from_config(&cfg)).await
}
