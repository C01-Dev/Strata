//! Clash checks between agents, before either is merged (NOTES: "Clash detection";
//! docs/spec-clash-any-repo.md).
//!
//! A round looks at every agent's worktree in two ways:
//!
//! - As a git tree (`strata_git::Shadow`), in any repo. Two agents' trees are merged by git, and
//!   the files it cannot merge are reported, whatever their type. Needs git 2.38.
//! - As a layer on the shared checker's base, where the repo has that checker. For a pair git
//!   can merge, the code files either agent changed are type-checked with each agent's layer
//!   alone and with both at once. An error the combination has and neither has alone is a
//!   clash. Each snapshot is made, read and disposed before the next, so a round adds one
//!   snapshot at a time to the group. Errors in files neither agent changed are missed; on
//!   excalidraw this found the same clashes as checking the whole project, 3 to 30 times faster.
//!
//! A pair with a file git cannot merge is not type-checked: the combination does not exist
//! until someone resolves the merge.
//!
//! The daemon runs a round every few seconds. A pair is checked once both agents have held
//! still since the previous round, then again only when one of them, or the base, changes.

use super::{Broker, GROUP, OPEN_TIMEOUT, QUERY_TIMEOUT, Settings, scan, typescript_version};
use crate::bridge::Bridge;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use strata_core::{Error, Result};
use strata_git::{Merge, Shadow};
use strata_overlay::{Layer, Tree, layer};
use xxhash_rust::xxh3::xxh3_64;

/// Files the checker reports errors in.
const CODE: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// An agent whose files did not change at the last look is looked at again every this many
/// rounds (30 s at the daemon's 5 s), not every round: taking a worktree as a git tree costs
/// 30 to 150 ms, against 7 to 55 ms for the layer scan.
// ponytail: polling with back-off; a file watcher replaces both scans if the CPU shows.
const LOOK_EVERY: u32 = 6;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Clash {
    /// Relative to the repo root.
    pub file: String,
    /// 1-based, in the combined file.
    pub line: u64,
    pub column: u64,
    pub code: u64,
    pub message: String,
    /// Other places in the same file with the same error, as `[line, column]`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<[u64; 2]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pair {
    /// In name order.
    pub agents: [String; 2],
    /// Type errors only the combination has.
    pub clashes: Vec<Clash>,
    /// Files git cannot merge. A pair with any is not type-checked.
    pub unmerged: Vec<String>,
    /// Why the pair's types were not checked.
    pub skipped: Option<String>,
    /// Both agents' files and layers, and the base, that it was checked at.
    #[serde(skip)]
    key: u64,
}

impl Pair {
    pub fn is_clean(&self) -> bool {
        self.clashes.is_empty() && self.unmerged.is_empty() && self.skipped.is_none()
    }
}

/// An error from the bridge: file relative to the repo root, 0-based position.
#[derive(Debug, Clone, PartialEq)]
struct Diag {
    file: String,
    line: u64,
    character: u64,
    code: u64,
    message: String,
}

impl Diag {
    fn key(&self) -> (&str, u64, &str) {
        (&self.file, self.code, &self.message)
    }
}

/// Errors per file.
type Files = HashMap<String, Vec<Diag>>;

/// One repo's clash checks. It belongs to the broker, not to the checker group, so a repo
/// without the shared checker has one too.
#[derive(Default)]
pub(super) struct State {
    /// Held for a whole round, so rounds never overlap.
    round: tokio::sync::Mutex<Round>,
    /// The latest check of every pair of agents with changes.
    pairs: std::sync::Mutex<Vec<Pair>>,
}

impl State {
    /// How many pairs are checked, and those with something to report: type clashes first,
    /// then files git cannot merge, then pairs whose types were not checked.
    pub(super) fn open(&self) -> (usize, Vec<Pair>) {
        let pairs = self.pairs.lock().unwrap();
        let mut open: Vec<Pair> = pairs.iter().filter(|p| !p.is_clean()).cloned().collect();
        open.sort_by_key(|p| (p.clashes.is_empty(), p.skipped.is_some(), p.agents.clone()));
        (pairs.len(), open)
    }
}

