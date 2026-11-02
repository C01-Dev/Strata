//! PLAN 4.5 warm pool: worktrees seeded ahead of time on the base commit, detached, so
//! `strata new` only moves one into place and checks out its branch. An entry's lock reason is
//! its state: `strata pool: seeding` while it is made, `strata pool: ready <dep_key> <tier>
//! <donor>` after. An entry still seeding when the daemon starts is a crash's leftover.

use crate::api::Daemon;
use crate::ops;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use strata_core::Result;
use strata_core::config::Config;
use strata_core::store::Repo;
use strata_git as git;

const SEEDING: &str = "strata pool: seeding";
const READY: &str = "strata pool: ready ";

pub struct Entry {
    pub path: PathBuf,
    pub dep_key: String,
    pub tier: String,
    pub donor: Option<PathBuf>,
}

fn dir(root: &Path, cfg: &Config) -> PathBuf {
    cfg.worktree_dir(root).join(".pool")
}

/// Pool worktrees with their lock reason.
fn listed(root: &Path, cfg: &Config) -> Result<Vec<(PathBuf, String)>> {
    let dir = dir(root, cfg);
    Ok(git::list(root)?
        .into_iter()
        .filter(|w| w.path.parent().is_some_and(|p| git::same_path(p, &dir)))
        .filter_map(|w| Some((w.path, w.locked.filter(|r| r.starts_with("strata pool:"))?)))
        .collect())
}

fn ready(path: PathBuf, reason: &str) -> Option<Entry> {
    let mut parts = reason.strip_prefix(READY)?.splitn(3, ' ');
    Some(Entry {
        path,
        dep_key: parts.next()?.into(),
        tier: parts.next()?.into(),
        donor: parts.next().filter(|d| !d.is_empty()).map(PathBuf::from),
    })
}

/// Repos whose pool is being filled right now.
static FILLING: Mutex<BTreeSet<i64>> = Mutex::new(BTreeSet::new());

/// Tops the pool up to `seed.warm_pool` entries, one at a time, off the critical path. Off for
/// repos with submodules (PLAN), for repos without a `package.json` (nothing to seed) and for
/// pnpm's global virtual store.
pub fn fill(d: &Daemon, repo: &Repo) -> Result<()> {
    if !FILLING.lock().unwrap().insert(repo.id) {
        return Ok(());
    }
    let result = fill_now(d, repo);
    FILLING.lock().unwrap().remove(&repo.id);
    result
}

fn fill_now(d: &Daemon, repo: &Repo) -> Result<()> {
    let root = &repo.root;
    loop {
        let cfg = Config::load(root)?;
        let base = ops::base_path(root, &cfg);
        // A shared store's relative links break when an entry moves a folder level up.
        if !base.join("package.json").exists()
            || root.join(".gitmodules").exists()
            || strata_seed::shared_store(&base)
            || listed(root, &cfg)?.len() >= cfg.seed.warm_pool as usize
        {
            return Ok(());
        }
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let path = dir(root, &cfg).join(millis.to_string());
        {
            let _git = d.git.lock().unwrap();
            fs::create_dir_all(dir(root, &cfg))?;
            git::add_detached_locked(root, &path, &git::resolve(&base, "HEAD")?, SEEDING)?;
        }
        let s = ops::seed_entry(d, repo, &cfg, &path);
        let _git = d.git.lock().unwrap();
        match (&s.dep_key, s.tier) {
            (Some(key), "clone" | "copy" | "hardlink" | "install") => {
                let donor = s.donor.map(|p| p.display().to_string()).unwrap_or_default();
                git::relock(root, &path, &format!("{READY}{key} {} {donor}", s.tier))?;
                tracing::info!(path = %path.display(), "warm pool entry ready ({})", s.tier);
            }
            _ => {
                tracing::warn!(path = %path.display(), "warm pool entry not seeded ({}); removed", s.tier);
                return git::remove(root, &path, true);
            }
        }
    }
}

/// For `strata new` (git lock held): a ready entry whose dependency files are the same at
/// `rev` (so its dep_key will match), unlocked, moved to `path`, locked, and switched to
/// `branch` (new from `from`, or existing). `None` when no entry fits.
pub fn take(
    root: &Path,
    cfg: &Config,
    path: &Path,
    branch: &str,
    from: Option<&str>,
) -> Result<Option<Entry>> {
    let rev = from.unwrap_or(branch);
    let fits = |e: &Entry| {
        git::resolve(&e.path, "HEAD")
            .and_then(|head| git::changed_between(root, &head, rev))
            .is_ok_and(|changed| !changed.iter().any(|f| strata_overlay::is_dep_input(f)))
    };
    let Some(entry) = listed(root, cfg)?
        .into_iter()
        .filter_map(|(p, reason)| ready(p, &reason))
        .find(fits)
    else {
        return Ok(None);
    };
    git::unlock(root, &entry.path)?;
    git::move_worktree(root, &entry.path, path)?;
    git::lock(root, path)?;
    git::switch(path, branch, from)?;
    Ok(Some(entry)) // `entry.path` is where it was, for the fix-ups
}

/// At daemon start: entries left mid-seeding by a crash, and records of deleted entries.
pub fn recover(root: &Path, cfg: &Config) -> Result<()> {
    for (path, reason) in listed(root, cfg)? {
        if !path.exists() {
            git::forget(root, &path)?;
        } else if reason == SEEDING {
            tracing::warn!(path = %path.display(), "warm pool entry left mid-seeding; removed");
            git::remove(root, &path, true)?;
        }
    }
    Ok(())
}

/// Entries in the pool folder (ready or seeding), for status: no git call.
pub fn count(root: &Path) -> usize {
    Config::load(root).map_or(0, |cfg| {
        fs::read_dir(dir(root, &cfg)).map_or(0, |d| d.count())
    })
}
