//! Clash checks: type errors that appear only once two agents' changes are combined, found
//! before either is merged (NOTES: "Clash detection").
//!
//! For a pair of agents whose layers both changed, the code files either agent changed are
//! checked with each agent's layer alone and with both layers at once. Files both edited get a
//! three-way merge (`git merge-file`); a pair with a file git cannot merge reports that file
//! and is not type-checked. An error the combination has and neither agent's own layer has is
//! a clash. Each snapshot is made, read and disposed before the next, so a round adds one
//! snapshot at a time to the group. Errors in files neither agent changed are missed; on
//! excalidraw this found the same clashes as checking the whole project, 3 to 30 times faster.
//!
//! The daemon runs a round every few seconds. A pair is checked once both layers have held
//! still since the previous round, then again only when one of them, or the base, changes.

use super::{
    Broker, GROUP, Group, OPEN_TIMEOUT, QUERY_TIMEOUT, Settings, scan, typescript_version,
};
use crate::bridge::Bridge;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use strata_core::{Error, Result};
use strata_overlay::{Layer, Tree, layer};

/// Files the checker reports errors in.
const CODE: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

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
    /// Files both agents changed that git cannot merge, or that one removed and the other
    /// edited. A pair with any is not type-checked.
    pub unmerged: Vec<String>,
    /// Why the pair was not checked.
    pub skipped: Option<String>,
    /// The two layer hashes and the base generation it was checked at.
    #[serde(skip)]
    key: (u64, u64, u64),
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

#[derive(Default)]
pub(super) struct Round {
    /// The round's own scan of the base checkout. The group's is left alone: rescanning it
    /// here would hide base changes from the next query, which reopens the base on a change.
    base: Option<Tree>,
    /// Bumped whenever the base checkout's content changes.
    base_gen: u64,
    trees: HashMap<String, Tree>,
    /// Each agent's layer hash at the previous round.
    last: HashMap<String, u64>,
    /// Errors per file with one agent's layer alone, for (layer hash, base_gen).
    alone: HashMap<String, ((u64, u64), Files)>,
}

struct Side {
    name: String,
    worktree: PathBuf,
    layer: Layer,
    /// Changed code files.
    code: Vec<String>,
    settled: bool,
}

fn is_code(rel: &str) -> bool {
    rel.rsplit_once('.')
        .is_some_and(|(_, ext)| CODE.contains(&ext))
}