#[derive(Default)]
struct Round {
    /// What the layers below were scanned with. Other settings start them over.
    settings: Option<Settings>,
    /// The round's own scan of the base checkout. The group's is left alone: rescanning it
    /// here would hide base changes from the next query, which reopens the base on a change.
    base: Option<Tree>,
    /// Bumped whenever the base checkout's content changes.
    base_gen: u64,
    trees: HashMap<String, Tree>,
    /// Each agent's git tree and layer hash at the previous round.
    last: HashMap<String, (String, u64)>,
    /// Errors per file with one agent's layer alone, for (layer hash, base_gen).
    alone: HashMap<String, ((u64, u64), Files)>,
    shadow: Option<Arc<Shadow>>,
    /// Each agent's worktree as git last saw it.
    seen: HashMap<String, Seen>,
}

struct Seen {
    tree: String,
    commit: String,
    /// Rounds since the tree last changed.
    idle: u32,
}

struct Side {
    name: String,
    worktree: PathBuf,
    /// Its files as a git tree, and a commit of that tree (empty when git could not say).
    tree: String,
    commit: String,
    /// Its layer on the checker's base (empty without the shared checker).
    layer: Layer,
    /// Changed code files.
    code: Vec<String>,
    settled: bool,
}

impl Side {
    fn layered(&self) -> bool {
        !self.layer.changed.is_empty() || !self.layer.removed.is_empty()
    }
}

/// What git made of merging a pair's trees.
#[derive(PartialEq)]
enum Merged {
    Yes,
    /// The files it cannot merge.
    No(Vec<String>),
    /// This git cannot merge trees: the checker's own file-by-file merge decides.
    Unknown,
    /// The merge itself failed; the pair is tried again next round.
    Failed,
}

/// What a round needs from a running checker group.
struct Checker {
    bridge: Arc<Bridge>,
    runtime: String,
    typescript: String,
    program_files: u64,
    base_dep: String,
}

fn is_code(rel: &str) -> bool {
    rel.rsplit_once('.')
        .is_some_and(|(_, ext)| CODE.contains(&ext))
}

/// Strata's own git folder for a repo's clash checks (see `strata_git::Shadow`), also holding
/// the merged files of a pair while it is type-checked.
fn store(home: &Path, repo: &Path) -> PathBuf {
    let key = xxh3_64(repo.as_os_str().as_encoded_bytes());
    home.join("clash").join(format!("{key:016x}"))
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| Error::Checker(e.to_string()))?
}

impl Broker {
    fn clash_state(&self, repo: &Path) -> Arc<State> {
        let mut all = self.clashes.lock().unwrap();
        all.entry(repo.to_path_buf()).or_default().clone()
    }

