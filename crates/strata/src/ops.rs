//! Worktree lifecycle. Every operation holds the daemon's git lock for its whole run, so
//! Strata's own git calls never overlap; the store lock is only held for single statements.

use crate::api::Daemon;
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use strata_core::config::{Config, is_dns_label, to_dns_label};
use strata_core::store::{AgentState, Repo};
use strata_core::{Error, Result, detect};
use strata_git as git;

const DETACHED: &str = "(detached)";

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
            return Ok((path, moved));
        }
        git::forget(root, &path)?;
    }
    fs::create_dir_all(cfg.worktree_dir(root))?;
    git::add_detached(root, &path, &commit)?;
    Ok((path, true))
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
    let _git = d.git.lock().unwrap();
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
    if let Err(e) = git::add(root, &path, &branch, from.as_deref()) {
        d.store().delete_agent(agent.id)?;
        return Err(e);
    }
    let warnings: Vec<String> = git::uninitialized_submodules(&path)?
        .into_iter()
        .map(|s| format!("submodule {s} is not initialized; run `git submodule update --init {s}` in the worktree"))
        .collect();
    d.store().set_agent_state(agent.id, AgentState::Ready)?;
    d.store().event(Some(agent.id), "agent.created", &name)?;
    Ok(json!({ "agent": d.store().agent(repo.id, &name)?, "warnings": warnings }))
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

pub fn remove(d: &Daemon, req: Remove) -> Result<Value> {
    let _git = d.git.lock().unwrap();
    let (repo, cfg) = repo_for(d, &req.repo)?;
    let root = &repo.root;
    let agent = d.store().agent(repo.id, &req.name)?.ok_or_else(|| {
        Error::NotFound(format!(
            "no agent named {:?}; see `strata status`",
            req.name
        ))
    })?;
    let mut notes = vec![];
    if agent.path.exists() {
        let changed = git::changes(&agent.path)?;
        if changed > 0 && !req.force {
            return Err(Error::Refused(format!(
                "{} has uncommitted changes in {changed} file(s); commit or stash them, or pass --force to discard them",
                agent.name
            )));
        }
        d.store().set_agent_state(agent.id, AgentState::Removing)?;
        if let Err(e) = git::remove(root, &agent.path, req.force) {
            d.store().set_agent_state(agent.id, AgentState::Ready)?;
            return Err(e);
        }
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

/// Every 2 s: move the base worktree when the base branch moves, mark agents whose folder was
/// deleted by hand as orphaned (unlock + prune), and pick up branch switches inside worktrees.
// ponytail: polling git; Phase 2's file watcher can trigger this instead.
pub async fn maintain(d: Arc<Daemon>) {
    let mut every = tokio::time::interval(Duration::from_secs(2));
    loop {
        every.tick().await;
        let d = d.clone();
        if let Ok(Err(e)) = tokio::task::spawn_blocking(move || tick(&d)).await {
            tracing::warn!("maintenance: {e}");
        }
    }
}

fn tick(d: &Daemon) -> Result<()> {
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
                continue;
            }
            let branch = git::current_branch(&agent.path)?.unwrap_or_else(|| DETACHED.into());
            if branch != agent.branch {
                d.store().set_agent_branch(agent.id, &branch)?;
            }
        }
    }
    Ok(())
}
