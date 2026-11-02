//! Which files differ between a worktree and the base, by content hash (not `git status`, so
//! untracked and generated files count).
//!
//! A `Tree` is a cached walk of one checkout: every file the checker may read, with its xxh3.
//! `scan` re-walks and rehashes only files whose size or mtime changed, so calling it before
//! every query is cheap.
// ponytail: a stat walk per query instead of a file watcher; add `notify` if the walk shows up in latency.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

/// Never walked: VCS data, dependencies (except `extra_paths`) and build caches.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    ".next",
    ".turbo",
    ".cache",
    "coverage",
    ".yarn",
    ".svelte-kit",
    ".vercel",
    ".parcel-cache",
];
/// Extensions the type checker reads.
const EXTENSIONS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "json"];
const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
];
const PM_CONFIG: &[&str] = &["pnpm-workspace.yaml", ".npmrc", ".yarnrc.yml", ".yarnrc"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Entry {
    size: u64,
    mtime_ns: u128,
    hash: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scan {
    pub files: usize,
    pub rehashed: usize,
    pub changed: bool,
}

pub struct Tree {
    root: PathBuf,
    extra_paths: Vec<String>,
    files: HashMap<String, Entry>,
}

/// The agent's diff against base, as paths relative to the checkout root.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Layer {
    /// Edited or added (sorted).
    pub changed: Vec<String>,
    /// In base, gone from the worktree (sorted).
    pub removed: Vec<String>,
    /// Identifies this exact layer, to skip snapshot updates when nothing changed.
    pub hash: u64,
}

impl Tree {
    /// `extra_paths` (relative, e.g. `node_modules/.prisma`) are walked even inside skipped folders.
    pub fn new(root: &Path, extra_paths: &[String]) -> Self {
        Self {
            root: root.to_path_buf(),
            extra_paths: extra_paths.to_vec(),
            files: HashMap::new(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn scan(&mut self) -> io::Result<Scan> {
        let mut seen = HashMap::with_capacity(self.files.len());
        let mut stats = Scan::default();
        self.walk(&self.root.clone(), "", false, &mut seen, &mut stats)?;
        for extra in self.extra_paths.clone() {
            let path = self.root.join(&extra);
            if path.is_dir() {
                self.walk(&path, &extra, true, &mut seen, &mut stats)?;
            } else if path.is_file() {
                self.visit(&path, extra, &mut seen, &mut stats)?;
            }
        }
        stats.files = seen.len();
        stats.changed |= seen.len() != self.files.len();
        self.files = seen;
        Ok(stats)
    }

    fn walk(
        &self,
        dir: &Path,
        rel: &str,
        in_extra: bool,
        seen: &mut HashMap<String, Entry>,
        stats: &mut Scan,
    ) -> io::Result<()> {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let child_rel = if rel.is_empty() {
                name.to_string()
            } else {
                format!("{rel}/{name}")
            };
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                let skipped = SKIP_DIRS.contains(&name) && !(in_extra && name == "node_modules");
                // A nested checkout (linked worktree or repo) has its own `.git`.
                if !skipped && !path.join(".git").exists() {
                    self.walk(&path, &child_rel, in_extra, seen, stats)?;
                }
            } else if (kind.is_file() || kind.is_symlink()) && has_checked_extension(name) {
                self.visit(&path, child_rel, seen, stats)?;
            }
        }
        Ok(())
    }

    fn visit(
        &self,
        path: &Path,
        rel: String,
        seen: &mut HashMap<String, Entry>,
        stats: &mut Scan,
    ) -> io::Result<()> {
        let Ok(meta) = fs::metadata(path) else {
            return Ok(());
        }; // broken symlink
        if !meta.is_file() {
            return Ok(());
        }
        let mtime_ns = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let entry = match self.files.get(&rel) {
            Some(old) if old.size == meta.len() && old.mtime_ns == mtime_ns => *old,
            old => {
                stats.rehashed += 1;
                let hash = xxh3_64(&fs::read(path)?);
                stats.changed |= old.is_none_or(|o| o.hash != hash);
                Entry {
                    size: meta.len(),
                    mtime_ns,
                    hash,
                }
            }
        };
        seen.insert(rel, entry);
        Ok(())
    }

    /// Dependency identity (PLAN's dep_key): lockfiles, every package.json, package-manager
    /// config, the `patches/` folder, and the runtime (Node ABI and platform). Files under
    /// `extra_paths` are excluded: they are per-worktree by design and go in the layer.
    pub fn dep_key(&self, runtime: &str) -> io::Result<String> {
        let mut parts: Vec<(String, u64)> = self
            .files
            .iter()
            .filter(|(rel, _)| {
                rel.rsplit('/').next() == Some("package.json") && !self.in_extra(rel)
            })
            .map(|(rel, e)| (rel.clone(), e.hash))
            .collect();
        for name in LOCKFILES.iter().chain(PM_CONFIG) {
            if let Ok(bytes) = fs::read(self.root.join(name)) {
                parts.push((name.to_string(), xxh3_64(&bytes)));
            }
        }
        hash_dir(&self.root.join("patches"), "patches", &mut parts)?;
        parts.sort();
        let mut h = Xxh3::new();
        h.update(runtime.as_bytes());
        for (rel, hash) in parts {
            h.update(rel.as_bytes());
            h.update(&hash.to_le_bytes());
        }
        Ok(format!("{:016x}", h.digest()))
    }

    fn in_extra(&self, rel: &str) -> bool {
        self.extra_paths
            .iter()
            .any(|x| rel == x || rel.starts_with(&format!("{x}/")))
    }
}

fn has_checked_extension(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| EXTENSIONS.contains(&ext))
}

fn hash_dir(dir: &Path, rel: &str, out: &mut Vec<(String, u64)>) -> io::Result<()> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        let child = format!("{rel}/{}", entry.file_name().to_string_lossy());
        if entry.file_type()?.is_dir() {
            hash_dir(&entry.path(), &child, out)?;
        } else {
            out.push((child, xxh3_64(&fs::read(entry.path())?)));
        }
    }
    Ok(())
}