    /// One round of clash checks between a repo's agents (each name with its worktree). With
    /// `only`, every agent is looked at now and that agent's pairs are checked, settled or not,
    /// and the memory limit does not hold them back.
    pub async fn check_clashes(
        &self,
        repo: &Path,
        base: &Path,
        agents: &[(String, PathBuf)],
        settings: &Settings,
        only: Option<&str>,
    ) -> Result<()> {
        let git = strata_git::merges_trees() && base.join(".git").exists();
        let group = match self.group(repo, base, settings).await {
            Ok(g) => Some(g),
            // No shared checker here: git's merges are still checked.
            Err(Error::Refused(_)) if git => None,
            Err(e) => return Err(e),
        };
        let state = self.clash_state(repo);
        let mut round = state.round.lock().await;
        let r = &mut *round;
        if r.settings.as_ref() != Some(settings) {
            (r.settings, r.base) = (Some(settings.clone()), None);
            r.trees.clear();
            r.alone.clear();
            r.base_gen += 1;
        }
        let live = |n: &String| agents.iter().any(|(a, _)| a == n);
        r.trees.retain(|n, _| live(n));
        r.last.retain(|n, _| live(n));
        r.seen.retain(|n, _| live(n));

        let base_tree = match &group {
            Some(_) => {
                let unscanned = || Tree::new(base, &settings.extra_paths);
                let (tree, moved) = scan(r.base.take().unwrap_or_else(unscanned)).await?;
                r.base_gen += u64::from(moved);
                Some(tree)
            }
            None => None,
        };
        let shadow = match (git, &r.shadow) {
            (false, _) => None,
            (true, Some(shadow)) => Some(shadow.clone()),
            (true, None) => {
                let (dir, repo) = (store(&self.home, repo), repo.to_path_buf());
                let shadow = Arc::new(blocking(move || Shadow::open(&dir, &repo)).await?);
                r.shadow = Some(shadow.clone());
                Some(shadow)
            }
        };
        let base_id = match &shadow {
            Some(_) => {
                let base = base.to_path_buf();
                blocking(move || strata_git::head_tree(&base)).await?
            }
            None => String::new(),
        };

        let mut sides = vec![];
        for (name, wt) in agents {
            let layer = match &base_tree {
                Some(base_tree) => {
                    let tree = match r.trees.remove(name) {
                        Some(t) if t.root() == wt => t,
                        _ => Tree::new(wt, &settings.extra_paths),
                    };
                    // A worktree being removed fails its scan; it leaves the list next round.
                    let Ok((tree, _)) = scan(tree).await else {
                        continue;
                    };
                    let l = layer(base_tree, &tree);
                    r.trees.insert(name.clone(), tree);
                    l
                }
                None => Layer::default(),
            };
            if let Some(shadow) = &shadow {
                let idle = r.seen.get(name).map(|s| s.idle);
                // The index goes with a removed agent, so a new one of the same name is new.
                let look = only.is_some()
                    || idle.is_none_or(|idle| idle % LOOK_EVERY == 0)
                    || r.last
                        .get(name)
                        .is_some_and(|(_, hash)| *hash != layer.hash)
                    || !shadow.index(name).exists();
                if look {
                    let (shadow, wt, index) = (shadow.clone(), wt.clone(), shadow.index(name));
                    let taken = blocking(move || {
                        let tree = shadow.tree(&wt, &index)?;
                        let commit = shadow.commit(&wt, &tree)?;
                        Ok((tree, commit))
                    });
                    match taken.await {
                        Ok((tree, commit)) => {
                            let idle = match r.seen.get(name) {
                                Some(s) if s.tree == tree => s.idle + 1,
                                _ => 0,
                            };
                            let seen = Seen { tree, commit, idle };
                            r.seen.insert(name.clone(), seen);
                        }
                        // A worktree being removed, or one git cannot add (a folder with its
                        // own empty repo): the agent goes on without a tree, as on a git too
                        // old to merge trees.
                        Err(e) => {
                            tracing::debug!(agent = name, "cannot take its files as a tree: {e}");
                            r.seen.remove(name);
                        }
                    }
                } else if let Some(s) = r.seen.get_mut(name) {
                    s.idle += 1;
                }
            }
            let (tree, commit) = match r.seen.get(name) {
                Some(s) => (s.tree.clone(), s.commit.clone()),
                None => Default::default(),
            };
            let now = (tree.clone(), layer.hash);
            let settled = r.last.insert(name.clone(), now.clone()) == Some(now);
            let side = Side {
                name: name.clone(),
                worktree: wt.clone(),
                code: layer
                    .changed
                    .iter()
                    .filter(|f| is_code(f))
                    .cloned()
                    .collect(),
                layer,
                tree,
                commit,
                settled,
            };
            if side.layered() || (!side.tree.is_empty() && side.tree != base_id) {
                sides.push(side);
            }
        }
        r.base = base_tree;
        sides.sort_by(|a, b| a.name.cmp(&b.name));
        r.alone.retain(|n, _| sides.iter().any(|s| s.name == *n));

        let mut current = vec![];
        let mut wanted = vec![];
        for i in 0..sides.len() {
            for j in i + 1..sides.len() {
                let (a, b) = (&sides[i], &sides[j]);
                current.push([a.name.clone(), b.name.clone()]);
                let settled = only.map_or(a.settled && b.settled, |o| o == a.name || o == b.name);
                if settled {
                    let key = format!(
                        "{} {} {} {} {base_id} {}",
                        a.tree, b.tree, a.layer.hash, b.layer.hash, r.base_gen
                    );
                    wanted.push((i, j, xxh3_64(key.as_bytes())));
                }
            }
        }
        let needed: Vec<_> = {
            let mut pairs = state.pairs.lock().unwrap();
            pairs.retain(|p| current.contains(&p.agents));
            wanted
                .into_iter()
                .filter(|(i, j, key)| {
                    !pairs.iter().any(|p| {
                        p.agents == [sides[*i].name.as_str(), sides[*j].name.as_str()]
                            && p.key == *key
                    })
                })
                .collect()
        };
        if needed.is_empty() {
            return Ok(());
        }

        // First what git makes of each pair, which is cheap and holds no checker memory.
        let mut failed = None;
        let mut merges = vec![];
        for &(i, j, _) in &needed {
            let (a, b) = (&sides[i], &sides[j]);
            merges.push(match &shadow {
                None => Merged::Unknown,
                Some(_) if a.tree.is_empty() || b.tree.is_empty() => Merged::Unknown,
                // One has only ignored files in its layer, or both made the same changes.
                Some(_) if a.tree == base_id || b.tree == base_id || a.tree == b.tree => {
                    Merged::Yes
                }
                Some(shadow) => {
                    let (shadow, repo) = (shadow.clone(), repo.to_path_buf());
                    let (a, b) = (a.commit.clone(), b.commit.clone());
                    match blocking(move || shadow.merge(&repo, &a, &b)).await {
                        Ok(Merge::Clean(_)) => Merged::Yes,
                        Ok(Merge::Unmerged(files)) => Merged::No(files),
                        Err(e) => {
                            failed.get_or_insert(e);
                            Merged::Failed
                        }
                    }
                }
            });
        }

        // What git alone decides is recorded at once: the type checks after it can take minutes.
        let blank = |i: usize, j: usize, key, skipped| Pair {
            agents: [sides[i].name.clone(), sides[j].name.clone()],
            clashes: vec![],
            unmerged: vec![],
            skipped,
            key,
        };
        let record = |pair: Pair| {
            if !pair.clashes.is_empty() || !pair.unmerged.is_empty() {
                tracing::info!(a = %pair.agents[0], b = %pair.agents[1], clashes = pair.clashes.len(), unmerged = pair.unmerged.len(), "agents' changes clash");
            }
            let mut pairs = state.pairs.lock().unwrap();
            pairs.retain(|p| p.agents != pair.agents);
            pairs.push(pair);
        };
        // Left for the checker: the pairs git can merge, where both layers changed something and
        // the repo has the shared checker.
        let mut typed = vec![];
        for ((i, j, key), merged) in needed.into_iter().zip(merges) {
            match merged {
                Merged::Failed => {}
                Merged::No(unmerged) => record(Pair {
                    unmerged,
                    ..blank(i, j, key, None)
                }),
                merged if group.is_some() && sides[i].layered() && sides[j].layered() => {
                    typed.push((i, j, key, merged))
                }
                _ => record(blank(i, j, key, None)),
            }
        }
        // tsgo starts only when a pair needs it. While the group is above its memory limit,
        // type checks in the background wait, and nothing is recorded for their pairs.
        let c = match &group {
            Some(group) if !typed.is_empty() => {
                if only.is_none() && group.memory().await > settings.group_memory_bytes {
                    tracing::debug!("checker group above group_memory_mb; clash checks wait");
                    return failed.map_or(Ok(()), Err);
                }
                let runtime = self.runtime().await?.to_string();
                let mut s = group.state.lock().await;
                group.fresh(&mut s, &runtime, &self.home).await?;
                Checker {
                    bridge: s.bridge.clone().expect("started"),
                    typescript: s.typescript.clone(),
                    program_files: s.program_files,
                    base_dep: r.base.as_ref().expect("scanned").dep_key(&runtime)?,
                    runtime,
                }
            }
            _ => return failed.map_or(Ok(()), Err),
        };
        let mut unshared: HashMap<&str, String> = HashMap::new();
        let mut check = BTreeSet::new();
        let mut judged = HashSet::new();
        for &(i, j, ..) in &typed {
            for side in [&sides[i], &sides[j]] {
                if !judged.insert(side.name.as_str()) {
                    continue;
                }
                let reason = if let Some(v) =
                    typescript_version(&side.worktree).filter(|v| *v != c.typescript)
                {
                    Some(format!(
                        "{}'s TypeScript {v} differs from the base's {}",
                        side.name, c.typescript
                    ))
                } else if r.trees[&side.name].dep_key(&c.runtime)? != c.base_dep {
                    Some(format!(
                        "{}'s dependencies differ from the base's",
                        side.name
                    ))
                } else if side.layer.changed.len() as f64
                    > settings.huge_diff_ratio * c.program_files.max(1) as f64
                {
                    Some(format!(
                        "{} changed too many files to check against",
                        side.name
                    ))
                } else {
                    check.extend(side.code.iter().cloned());
                    None
                };
                if let Some(reason) = reason {
                    unshared.insert(&side.name, reason);
                }
            }
        }

        for (i, j, key, merged) in typed {
            let (a, b) = (&sides[i], &sides[j]);
            let skipped = [a, b]
                .iter()
                .find_map(|s| unshared.get(s.name.as_str()).cloned());
            if skipped.is_some() {
                record(blank(i, j, key, skipped));
                continue;
            }
            for side in [a, b] {
                alone(r, &c.bridge, side, &check, base).await?;
            }
            let tmp = store(&self.home, repo)
                .join("tmp")
                .join(format!("{}+{}", a.name, b.name));
            let both = combine(r, &c.bridge, a, b, base, &tmp).await;
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            record(match both? {
                Ok(clashes) => Pair {
                    clashes,
                    ..blank(i, j, key, None)
                },
                // git merged the two trees, so these files merge with what a merge file by
                // file lacks. Without that, the combination cannot be put on the checker.
                Err(files) if merged == Merged::Yes => blank(
                    i,
                    j,
                    key,
                    Some(format!(
                        "git merges {} only by following a rename or the branches' history, so their types cannot be checked together",
                        files.join(", ")
                    )),
                ),
                Err(unmerged) => Pair {
                    unmerged,
                    ..blank(i, j, key, None)
                },
            });
        }
        failed.map_or(Ok(()), Err)
    }

