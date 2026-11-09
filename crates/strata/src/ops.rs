//! Worktree lifecycle. Every operation holds the daemon's git lock for its whole run, so
//! Strata's own git calls never overlap; the store lock is only held for single statements.

use crate::api::Daemon;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use strata_checker::broker::{Op, Request, Settings};
use strata_core::config::{Config, Fallback, is_dns_label, to_dns_label};
use strata_core::store::{Agent, AgentState, Repo};
use strata_core::{Error, Result, detect};
use strata_git as git;
use strata_seed::Way;

const DETACHED: &str = "(detached)";

/// A note in each agent's worktree, telling the agent to ask Strata instead of starting its own
/// checker or dev server. Claude Code reads `.claude/rules/*.md`, and the repo's shared
/// `info/exclude` keeps the file out of `git status`, so no tracked file changes and `strata rm`
/// still sees a clean tree. Sonnet ran its own `tsc` in 2 of 3 sessions with only Strata's MCP
/// instructions and in none with this note (NOTES, "Do agents use Strata on their own?").
/// `repo.agent_note = false` in strata.toml turns it off.
pub const NOTE: &str = ".claude/rules/strata.md";

pub fn write_note(root: &Path, path: &Path, cfg: &Config) -> Result<()> {
    git::exclude(root, &format!("/{NOTE}"))?;
    let mut text = String::from(
        "# Strata\n\nThis worktree is managed by Strata, which shares one TypeScript checker and a dev server supervisor between every agent working on this repo. If the Strata tools named here are not available to you, ignore this file.\n\n",
    );
    if detect::detect(path).checker_off_reason().is_none() {
        text.push_str(
            "- Check types with the `strata_diagnostics` tool (one file, or the whole project when `file` is omitted) instead of running `tsc`, `tsgo` or the repo's type-check script. It answers from the shared checker in about a second and does not start another checker.\n\
             - `strata_references`, `strata_definition`, `strata_hover` and `strata_symbols` answer questions about the code from the same checker.\n",
        );
        if cfg.checker.conflicts {
            text.push_str(
                "- Before you report back, run `strata_conflicts`. It checks your changes against the other agents' unmerged changes. If it finds clashes, name the other agent and the errors in your summary instead of changing your code to match theirs.\n",
            );
        }
    }
    text.push_str(
        "- Start the dev server with `strata_start_dev_server`, which gives this worktree its own port and URL, instead of running the dev command yourself.\n",
    );
    let file = path.join(NOTE);
    fs::create_dir_all(file.parent().expect("NOTE has a parent"))?;
    fs::write(file, text)?;
    Ok(())
}

/// Test hooks, debug builds only: with `STRATA_FAULT=<point>` the daemon stalls at that point
/// so a test can SIGKILL it mid-operation (task 1.7); `no-clone` makes every clone fail as on a
/// filesystem without clones (task 4.3).
fn faulty(point: &str) -> bool {
    cfg!(debug_assertions) && std::env::var("STRATA_FAULT").is_ok_and(|p| p == point)
}

fn fault(point: &str) {
    if faulty(point) {
        tracing::warn!("fault point {point}: stalling");
        std::thread::sleep(Duration::from_secs(600));
    }
}