/// Files that differ from or are missing in `base`, and base files missing from `wt`.
pub fn layer(base: &Tree, wt: &Tree) -> Layer {
    let mut changed: Vec<String> = wt
        .files
        .iter()
        .filter(|(rel, e)| base.files.get(*rel).is_none_or(|b| b.hash != e.hash))
        .map(|(rel, _)| rel.clone())
        .collect();
    let mut removed: Vec<String> = base
        .files
        .keys()
        .filter(|rel| !wt.files.contains_key(*rel))
        .cloned()
        .collect();
    changed.sort();
    removed.sort();
    let mut h = Xxh3::new();
    for rel in &changed {
        h.update(rel.as_bytes());
        h.update(&wt.files[rel].hash.to_le_bytes());
    }
    for rel in &removed {
        h.update(b"\0-");
        h.update(rel.as_bytes());
    }
    Layer {
        changed,
        removed,
        hash: h.digest(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, files: &[(&str, &str)]) {
        for (path, text) in files {
            let p = root.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
    }

    const FILES: &[(&str, &str)] = &[
        ("package.json", "{}"),
        ("tsconfig.json", "{}"),
        ("src/a.ts", "export const a = 1;\n"),
        ("src/b.tsx", "export const b = 2;\n"),
        ("src/gone.ts", "export const gone = 3;\n"),
        ("README.md", "not checked"),
        ("node_modules/x/index.d.ts", "dependency"),
    ];

    fn pair() -> (tempfile::TempDir, Tree, Tree) {
        let tmp = tempfile::tempdir().unwrap();
        for side in ["base", "wt"] {
            write(&tmp.path().join(side), FILES);
        }
        let base = Tree::new(&tmp.path().join("base"), &[]);
        let wt = Tree::new(&tmp.path().join("wt"), &[]);
        (tmp, base, wt)
    }

    #[test]
    fn identical_checkouts_have_an_empty_layer() {
        let (_tmp, mut base, mut wt) = pair();
        assert_eq!(
            base.scan().unwrap().files,
            5,
            "md and node_modules are not walked"
        );
        wt.scan().unwrap();
        let l = layer(&base, &wt);
        assert!(l.changed.is_empty() && l.removed.is_empty());
        assert_eq!(
            l.hash,
            Layer {
                hash: layer(&wt, &base).hash,
                ..Default::default()
            }
            .hash
        );
    }

    #[test]
    fn edits_additions_and_deletions() {
        let (tmp, mut base, mut wt) = pair();
        let w = tmp.path().join("wt");
        write(
            &w,
            &[
                ("src/a.ts", "export const a = 100;\n"),
                ("src/new.ts", "x"),
                ("dist/gen.d.ts", "generated, untracked"),
            ],
        );
        fs::remove_file(w.join("src/gone.ts")).unwrap();
        base.scan().unwrap();
        wt.scan().unwrap();
        let l = layer(&base, &wt);
        assert_eq!(l.changed, ["dist/gen.d.ts", "src/a.ts", "src/new.ts"]);
        assert_eq!(l.removed, ["src/gone.ts"]);
    }

    #[test]
    fn rescan_rehashes_only_what_changed() {
        let (tmp, mut base, mut wt) = pair();
        base.scan().unwrap();
        wt.scan().unwrap();
        let empty = layer(&base, &wt).hash;
        let again = wt.scan().unwrap();
        assert_eq!((again.rehashed, again.changed), (0, false));
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(
            &tmp.path().join("wt"),
            &[("src/a.ts", "export const a = 9;\n")],
        ); // same size
        let s = wt.scan().unwrap();
        assert_eq!((s.rehashed, s.changed), (1, true));
        assert_ne!(layer(&base, &wt).hash, empty);
        // Touching without changing content rehashes but reports no change.
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(
            &tmp.path().join("wt"),
            &[("src/b.tsx", "export const b = 2;\n")],
        );
        let s = wt.scan().unwrap();
        assert_eq!((s.rehashed, s.changed), (1, false));
    }

    #[test]
    fn nested_checkouts_and_dir_symlinks_are_skipped() {
        let (tmp, mut base, mut wt) = pair();
        let w = tmp.path().join("wt");
        write(
            &w,
            &[
                (".claude/worktrees/x/.git", "gitdir: elsewhere"),
                (".claude/worktrees/x/src/a.ts", "nested"),
            ],
        );
        std::os::unix::fs::symlink(w.join("src"), w.join("loop")).unwrap();
        base.scan().unwrap();
        wt.scan().unwrap();
        assert_eq!(
            layer(&base, &wt),
            Layer {
                hash: layer(&base, &base).hash,
                ..Default::default()
            }
        );
    }

    #[test]
    fn extra_paths_are_layered_inside_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        let extra = vec!["node_modules/.prisma".to_string()];
        for (side, client) in [("base", "a"), ("wt", "b")] {
            write(
                &tmp.path().join(side),
                &[
                    ("node_modules/.prisma/client/index.d.ts", client),
                    ("node_modules/.prisma/client/package.json", client),
                ],
            );
        }
        let mut base = Tree::new(&tmp.path().join("base"), &extra);
        let mut wt = Tree::new(&tmp.path().join("wt"), &extra);
        base.scan().unwrap();
        wt.scan().unwrap();
        assert_eq!(
            layer(&base, &wt).changed,
            [
                "node_modules/.prisma/client/index.d.ts",
                "node_modules/.prisma/client/package.json"
            ]
        );
        assert_eq!(
            base.dep_key("node").unwrap(),
            wt.dep_key("node").unwrap(),
            "extra paths are not dependencies"
        );
    }

    #[test]
    fn dep_key_tracks_dependencies_not_sources() {
        let (tmp, mut base, mut wt) = pair();
        base.scan().unwrap();
        let key = |t: &mut Tree| {
            t.scan().unwrap();
            t.dep_key("abi-127 darwin-arm64").unwrap()
        };
        let k0 = key(&mut wt);
        assert_eq!(k0, base.dep_key("abi-127 darwin-arm64").unwrap());
        assert_ne!(
            k0,
            wt.dep_key("abi-131 darwin-arm64").unwrap(),
            "runtime is part of the key"
        );
        let w = tmp.path().join("wt");
        write(&w, &[("src/a.ts", "source edits do not matter")]);
        assert_eq!(key(&mut wt), k0);
        for (file, text) in [
            ("packages/ui/package.json", "{\"name\":\"ui\"}"),
            ("pnpm-lock.yaml", "lock"),
            (".npmrc", "x=1"),
            ("patches/react.patch", "diff"),
        ] {
            let before = key(&mut wt);
            write(&w, &[(file, text)]);
            assert_ne!(key(&mut wt), before, "{file}");
        }
    }
}