    /// Pairs including `agent`, with the other one as `with`. Unless `all`, only those with
    /// clashes or files git cannot merge.
    pub(super) fn pairs_of(&self, repo: &Path, agent: &str, all: bool) -> Vec<Value> {
        let Some(state) = self.clashes.lock().unwrap().get(repo).cloned() else {
            return vec![];
        };
        let pairs = state.pairs.lock().unwrap();
        pairs
            .iter()
            .filter(|p| p.agents.iter().any(|a| a == agent))
            .filter(|p| all || !p.clashes.is_empty() || !p.unmerged.is_empty())
            .map(|p| {
                let with = if p.agents[0] == agent { &p.agents[1] } else { &p.agents[0] };
                json!({ "with": with, "clashes": p.clashes, "unmerged": p.unmerged, "skipped": p.skipped })
            })
            .collect()
    }

    /// Every pair including `agent`, from its side (see `pairs_of`).
    pub async fn clashes(&self, repo: &Path, agent: &str) -> Vec<Value> {
        self.pairs_of(repo, agent, true)
    }

    /// A removed agent: its pairs, and its index in Strata's git folder.
    pub(super) fn forget_clashes(&self, repo: &Path, agent: &str) {
        if let Some(state) = self.clashes.lock().unwrap().get(repo) {
            let mut pairs = state.pairs.lock().unwrap();
            pairs.retain(|p| !p.agents.iter().any(|a| a == agent));
        }
        let _ = std::fs::remove_file(store(&self.home, repo).join(format!("{agent}.index")));
    }