pub fn base_path(root: &Path, cfg: &Config) -> PathBuf {
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
    let mut seeded = seed_base(d, &repo)?;
    let dev = |p: &Path| fs::metadata(p).map(|m| std::os::unix::fs::MetadataExt::dev(&m));
    if dev(&root)? != dev(&cfg.worktree_dir(&root))? {
        seeded.warnings.push(format!(
            "{} is on another volume than the repo, so the base worktree cannot clone the main checkout's dependencies and installs them (agents still clone from the base)",
            cfg.worktree_dir(&root).display()
        ));
    }
    Ok(
        json!({ "repo": repo, "base_worktree": base, "seed": seeded.describe(), "warnings": seeded.warnings }),
    )
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
    let made = match crate::pool::take(root, &cfg, &path, &branch, from.as_deref()) {
        Ok(None) => git::add(root, &path, &branch, from.as_deref()).map(|()| None),
        taken => taken,
    };
    let pooled = match made {
        Ok(pooled) => pooled,
        Err(e) => {
            if path.exists() {
                let _ = git::remove(root, &path, true);
            }
            d.store().delete_agent(agent.id)?;
            return Err(e);
        }
    };
    fault("new:after_git");
    let warnings: Vec<String> = git::uninitialized_submodules(&path)?
        .into_iter()
        .map(|s| format!("submodule {s} is not initialized; run `git submodule update --init {s}` in the worktree"))
        .collect();
    // Installing can take minutes; other agents' git operations need not wait for it. The agent
    // stays `creating` until it is done, so `rm` and the maintenance pass leave it alone.
    drop(git_lock);
    let seeded = match pooled {
        Some(entry) => settle_entry(d, &repo, &cfg, &path, &name, entry),
        None => seed(d, &repo, &cfg, &path, &name, None, true),
    };
    let mut warnings = warnings;
    warnings.extend(seeded.warnings.iter().cloned());
    if cfg.repo.agent_note
        && let Err(e) = write_note(root, &path, &cfg)
    {
        warnings.push(format!("could not write {NOTE}: {e}"));
    }
    d.store()
        .set_agent_seed(agent.id, seeded.tier, seeded.dep_key.as_deref())?;
    let port = crate::ports::ensure(d, &agent, &cfg)?;
    d.store().set_agent_state(agent.id, AgentState::Ready)?;
    d.store().event(
        Some(agent.id),
        "agent.created",
        &format!("{name} (dependencies: {})", seeded.describe()),
    )?;
    Ok(
        json!({ "agent": d.store().agent(repo.id, &name)?, "port": port, "seed": seeded.describe(), "warnings": warnings }),
    )
}

/// What seeding did for one checkout.
#[derive(Default)]
pub struct Seeded {
    /// clone, copy, hardlink, clone-failed (disk full), install, install-failed, shared-store
    /// (pnpm's global virtual store: installed, not isolated), or none (no package.json)
    pub tier: &'static str,
    pub dep_key: Option<String>,
    pub donor: Option<PathBuf>,
    /// Native addons rebuilt after seeding (PLAN 4.4).
    pub rebuilt: Vec<String>,
    /// Taken from the warm pool (PLAN 4.5).
    pub warm: bool,
    pub warnings: Vec<String>,
}

impl Seeded {
    pub fn describe(&self) -> String {
        if self.tier == "shared-store" {
            return "shared store, not isolated".into();
        }
        let mut d = match &self.donor {
            Some(from) => format!("{} from {}", self.tier, from.display()),
            None => self.tier.to_string(),
        };
        if !self.rebuilt.is_empty() {
            d += &format!(", rebuilt {}", self.rebuilt.join(", "));
        }
        if self.warm {
            d += " (warm pool)";
        }
        d
    }
}

/// PLAN's dep_key of a checkout, computed exactly as the checker does.
fn dep_key(path: &Path, cfg: &Config) -> Result<String> {
    let mut tree = strata_overlay::Tree::new(path, &cfg.checker.extra_paths);
    tree.scan()?;
    Ok(tree.dep_key(&detect::node_runtime()?)?)
}

/// The registered donor for `key` if it still has that key. A stale or vanished one is dropped:
/// a stale donor measured slower than a fresh install.
fn donor(d: &Daemon, repo: &Repo, cfg: &Config, key: &str) -> Option<PathBuf> {
    let path = d.store().donor(repo.id, key).ok().flatten()?;
    if dep_key(&path, cfg).is_ok_and(|k| k == key) {
        return Some(path);
    }
    let _ = d.store().delete_donor(repo.id, &path);
    None
}

/// Seeds `from`'s `seed.paths` folders into `path` the given way. Returns them all (for the
/// fix-ups), or nothing if none was seeded.
fn seed_from(from: &Path, path: &Path, cfg: &Config, way: Way) -> Result<Vec<PathBuf>> {
    if way == Way::Clone && faulty("no-clone") {
        return Err(std::io::Error::from(std::io::ErrorKind::Unsupported).into());
    }
    let folders = strata_seed::folders(&git::ignored(from)?, &cfg.seed.paths);
    let seeded = strata_seed::seed_folders(from, path, &folders, way)?;
    Ok(if seeded.is_empty() { vec![] } else { folders })
}