impl Broker {
    /// One round of clash checks between a repo's agents (each name with its worktree). With
    /// `only`, that agent's pairs are checked now, settled or not, and the memory limit does
    /// not hold them back.
    pub async fn check_clashes(
        &self,
        repo: &Path,
        base: &Path,
        agents: &[(String, PathBuf)],
        settings: &Settings,
        only: Option<&str>,
    ) -> Result<()> {
        let runtime = self.runtime().await?.to_string();
        let group = self.group(repo, base, settings).await?;
        let mut round = group.round.lock().await;
        let r = &mut *round;
        let unscanned = || Tree::new(base, &settings.extra_paths);
        let (base_tree, moved) = scan(r.base.take().unwrap_or_else(unscanned)).await?;
        r.base_gen += u64::from(moved);
        r.trees.retain(|n, _| agents.iter().any(|(a, _)| a == n));
        r.last.retain(|n, _| agents.iter().any(|(a, _)| a == n));
        let mut sides = vec![];
        for (name, wt) in agents {
            let tree = match r.trees.remove(name) {
                Some(t) if t.root() == wt => t,
                _ => Tree::new(wt, &settings.extra_paths),
            };
            // A worktree being removed fails its scan; it leaves the list next round.
            let Ok((tree, _)) = scan(tree).await else {
                continue;
            };
            let l = layer(&base_tree, &tree);
            r.trees.insert(name.clone(), tree);
            let settled = r.last.insert(name.clone(), l.hash) == Some(l.hash);
            if l.changed.is_empty() && l.removed.is_empty() {
                continue;
            }
            sides.push(Side {
                name: name.clone(),
                worktree: wt.clone(),
                code: l.changed.iter().filter(|f| is_code(f)).cloned().collect(),
                layer: l,
                settled,
            });
        }
        r.base = Some(base_tree);
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
                    wanted.push((i, j, (a.layer.hash, b.layer.hash, r.base_gen)));
                }
            }
        }
        let needed: Vec<_> = {
            let mut pairs = group.pairs.lock().unwrap();
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
        if only.is_none() && group.memory().await > settings.group_memory_bytes {
            tracing::debug!("checker group above group_memory_mb; clash checks wait");
            return Ok(());
        }

        let (bridge, typescript, program_files) = {
            let mut s = group.state.lock().await;
            group.fresh(&mut s, &runtime, &self.home).await?;
            let bridge = s.bridge.clone().expect("started");
            (bridge, s.typescript.clone(), s.program_files)
        };
        let base_dep = r.base.as_ref().expect("scanned").dep_key(&runtime)?;
        let mut unshared: HashMap<&str, String> = HashMap::new();
        let mut check = BTreeSet::new();
        for &(i, j, _) in &needed {
            for side in [&sides[i], &sides[j]] {
                if unshared.contains_key(side.name.as_str()) {
                    continue;
                }
                let reason = if let Some(v) =
                    typescript_version(&side.worktree).filter(|v| *v != typescript)
                {
                    Some(format!(
                        "{}'s TypeScript {v} differs from the base's {typescript}",
                        side.name
                    ))
                } else if r.trees[&side.name].dep_key(&runtime)? != base_dep {
                    Some(format!(
                        "{}'s dependencies differ from the base's",
                        side.name
                    ))
                } else if side.layer.changed.len() as f64
                    > settings.huge_diff_ratio * program_files.max(1) as f64
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

        for (i, j, key) in needed {
            let (a, b) = (&sides[i], &sides[j]);
            let skipped = [a, b]
                .iter()
                .find_map(|s| unshared.get(s.name.as_str()).cloned());
            let pair = match skipped {
                Some(reason) => Pair {
                    agents: [a.name.clone(), b.name.clone()],
                    clashes: vec![],
                    unmerged: vec![],
                    skipped: Some(reason),
                    key,
                },
                None => {
                    for side in [a, b] {
                        alone(r, &bridge, side, &check, base).await?;
                    }
                    self.combine(r, &bridge, a, b, base, key).await?
                }
            };
            if !pair.clashes.is_empty() || !pair.unmerged.is_empty() {
                tracing::info!(a = %a.name, b = %b.name, clashes = pair.clashes.len(), unmerged = pair.unmerged.len(), "agents' changes clash");
            }
            let mut pairs = group.pairs.lock().unwrap();
            pairs.retain(|p| p.agents != pair.agents);
            pairs.push(pair);
        }
        Ok(())
    }

    /// Checks both layers at once and keeps the errors neither has alone.
    async fn combine(
        &self,
        r: &Round,
        bridge: &Bridge,
        a: &Side,
        b: &Side,
        base: &Path,
        key: (u64, u64, u64),
    ) -> Result<Pair> {
        let tmp = self
            .home
            .join("clash")
            .join(format!("{}+{}", a.name, b.name));
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
        let pair = |clashes, mut unmerged: Vec<String>| {
            unmerged.sort();
            Pair {
                agents: [a.name.clone(), b.name.clone()],
                clashes,
                unmerged,
                skipped: None,
                key,
            }
        };
        // Until someone resolves the merge there is no combination to check; picking one
        // side's version would report errors that come from the pick.
        if !unmerged.is_empty() {
            let _ = tokio::fs::remove_dir_all(&tmp).await;
            return Ok(pair(vec![], unmerged));
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
        let both = diagnose(bridge, &name, files, removed, &touched, base).await;
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        let both: Vec<Diag> = both?.into_values().flatten().collect();
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
        Ok(pair(grouped(clashes), unmerged))
    }

    /// Every pair including `agent`, from its side (see `Group::pairs_of`).
    pub async fn clashes(&self, repo: &Path, agent: &str) -> Vec<Value> {
        match self.groups.lock().await.get(repo) {
            Some(g) => g.pairs_of(agent, true),
            None => vec![],
        }
    }
}

impl Group {
    /// Pairs including `agent`, with the other one as `with`. Unless `all`, only those with
    /// clashes or files git cannot merge.
    pub(super) fn pairs_of(&self, agent: &str, all: bool) -> Vec<Value> {
        self.pairs
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.agents.iter().any(|a| a == agent))
            .filter(|p| all || !p.clashes.is_empty() || !p.unmerged.is_empty())
            .map(|p| {
                let with = if p.agents[0] == agent { &p.agents[1] } else { &p.agents[0] };
                json!({ "with": with, "clashes": p.clashes, "unmerged": p.unmerged, "skipped": p.skipped })
            })
            .collect()
    }
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