    /// Removes Strata's git folder for a repo (it is rebuilt by the next round that needs it):
    /// its objects only grow while agents work. For a repo left without agents, and `strata gc`.
    pub async fn empty_clash_store(&self, repo: &Path) -> bool {
        let dir = store(&self.home, repo);
        if !dir.exists() {
            return false;
        }
        let state = self.clash_state(repo);
        let mut round = state.round.lock().await;
        round.seen.clear();
        tokio::fs::remove_dir_all(&dir).await.is_ok()
    }
}

/// Checks both layers at once and returns the errors neither has alone, or the files both
/// changed that a merge file by file cannot combine (`tmp` holds the merged ones meanwhile).
async fn combine(
    r: &Round,
    bridge: &Bridge,
    a: &Side,
    b: &Side,
    base: &Path,
    tmp: &Path,
) -> Result<std::result::Result<Vec<Clash>, Vec<String>>> {
    let at = |rel: &str| base.join(rel).to_string_lossy().into_owned();
    let mut files: Map<String, Value> = a
        .layer
        .changed
        .iter()
        .map(|rel| (at(rel), json!(a.worktree.join(rel))))
        .collect();
    let mut unmerged = vec![];
    for rel in &b.layer.changed {
        let read = if a.layer.changed.binary_search(rel).is_ok() {
            let out = tmp.join(rel);
            let a_b = (&a.worktree.join(rel), &b.worktree.join(rel));
            if !merge(a_b.0, &base.join(rel), a_b.1, &out).await? {
                unmerged.push(rel.clone());
            }
            out
        } else {
            b.worktree.join(rel)
        };
        files.insert(at(rel), json!(read));
    }
    for (x, y) in [(a, b), (b, a)] {
        for rel in &x.layer.removed {
            if y.layer.changed.binary_search(rel).is_ok() {
                unmerged.push(rel.clone());
            }
        }
    }
    // Until someone resolves the merge there is no combination to check; picking one side's
    // version would report errors that come from the pick.
    if !unmerged.is_empty() {
        unmerged.sort();
        return Ok(Err(unmerged));
    }
    let removed: BTreeSet<String> = a
        .layer
        .removed
        .iter()
        .chain(&b.layer.removed)
        .map(|rel| at(rel))
        .filter(|p| !files.contains_key(p))
        .collect();
    let touched: BTreeSet<String> = a.code.iter().chain(&b.code).cloned().collect();
    let name = format!("clash:{}+{}", a.name, b.name);
    let both = diagnose(bridge, &name, files, removed, &touched, base).await?;
    let both: Vec<Diag> = both.into_values().flatten().collect();
    let own: Vec<&Diag> = [a, b]
        .iter()
        .flat_map(|s| touched.iter().flat_map(|f| &r.alone[&s.name].1[f]))
        .collect();
    let mut clashes: Vec<Clash> = only_together(&both, &own)
        .into_iter()
        .map(|d| Clash {
            file: d.file,
            line: d.line + 1,
            column: d.character + 1,
            code: d.code,
            message: d.message,
            also: vec![],
        })
        .collect();
    clashes.sort_by(|x, y| (&x.file, x.line, x.column).cmp(&(&y.file, y.line, y.column)));
    Ok(Ok(grouped(clashes)))
}