/// Records how a checkout was seeded, then PLAN 4.4's fix-ups: state files pointed at the
/// checkout, and node-gyp addons built in the donor rebuilt with the package manager. Not when
/// hardlinked: a rebuild would write into the donor's files.
fn seeded(s: &mut Seeded, from: PathBuf, path: &Path, folders: &[PathBuf], way: Way, log: &str) {
    s.tier = way.name();
    fix_ups(s, &from, path, folders, way == Way::Hardlink, log);
    s.donor = Some(from);
}

fn fix_ups(
    s: &mut Seeded,
    from: &Path,
    path: &Path,
    folders: &[PathBuf],
    hardlinked: bool,
    log: &str,
) {
    match strata_seed::fix_up(from, path, folders) {
        Ok(f) if f.rebuild.is_empty() => {}
        Ok(f) if hardlinked => s.warnings.push(format!(
            "native addons {} were built in {}; not rebuilt, since that would write into its hardlinked files",
            f.rebuild.join(", "),
            from.display()
        )),
        Ok(f) => {
            if let Some(pm) = detect::detect(path).package_manager {
                match run_logged(path, log, "rebuild", &pm.rebuild(path, &f.rebuild)) {
                    Ok(()) => s.rebuilt = f.rebuild,
                    Err(warning) => s.warnings.push(warning),
                }
            }
        }
        Err(e) => s.warnings.push(format!(
            "pointing state files at the new worktree failed ({e})"
        )),
    }
}

/// Seeds a warm pool entry: like an agent, but it never becomes a donor (it moves).
pub fn seed_entry(d: &Daemon, repo: &Repo, cfg: &Config, path: &Path) -> Seeded {
    seed(d, repo, cfg, path, ".pool", None, false)
}

/// After `pool::take`: the entry's seeding carries over once fix-ups point it at its new
/// folder (PLAN 4.5). Should its dep_key differ after all, its folders go and it is seeded
/// afresh.
fn settle_entry(
    d: &Daemon,
    repo: &Repo,
    cfg: &Config,
    path: &Path,
    agent: &str,
    entry: crate::pool::Entry,
) -> Seeded {
    let key = dep_key(path, cfg).ok();
    let folders = git::ignored(path)
        .map(|i| strata_seed::folders(&i, &cfg.seed.paths))
        .unwrap_or_default();
    if key.as_deref() == Some(entry.dep_key.as_str()) {
        let mut s = Seeded {
            tier: match entry.tier.as_str() {
                "copy" => "copy",
                "hardlink" => "hardlink",
                "install" => "install",
                _ => "clone",
            },
            dep_key: key,
            donor: entry.donor,
            warm: true,
            ..Default::default()
        };
        let hardlinked = s.tier == "hardlink";
        fix_ups(&mut s, &entry.path, path, &folders, hardlinked, agent);
        return s;
    }
    for rel in &folders {
        let _ = strata_seed::remove(&path.join(rel));
    }
    seed(d, repo, cfg, path, agent, None, true)
}

/// Refills the repo's warm pool (after `strata new` took an entry, or at daemon start).
pub fn fill_pool(d: &Daemon, repo_id: i64) {
    let repo = d
        .store()
        .repos()
        .ok()
        .and_then(|r| r.into_iter().find(|r| r.id == repo_id));
    if let Some(repo) = repo
        && let Err(e) = crate::pool::fill(d, &repo)
    {
        tracing::warn!("warm pool for {}: {e}", repo.root.display());
    }
}