/// Fills in the agent's own errors in `check` files it has none recorded for.
async fn alone(
    r: &mut Round,
    bridge: &Bridge,
    side: &Side,
    check: &BTreeSet<String>,
    base: &Path,
) -> Result<()> {
    let key = (side.layer.hash, r.base_gen);
    let entry = r
        .alone
        .entry(side.name.clone())
        .or_insert_with(|| (key, Files::new()));
    if entry.0 != key {
        *entry = (key, Files::new());
    }
    let missing: BTreeSet<String> = check
        .iter()
        .filter(|f| !entry.1.contains_key(*f))
        .cloned()
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let files = side
        .layer
        .changed
        .iter()
        .map(|rel| {
            (
                base.join(rel).to_string_lossy().into_owned(),
                json!(side.worktree.join(rel)),
            )
        })
        .collect();
    let removed = side
        .layer
        .removed
        .iter()
        .map(|rel| base.join(rel).to_string_lossy().into_owned())
        .collect();
    let name = format!("clash:{}", side.name);
    let found = diagnose(bridge, &name, files, removed, &missing, base).await?;
    entry.1.extend(found);
    Ok(())
}

/// Makes a snapshot of this layer on the base, reads the errors in each `check` file
/// (relative to the repo), and disposes the snapshot.
async fn diagnose(
    bridge: &Bridge,
    name: &str,
    files: Map<String, Value>,
    removed: BTreeSet<String>,
    check: &BTreeSet<String>,
    base: &Path,
) -> Result<Files> {
    let params = json!({ "group": GROUP, "agent": name, "files": files, "removed": removed });
    bridge.call("createSnapshot", params, OPEN_TIMEOUT).await?;
    let mut out = Files::new();
    let mut failed = None;
    for rel in check {
        let params = json!({ "agent": name, "file": base.join(rel) });
        match bridge.call("diagnostics", params, QUERY_TIMEOUT).await {
            Ok(v) => {
                out.insert(rel.clone(), errors(&v, base));
            }
            // Not in any project (a script outside tsconfig's include, or removed here).
            Err(Error::NotFound(_)) => {
                out.insert(rel.clone(), vec![]);
            }
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    let _ = bridge
        .call("dispose", json!({ "agent": name }), QUERY_TIMEOUT)
        .await;
    failed.map_or(Ok(out), Err)
}

fn errors(v: &Value, base: &Path) -> Vec<Diag> {
    v["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["category"] == "error")
        .map(|d| {
            let file = d["file"].as_str().unwrap_or_default();
            Diag {
                file: Path::new(file)
                    .strip_prefix(base)
                    .map_or(file.into(), |p| p.to_string_lossy().into_owned()),
                line: d["line"].as_u64().unwrap_or(0),
                character: d["character"].as_u64().unwrap_or(0),
                code: d["code"].as_u64().unwrap_or(0),
                message: d["message"].as_str().unwrap_or_default().into(),
            }
        })
        .collect()
}

/// Errors in `both` beyond those in `own`, matched by file, code and message (not position,
/// which the other agent's edits to the same file move), counting repeats.
fn only_together(both: &[Diag], own: &[&Diag]) -> Vec<Diag> {
    let mut seen: HashMap<(&str, u64, &str), usize> = HashMap::new();
    for d in own {
        *seen.entry(d.key()).or_default() += 1;
    }
    both.iter()
        .filter(|d| match seen.get_mut(&d.key()) {
            Some(n) if *n > 0 => {
                *n -= 1;
                false
            }
            _ => true,
        })
        .cloned()
        .collect()
}

/// One entry per file, error code and first sentence of the message, with the other places in
/// `also`: a rename breaks every use of the old name the same way. TypeScript's "Did you mean"
/// guesses differ from place to place, so when the messages differ only that sentence is kept.
fn grouped(clashes: Vec<Clash>) -> Vec<Clash> {
    fn head(m: &str) -> &str {
        let first = m.lines().next().unwrap_or_default();
        first.find(". ").map_or(first, |i| &first[..=i])
    }
    let mut out: Vec<Clash> = vec![];
    for c in clashes {
        let same = |g: &&mut Clash| {
            g.file == c.file && g.code == c.code && head(&g.message) == head(&c.message)
        };
        match out.iter_mut().find(same) {
            Some(g) => {
                if g.message != c.message {
                    g.message = head(&c.message).to_string();
                }
                g.also.push([c.line, c.column]);
            }
            None => out.push(c),
        }
    }
    out
}

/// Writes the three-way merge of `a` and `b` to `out`; false when git cannot merge them.
async fn merge(a: &Path, base: &Path, b: &Path, out: &Path) -> Result<bool> {
    let base = if base.exists() {
        base
    } else {
        Path::new("/dev/null")
    };
    let o = tokio::process::Command::new("git")
        .args(["merge-file", "-p"])
        .args([a, base, b])
        .output()
        .await?;
    match o.status.code() {
        Some(0) => {
            if let Some(dir) = out.parent() {
                tokio::fs::create_dir_all(dir).await?;
            }
            tokio::fs::write(out, &o.stdout).await?;
            Ok(true)
        }
        // The number of conflicts.
        Some(1..=127) => Ok(false),
        _ => Err(Error::Checker(format!(
            "git merge-file {}: {}",
            a.display(),
            String::from_utf8_lossy(&o.stderr).trim()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(file: &str, line: u64, code: u64, message: &str) -> Diag {
        Diag {
            file: file.into(),
            line,
            character: 0,
            code,
            message: message.into(),
        }
    }

    #[test]
    fn only_errors_neither_agent_has_alone_are_clashes() {
        let mine = d("a.ts", 3, 2322, "Bad.");
        let theirs = d("b.ts", 1, 2304, "Cannot find name 'x'.");
        let both = [
            d("a.ts", 9, 2322, "Bad."), // mine, moved down by their edit to a.ts
            d("b.ts", 1, 2304, "Cannot find name 'x'."),
            d("b.ts", 4, 2304, "Cannot find name 'x'."), // a second one
            d("c.ts", 2, 2305, "Module has no exported member 'y'."),
        ];
        let clashes = only_together(&both, &[&mine, &theirs]);
        assert_eq!(
            clashes,
            [
                d("b.ts", 4, 2304, "Cannot find name 'x'."),
                d("c.ts", 2, 2305, "Module has no exported member 'y'.")
            ]
        );
    }

    #[test]
    fn the_same_error_in_one_file_is_one_clash() {
        let c = |file: &str, line, code, message: &str| Clash {
            file: file.into(),
            line,
            column: 3,
            code,
            message: message.into(),
            also: vec![],
        };
        let name = "Cannot find name 'getBoundTextElement'.";
        let g = grouped(vec![
            c(
                "a.tsx",
                160,
                2552,
                &format!("{name} Did you mean 'hasBoundTextElement'?"),
            ),
            c(
                "a.tsx",
                168,
                2552,
                &format!("{name} Did you mean 'boundTextElement'?"),
            ),
            c("a.tsx", 170, 2304, name),
            c(
                "b.ts",
                1,
                2552,
                &format!("{name} Did you mean 'hasBoundTextElement'?"),
            ),
        ]);
        assert_eq!(g.len(), 3);
        assert_eq!((g[0].line, g[0].message.as_str()), (160, name));
        assert_eq!(g[0].also, [[168, 3]]);
        assert!(g[1].also.is_empty() && g[2].also.is_empty());
        assert!(
            g[2].message.contains("Did you mean"),
            "one place keeps the whole message"
        );
    }

    #[test]
    fn code_files_are_the_ones_checked() {
        assert!(is_code("src/a.ts") && is_code("app/page.tsx") && is_code("x.mjs"));
        assert!(!is_code("package.json") && !is_code("README.md") && !is_code("Makefile"));
    }

    #[tokio::test]
    async fn merges_or_says_it_cannot() {
        let tmp = tempfile::tempdir().unwrap();
        let p = |n: &str| tmp.path().join(n);
        std::fs::write(p("base"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(p("a"), "ONE\ntwo\nthree\n").unwrap();
        std::fs::write(p("b"), "one\ntwo\nTHREE\n").unwrap();
        std::fs::write(p("c"), "uno\ntwo\nthree\n").unwrap();
        assert!(
            merge(&p("a"), &p("base"), &p("b"), &p("out/m"))
                .await
                .unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(p("out/m")).unwrap(),
            "ONE\ntwo\nTHREE\n"
        );
        assert!(
            !merge(&p("a"), &p("base"), &p("c"), &p("out/n"))
                .await
                .unwrap()
        );
        // A file new in both, with no base version.
        assert!(
            !merge(&p("a"), &p("missing"), &p("c"), &p("out/o"))
                .await
                .unwrap()
        );
        assert!(
            merge(&p("a"), &p("missing"), &p("a"), &p("out/p"))
                .await
                .unwrap()
        );
    }
}