/// Seeds a checkout's dependencies (PLAN Phase 4): clones the `seed.paths` folders from the
/// donor with the same dep_key (or from `also`, if it has that key), else installs, after which
/// the checkout is the donor for its key if `donate`. The checkout is usable whatever happens.
fn seed(
    d: &Daemon,
    repo: &Repo,
    cfg: &Config,
    path: &Path,
    log: &str,
    also: Option<&Path>,
    donate: bool,
) -> Seeded {
    let mut s = Seeded {
        tier: "none",
        ..Default::default()
    };
    if !path.join("package.json").exists() {
        return s;
    }
    match dep_key(path, cfg) {
        Ok(k) => s.dep_key = Some(k),
        Err(e) => s.warnings.push(format!(
            "cannot compute the dependency key ({e}); installing"
        )),
    }
    if let Some(key) = s.dep_key.clone() {
        let also = || also.filter(|a| dep_key(a, cfg).is_ok_and(|k| k == key));
        // A shared store is never cloned (PLAN 4.6): the copy would share it anyway, and its
        // relative links break at another folder depth. Installing links it properly.
        if let Some(from) = donor(d, repo, cfg, &key)
            .or_else(|| also().map(Path::to_path_buf))
            .filter(|from| !strata_seed::shared_store(from))
        {
            match seed_from(&from, path, cfg, Way::Clone) {
                Ok(folders) if !folders.is_empty() => {
                    seeded(&mut s, from, path, &folders, Way::Clone, log);
                    return s;
                }
                Ok(_) => {}
                // An install would fill the disk too: stop, the partial clone is already gone.
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::StorageFull => {
                    s.tier = "clone-failed";
                    s.warnings.push(format!(
                        "disk full while cloning dependencies from {}: {} MB free; a full install needs about {} MB",
                        from.display(),
                        strata_seed::free_space(path).unwrap_or(0) / 1_000_000,
                        strata_seed::size(
                            &from,
                            &strata_seed::folders(
                                &git::ignored(&from).unwrap_or_default(),
                                &cfg.seed.paths
                            )
                        ) / 1_000_000,
                    ));
                    return s;
                }
                Err(e) => {
                    let why = format!("cannot clone dependencies from {} ({e})", from.display());
                    let way = match cfg.seed.fallback {
                        Fallback::Install => None,
                        Fallback::Copy => Some(Way::Copy),
                        Fallback::Hardlink => Some(Way::Hardlink),
                    };
                    match way.map(|w| (w, seed_from(&from, path, cfg, w))) {
                        Some((way, Ok(folders))) => {
                            if way == Way::Hardlink {
                                s.warnings.push(format!(
                                    "dependencies are hardlinked from {}: the files are shared, so a write to one in this worktree changes it there too",
                                    from.display()
                                ));
                            }
                            seeded(&mut s, from, path, &folders, way, log);
                            return s;
                        }
                        Some((way, Err(e))) => s.warnings.push(format!(
                            "{why}, and {} failed too ({e}); installing instead",
                            way.name()
                        )),
                        None => s.warnings.push(format!(
                            "{why}; installing instead (set seed.fallback = \"copy\" to copy from it)"
                        )),
                    }
                }
            }
        }
    }
    let (tier, failed) = install_dependencies(path, log);
    s.tier = match tier {
        "install" if strata_seed::shared_store(path) => "shared-store",
        t => t,
    };
    s.warnings.extend(failed);
    if tier == "install"
        && donate
        && let Some(key) = &s.dep_key
    {
        let _ = d.store().set_donor(repo.id, key, path);
    }
    s
}

/// Repos whose base worktree is being seeded right now.
static SEEDING: Mutex<BTreeSet<i64>> = Mutex::new(BTreeSet::new());

/// Gives the base worktree its own dependencies so it is the donor for its dep_key (PLAN 4.1):
/// cloned from a donor or from the main checkout when either has the base's key, else
/// installed. Does nothing when the base already donates its current key. Replaces Phase 2's
/// links into the main checkout.
pub fn seed_base(d: &Daemon, repo: &Repo) -> Result<Seeded> {
    if !SEEDING.lock().unwrap().insert(repo.id) {
        return Ok(Seeded {
            tier: "seeding",
            ..Default::default()
        });
    }
    let result = (|| {
        let cfg = Config::load(&repo.root)?;
        let base = base_path(&repo.root, &cfg);
        if base.join("package.json").exists() {
            let key = dep_key(&base, &cfg)?;
            if d.store().donor(repo.id, &key)?.as_deref() == Some(base.as_path()) {
                return Ok(Seeded {
                    tier: "seeded",
                    dep_key: Some(key),
                    ..Default::default()
                });
            }
        }
        d.store().delete_donor(repo.id, &base)?;
        for rel in strata_seed::folders(&git::ignored(&base)?, &cfg.seed.paths) {
            strata_seed::remove(&base.join(rel))?;
        }
        let s = seed(d, repo, &cfg, &base, ".base", Some(&repo.root), true);
        // A copy or hardlink is the configured fallback where clones fail (ext4): the base
        // still has the key's dependencies, so agents seed from it the same way.
        if matches!(s.tier, "clone" | "copy" | "hardlink")
            && let Some(key) = &s.dep_key
        {
            d.store().set_donor(repo.id, key, &base)?;
        }
        d.store().event(None, "base.seeded", &s.describe())?;
        Ok(s)
    })();
    SEEDING.lock().unwrap().remove(&repo.id);
    result
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
    let args: Vec<String> = pm.install(path).iter().map(|a| a.to_string()).collect();
    match run_logged(path, agent, "install", &args) {
        Ok(()) => ("install", None),
        Err(warning) => ("install-failed", Some(warning)),
    }
}

/// Runs `args` in `path` with its output in `$STRATA_HOME/logs/<agent>/<name>.log`; the
/// warning to give if it fails.
fn run_logged(
    path: &Path,
    agent: &str,
    name: &str,
    args: &[String],
) -> std::result::Result<(), String> {
    let log = log_dir(agent).join(format!("{name}.log"));
    let run = || -> std::io::Result<std::process::ExitStatus> {
        fs::create_dir_all(log_dir(agent))?;
        let out = fs::File::create(&log)?;
        std::process::Command::new(&args[0])
            .args(&args[1..])
            .current_dir(path)
            .stdin(std::process::Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .status()
    };
    match run() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!(
            "`{}` failed ({status}); see {}",
            args.join(" "),
            log.display()
        )),
        Err(e) => Err(format!("could not run `{}`: {e}", args.join(" "))),
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
    let port = crate::ports::ensure(d, &agent, &cfg)?;
    let warnings: Vec<String> = cfg
        .repo
        .agent_note
        .then(|| write_note(&root, &path, &cfg).err())
        .flatten()
        .map(|e| format!("could not write {NOTE}: {e}"))
        .into_iter()
        .collect();
    Ok(json!({ "agent": agent, "port": port, "warnings": warnings }))
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
        let aside = set_aside(root, &cfg, &agent);
        if let Err(e) = git::remove(root, &agent.path, req.force) {
            aside.put_back();
            d.store().set_agent_state(agent.id, AgentState::Ready)?;
            return Err(e);
        }
        aside.discard();
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

/// A worktree's seed folders moved to `<worktree_dir>/.trash/<millis>-<name>/<n>` before
/// `git worktree remove`, which then deletes only the checkout: deleting a seeded cal.com
/// `node_modules` takes 15 s and more, and happens in the background instead.
struct Aside {
    moved: Vec<(PathBuf, PathBuf)>,
    trash: PathBuf,
}

/// Renames (same volume, so instant) whatever can be renamed; anything else stays for git.
fn set_aside(root: &Path, cfg: &Config, agent: &Agent) -> Aside {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let trash = trash_dir(root, cfg).join(format!("{millis}-{}", agent.name));
    let mut aside = Aside {
        moved: vec![],
        trash,
    };
    let folders = git::ignored(&agent.path)
        .map(|i| strata_seed::folders(&i, &cfg.seed.paths))
        .unwrap_or_default();
    if folders.is_empty() || fs::create_dir_all(&aside.trash).is_err() {
        return aside;
    }
    for (n, rel) in folders.iter().enumerate() {
        let (from, to) = (agent.path.join(rel), aside.trash.join(n.to_string()));
        if fs::rename(&from, &to).is_ok() {
            aside.moved.push((from, to));
        }
    }
    aside
}

impl Aside {
    /// `git worktree remove` refused: the worktree stays, so its folders come back.
    fn put_back(self) {
        for (from, to) in &self.moved {
            let _ = fs::rename(to, from);
        }
        let _ = fs::remove_dir(&self.trash);
    }

    fn discard(self) {
        if self.trash.exists() {
            std::thread::spawn(move || strata_seed::remove(&self.trash));
        }
    }
}

fn trash_dir(root: &Path, cfg: &Config) -> PathBuf {
    cfg.worktree_dir(root).join(".trash")
}

/// `strata gc` (PLAN 5.2): removes what nothing uses any more. Agents orphaned by a hand-deleted
/// folder (with their records and logs), donors whose folder is gone, port reservations without
/// an agent, git's records of deleted worktrees, `.strata-seed-*` folders a crash left inside a
/// worktree, logs of agents that no longer exist, and the trash (deleted in the background).
pub fn gc(d: &Daemon) -> Result<Value> {
    let _git = d.git.lock().unwrap();
    let (mut agents_removed, mut donors_removed, mut leftovers, mut trash) = (vec![], 0, 0, 0);
    let mut kept_logs = BTreeSet::from([".base".to_string(), ".pool".to_string()]);
    let repos = d.store().repos()?;
    for repo in repos {
        let agents = d.store().agents(repo.id)?;
        for a in &agents {
            if a.state == AgentState::Orphaned {
                d.store().delete_agent(a.id)?;
                d.store().event(
                    Some(a.id),
                    "agent.removed",
                    &format!("{} (orphaned; gc)", a.name),
                )?;
                agents_removed.push(a.name.clone());
                continue;
            }
            kept_logs.insert(a.name.clone());
            if a.state == AgentState::Ready && a.path.exists() {
                for rel in git::git(
                    &a.path,
                    &[
                        "ls-files",
                        "-z",
                        "--others",
                        "--exclude-standard",
                        "--directory",
                    ],
                )?
                .split('\0')
                .filter(|p| {
                    p.trim_end_matches('/')
                        .rsplit('/')
                        .next()
                        .is_some_and(|n| n.starts_with(".strata-seed-"))
                }) {
                    strata_seed::remove(&a.path.join(rel.trim_end_matches('/')))?;
                    leftovers += 1;
                }
            }
        }
        let donors = d.store().donors(repo.id)?;
        for path in donors {
            if !path.exists() {
                d.store().delete_donor(repo.id, &path)?;
                donors_removed += 1;
            }
        }
        git::git(&repo.root, &["worktree", "prune"])?;
        if let Ok(cfg) = Config::load(&repo.root) {
            let dir = trash_dir(&repo.root, &cfg);
            trash += fs::read_dir(&dir).map_or(0, |e| e.count());
            if dir.exists() {
                std::thread::spawn(move || strata_seed::remove(&dir));
            }
        }
    }
    let ports_freed = d.store().delete_orphan_ports()?;
    let mut logs_removed = vec![];
    for entry in fs::read_dir(strata_core::strata_home().join("logs"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() && !kept_logs.contains(&name) {
            strata_seed::remove(&entry.path())?;
            logs_removed.push(name);
        }
    }
    Ok(json!({
        "agents_removed": agents_removed,
        "donors_removed": donors_removed,
        "ports_freed": ports_freed,
        "seed_leftovers_removed": leftovers,
        "trash_removed": trash,
        "logs_removed": logs_removed,
    }))
}

/// At startup, before serving: settle operations a crash interrupted, and orphan ready agents
/// whose worktree git no longer knows. An interrupted `new` is undone (its caller got an error and
/// never saw the path); an interrupted `rm` is finished if git already removed the worktree,
/// otherwise the agent is ready again so the caller can retry.
pub fn recover(d: &Daemon) -> Result<()> {
    let _git = d.git.lock().unwrap();
    let repos = d.store().repos()?;
    for repo in repos {
        if let Ok(cfg) = Config::load(&repo.root) {
            crate::pool::recover(&repo.root, &cfg)?;
        }
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
    // At start, a base that is not yet the donor for its key (new, or seeded by Phase 2's
    // links); then the warm pool of repos in use. Bound first: a `for` would hold the store
    // guard of its head expression through the body, which takes the store lock again.
    let repos = d.store().repos().unwrap_or_default();
    for repo in repos {
        let in_use = d.store().agents(repo.id).is_ok_and(|a| !a.is_empty());
        let d = d.clone();
        tokio::task::spawn_blocking(move || {
            // Trash a stopped daemon did not finish deleting (see `Aside`).
            if let Ok(cfg) = Config::load(&repo.root) {
                let _ = strata_seed::remove(&trash_dir(&repo.root, &cfg));
            }
            if let Err(e) = seed_base(&d, &repo) {
                tracing::warn!("seeding the base worktree of {}: {e}", repo.root.display());
            }
            if in_use {
                fill_pool(&d, repo.id);
            }
        });
    }
    loop {
        every.tick().await;
        n += 1;
        let dd = d.clone();
        match tokio::task::spawn_blocking(move || tick(&dd)).await {
            Ok(Ok((orphaned, moved))) => {
                for repo in moved {
                    reseed_base(&d, repo);
                }
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

/// Seeds the base in the background: installing can take minutes.
fn reseed_base(d: &Arc<Daemon>, repo: Repo) {
    let d = d.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = seed_base(&d, &repo) {
            tracing::warn!("seeding the base worktree of {}: {e}", repo.root.display());
        }
    });
}

/// Agents marked orphaned, with their repo root.
type Orphans = Vec<(PathBuf, Agent)>;

/// Returns agents it marked orphaned, with their repo root, so their processes and snapshots
/// can go, and repos whose base moved, so it can be seeded again if its dep_key changed.
fn tick(d: &Daemon) -> Result<(Orphans, Vec<Repo>)> {
    crate::procs::refresh(d);
    let (mut orphaned, mut moved) = (vec![], vec![]);
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
                moved.push(repo.clone());
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
    Ok((orphaned, moved))
}

#[derive(Deserialize)]
pub struct Check {
    pub repo: PathBuf,
    /// diagnostics | hover | definition | references | symbols | conflicts
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

/// Name and worktree of each ready agent of a repo.
fn ready_worktrees(d: &Daemon, repo: &Repo) -> Result<Vec<(String, PathBuf)>> {
    Ok(d.store()
        .agents(repo.id)?
        .into_iter()
        .filter(|a| a.state == AgentState::Ready)
        .map(|a| (a.name, a.path))
        .collect())
}

/// Every 5 s: clash checks between each repo's agents (see strata-checker's `clash.rs`).
pub async fn clashes(d: Arc<Daemon>) {
    let mut every = tokio::time::interval(Duration::from_secs(5));
    let mut last_error = String::new();
    loop {
        every.tick().await;
        let repos = d.store().repos().unwrap_or_default();
        for repo in repos {
            let Ok(cfg) = Config::load(&repo.root) else {
                continue;
            };
            let agents = ready_worktrees(&d, &repo).unwrap_or_default();
            if !cfg.checker.conflicts || agents.len() < 2 {
                continue;
            }
            let base = base_path(&repo.root, &cfg);
            let settings = Settings::from_config(&cfg);
            match d
                .checker
                .check_clashes(&repo.root, &base, &agents, &settings, None)
                .await
            {
                Ok(()) => {}
                // The checker is off for this repo (`strata doctor` says why).
                Err(Error::Refused(_)) => {}
                Err(e) => {
                    let e = format!("clash check in {}: {e}", repo.root.display());
                    if e != last_error {
                        tracing::warn!("{e}");
                        last_error = e;
                    }
                }
            }
        }
    }
}

pub async fn check(d: &Daemon, name: String, req: Check) -> Result<Value> {
    let (repo, agent, cfg) = ready_agent(d, &req.repo, &name)?;
    if req.op == "conflicts" {
        if !cfg.checker.conflicts {
            return Err(Error::Refused(
                "clash checks are off (checker.conflicts = false in strata.toml)".into(),
            ));
        }
        let agents = ready_worktrees(d, &repo)?;
        let base = base_path(&repo.root, &cfg);
        let settings = Settings::from_config(&cfg);
        d.checker
            .check_clashes(&repo.root, &base, &agents, &settings, Some(&name))
            .await?;
        let pairs = d.checker.clashes(&repo.root, &name).await;
        // Agents with no pair against the caller: one of the two has no changes yet, so a
        // clash with them can still appear after this answer.
        let unchecked: Vec<&String> = agents
            .iter()
            .map(|(a, _)| a)
            .filter(|a| **a != name && !pairs.iter().any(|p| p["with"].as_str() == Some(a)))
            .collect();
        return Ok(json!({ "pairs": pairs, "unchecked": unchecked }));
    }
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
