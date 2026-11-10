//! Worktree operations through the git CLI (PLAN ground rule 8: no git library).
//! Callers serialize these: the daemon holds one git lock around every operation.

use std::path::{Path, PathBuf};
use std::process::Command;
use strata_core::{Error, Result};

/// Runs `git -C <dir> <args>` and returns stdout without the trailing newline.
/// `LC_ALL=C` keeps stderr in English, since a few callers match on it.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("LC_ALL", "C")
        .output()?;
    if !out.status.success() {
        return Err(Error::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .trim_end_matches('\n')
        .to_string())
}

fn utf8(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Error::Refused(format!("{} is not valid UTF-8", path.display())))
}

/// (major, minor) and the whole `git --version` line.
fn version() -> Result<((u32, u32), String)> {
    let out = Command::new("git").arg("--version").output()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut nums = text
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .split('.')
        .map(|n| n.parse::<u32>().unwrap_or(0));
    Ok(((nums.next().unwrap_or(0), nums.next().unwrap_or(0)), text))
}

/// `worktree list --porcelain -z` needs git 2.36.
pub fn check_version() -> Result<()> {
    let (version, text) = version()?;
    if version < (2, 36) {
        return Err(Error::Refused(format!(
            "Strata needs git 2.36 or later; found {text}"
        )));
    }
    Ok(())
}

/// Whether this git can merge two trees without a checkout (`merge-tree --write-tree`, git
/// 2.38), which the clash checks between agents use (see `Shadow`). Asked once per process.
pub fn merges_trees() -> bool {
    static CAN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CAN.get_or_init(|| version().is_ok_and(|(v, _)| v >= (2, 38)))
}

/// What merging two agents' work gives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Merge {
    /// The merged tree's id.
    Clean(String),
    /// The files git cannot merge (sorted).
    Unmerged(Vec<String>),
}

/// Strata's own git state for one repo, for looking at worktrees without touching them: a
/// folder of private index files, and a private object folder that reads the repo's own objects
/// as alternates. Nothing is written into the repo, and no worktree's own index changes, so
/// agents' `git status`, staging and commits are unaffected.
pub struct Shadow {
    dir: PathBuf,
    /// The repo's own object folder.
    alternates: PathBuf,
}

impl Shadow {
    /// `dir` is Strata's folder for this repo; it is created when first used.
    pub fn open(dir: &Path, repo: &Path) -> Result<Self> {
        let common = git(
            repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        Ok(Self {
            dir: dir.to_path_buf(),
            alternates: Path::new(&common).join("objects"),
        })
    }

    /// The private index of the worktree called `name`.
    pub fn index(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.index"))
    }

    /// `git -C <cwd> <args>` reading and writing Strata's objects, and `index` if given.
    fn run(&self, cwd: &Path, index: Option<&Path>, args: &[&str]) -> Result<std::process::Output> {
        let objects = self.dir.join("objects");
        // git calls a missing object folder "not a git repository".
        std::fs::create_dir_all(&objects)?;
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(cwd)
            .args(args)
            .env("LC_ALL", "C")
            .env("GIT_OBJECT_DIRECTORY", objects)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &self.alternates);
        if let Some(index) = index {
            cmd.env("GIT_INDEX_FILE", index);
        }
        // `commit-tree` needs an identity, and a fixed one (with a fixed date) gives the same
        // commit for the same tree and parent.
        for who in ["AUTHOR", "COMMITTER"] {
            cmd.env(format!("GIT_{who}_NAME"), "strata")
                .env(format!("GIT_{who}_EMAIL"), "strata@localhost")
                .env(format!("GIT_{who}_DATE"), "1700000000 +0000");
        }
        Ok(cmd.output()?)
    }

    fn ok(&self, cwd: &Path, index: Option<&Path>, args: &[&str]) -> Result<String> {
        let out = self.run(cwd, index, args)?;
        if !out.status.success() {
            return Err(failed(args, &out));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
    }

    /// The worktree's files as a git tree: everything tracked or untracked that is not ignored,
    /// whether committed, staged or neither. Returns the tree's id. `index` is this worktree's
    /// private index (see `index`); it is made on first use and makes later calls cheap, since
    /// git rehashes only files whose stat changed.
    pub fn tree(&self, worktree: &Path, index: &Path) -> Result<String> {
        let tree = self.write_tree(worktree, index)?;
        if self
            .run(worktree, None, &["cat-file", "-e", &tree])?
            .status
            .success()
        {
            return Ok(tree);
        }
        // The index outlived its objects (the object folder was emptied): `write-tree` answered
        // from the index's own record of a tree that is gone. Start over.
        std::fs::remove_file(index)?;
        self.write_tree(worktree, index)
    }

    fn write_tree(&self, worktree: &Path, index: &Path) -> Result<String> {
        if !index.exists() {
            self.ok(worktree, Some(index), &["read-tree", "HEAD"])?;
        }
        self.ok(worktree, Some(index), &["add", "-A"])?;
        self.ok(worktree, Some(index), &["write-tree"])
    }

    /// A commit of `tree` on top of the worktree's `HEAD`, so a merge of two worktrees finds
    /// their real common ancestor. The same tree on the same `HEAD` gives the same commit.
    pub fn commit(&self, worktree: &Path, tree: &str) -> Result<String> {
        self.ok(
            worktree,
            None,
            &["commit-tree", tree, "-p", "HEAD", "-m", "strata"],
        )
    }

    /// Three-way merge of two commits made by `commit`, with git's rename detection, without
    /// a checkout. Needs `merges_trees()`.
    pub fn merge(&self, repo: &Path, a: &str, b: &str) -> Result<Merge> {
        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "--allow-unrelated-histories",
            "-z",
            a,
            b,
        ];
        let out = self.run(repo, None, &args)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut fields = text.split('\0').filter(|f| !f.is_empty());
        match (out.status.code(), fields.next()) {
            (Some(0), Some(tree)) => Ok(Merge::Clean(tree.to_string())),
            (Some(1), Some(_)) => {
                let mut files: Vec<String> = fields.map(String::from).collect();
                files.sort();
                files.dedup();
                Ok(Merge::Unmerged(files))
            }
            _ => Err(failed(&args, &out)),
        }
    }
}

fn failed(args: &[&str], out: &std::process::Output) -> Error {
    Error::Git {
        args: args.join(" "),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    }
}

/// The tree id of `dir`'s `HEAD` commit.
pub fn head_tree(dir: &Path) -> Result<String> {
    git(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{tree}"])
}

/// Root of the main checkout that owns `dir`, also when `dir` is inside a linked worktree.
pub fn repo_root(dir: &Path) -> Result<PathBuf> {
    let common = PathBuf::from(git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    match (common.file_name(), common.parent()) {
        (Some(name), Some(root)) if name == ".git" => Ok(root.canonicalize()?),
        _ => Err(Error::Refused(format!(
            "{} is a bare repository; Strata needs a checkout",
            common.display()
        ))),
    }
}

/// Top folder of the checkout `dir` is in.
pub fn toplevel(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(dir, &["rev-parse", "--show-toplevel"])?).canonicalize()?)
}

/// `main`, else `master`, else the current branch.
pub fn default_base(root: &Path) -> Result<String> {
    for branch in ["main", "master"] {
        if branch_exists(root, branch) {
            return Ok(branch.into());
        }
    }
    git(root, &["symbolic-ref", "--short", "HEAD"]).map_err(|_| {
        Error::Refused(
            "HEAD is detached and there is no main or master branch; set repo.base in strata.toml"
                .into(),
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: String,
    /// Short name; `None` when detached.
    pub branch: Option<String>,
    /// The lock reason (`Some("")` when locked without one); `None` when not locked.
    pub locked: Option<String>,
}

pub fn list(root: &Path) -> Result<Vec<Worktree>> {
    Ok(parse_list(&git(
        root,
        &["worktree", "list", "--porcelain", "-z"],
    )?))
}

fn parse_list(out: &str) -> Vec<Worktree> {
    let mut all = vec![];
    let mut cur: Option<Worktree> = None;
    for field in out.split('\0') {
        let (key, val) = field.split_once(' ').unwrap_or((field, ""));
        match (key, cur.as_mut()) {
            ("", _) => all.extend(cur.take()),
            ("worktree", _) => {
                all.extend(cur.take());
                cur = Some(Worktree {
                    path: val.into(),
                    head: String::new(),
                    branch: None,
                    locked: None,
                });
            }
            ("HEAD", Some(w)) => w.head = val.into(),
            ("branch", Some(w)) => {
                w.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).into())
            }
            ("locked", Some(w)) => w.locked = Some(val.into()),
            _ => {}
        }
    }
    all.extend(cur);
    all
}

/// Whether two paths name the same folder (git records paths as given; macOS temp
/// folders, for one, have a symlinked prefix).
pub fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// `git worktree add --lock`: on a new branch from `from`, or on the existing `branch` when `from` is `None`.
pub fn add(root: &Path, path: &Path, branch: &str, from: Option<&str>) -> Result<()> {
    let p = utf8(path)?;
    match from {
        Some(from) => git(
            root,
            &["worktree", "add", "--lock", "-q", "-b", branch, p, from],
        ),
        None => git(root, &["worktree", "add", "--lock", "-q", p, branch]),
    }
    .map(drop)
}

pub fn add_detached(root: &Path, path: &Path, rev: &str) -> Result<()> {
    git(
        root,
        &[
            "worktree",
            "add",
            "--lock",
            "-q",
            "--detach",
            utf8(path)?,
            rev,
        ],
    )
    .map(drop)
}

/// Like `add_detached`, locked with `reason` (the warm pool keeps its state there).
pub fn add_detached_locked(root: &Path, path: &Path, rev: &str, reason: &str) -> Result<()> {
    git(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            "--lock",
            "--reason",
            reason,
            utf8(path)?,
            rev,
        ],
    )
    .map(drop)
}

/// Replaces the lock (and its reason).
pub fn relock(root: &Path, path: &Path, reason: &str) -> Result<()> {
    unlock(root, path)?;
    git(root, &["worktree", "lock", "--reason", reason, utf8(path)?]).map(drop)
}

/// `git worktree move` (the worktree must be unlocked).
pub fn move_worktree(root: &Path, from: &Path, to: &Path) -> Result<()> {
    git(root, &["worktree", "move", utf8(from)?, utf8(to)?]).map(drop)
}

/// Checks out `branch` in the worktree: new from `from`, or the existing one when `from` is `None`.
pub fn switch(path: &Path, branch: &str, from: Option<&str>) -> Result<()> {
    match from {
        Some(from) => git(path, &["switch", "-q", "-c", branch, from]),
        None => git(path, &["switch", "-q", branch]),
    }
    .map(drop)
}

/// Paths that differ between two commits.
pub fn changed_between(dir: &Path, a: &str, b: &str) -> Result<Vec<String>> {
    Ok(git(dir, &["diff", "--name-only", "-z", a, b])?
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect())
}

pub fn lock(root: &Path, path: &Path) -> Result<()> {
    match git(root, &["worktree", "lock", utf8(path)?]) {
        Err(Error::Git { stderr, .. }) if stderr.contains("is already locked") => Ok(()),
        r => r.map(drop),
    }
}

pub fn unlock(root: &Path, path: &Path) -> Result<()> {
    match git(root, &["worktree", "unlock", utf8(path)?]) {
        Err(Error::Git { stderr, .. }) if stderr.contains("is not locked") => Ok(()),
        r => r.map(drop),
    }
}

/// Unlock, then `git worktree remove` (`--force` only when asked). Re-locks if removal fails.
pub fn remove(root: &Path, path: &Path, force: bool) -> Result<()> {
    unlock(root, path)?;
    let p = utf8(path)?;
    let args: &[&str] = if force {
        &["worktree", "remove", "--force", p]
    } else {
        &["worktree", "remove", p]
    };
    git(root, args).map(drop).inspect_err(|_| {
        let _ = lock(root, path);
    })
}

/// The folder was deleted by hand: unlock its record, then `git worktree prune`.
/// Idempotent: a record that is already pruned is fine.
pub fn forget(root: &Path, path: &Path) -> Result<()> {
    match unlock(root, path) {
        Err(Error::Git { stderr, .. }) if stderr.contains("is not a working tree") => {}
        r => r?,
    }
    git(root, &["worktree", "prune"]).map(drop)
}

/// Modified, staged and untracked (not ignored) files.
pub fn changes(path: &Path) -> Result<usize> {
    Ok(git(path, &["status", "--porcelain"])?.lines().count())
}

/// Adds `pattern` to the repo's shared `info/exclude`, which every worktree reads, unless it is
/// there already.
pub fn exclude(dir: &Path, pattern: &str) -> Result<()> {
    let file = PathBuf::from(git(dir, &["rev-parse", "--git-path", "info/exclude"])?);
    let file = dir.join(file); // relative to `dir` unless git printed an absolute path
    let old = std::fs::read_to_string(&file).unwrap_or_default();
    if old.lines().any(|l| l.trim() == pattern) {
        return Ok(());
    }
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let sep = if old.is_empty() || old.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    std::fs::write(&file, format!("{old}{sep}{pattern}\n"))?;
    Ok(())
}

pub fn branch_exists(root: &Path, branch: &str) -> bool {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// `git branch -d`: refuses unless the branch is merged, so no commit is lost.
pub fn delete_merged_branch(root: &Path, branch: &str) -> Result<()> {
    git(root, &["branch", "-d", branch]).map(drop)
}

/// `None` when HEAD is detached.
pub fn current_branch(path: &Path) -> Result<Option<String>> {
    Ok(git(path, &["symbolic-ref", "--short", "-q", "HEAD"]).ok())
}

/// Commit id of `rev`.
pub fn resolve(dir: &Path, rev: &str) -> Result<String> {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
}

pub fn checkout_detached(path: &Path, commit: &str) -> Result<()> {
    git(path, &["checkout", "-q", "--detach", commit]).map(drop)
}

/// Submodule paths that are not checked out in this worktree.
pub fn uninitialized_submodules(path: &Path) -> Result<Vec<String>> {
    if !path.join(".gitmodules").exists() {
        return Ok(vec![]);
    }
    let out = git(path, &["submodule", "status"])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            l.strip_prefix('-')?
                .split_whitespace()
                .nth(1)
                .map(String::from)
        })
        .collect())
}

/// Ignored files and outermost ignored folders (folders end in `/`), relative to `dir`.
pub fn ignored(dir: &Path) -> Result<Vec<String>> {
    let out = git(
        dir,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )?;
    Ok(out
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(String::from)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp repo `app` on `main` with one commit.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(root.join("README.md"), "hi\n").unwrap();
        git(&root, &["add", "."]).unwrap();
        commit(&root);
        let root = root.canonicalize().unwrap();
        (dir, root)
    }

    fn commit(dir: &Path) {
        let args = [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            "c",
        ];
        git(dir, &args).unwrap();
    }

    #[test]
    fn repo_root_from_a_linked_worktree() {
        let (dir, root) = repo();
        let wt = dir.path().join("wt");
        add(&root, &wt, "x", Some("main")).unwrap();
        assert_eq!(repo_root(&wt).unwrap(), root);
        assert_eq!(toplevel(&wt).unwrap(), wt.canonicalize().unwrap());
    }

    #[test]
    fn default_base_prefers_main_then_master_then_head() {
        let (_dir, root) = repo();
        assert_eq!(default_base(&root).unwrap(), "main");
        git(&root, &["branch", "-m", "main", "trunk"]).unwrap();
        assert_eq!(default_base(&root).unwrap(), "trunk");
        git(&root, &["branch", "master"]).unwrap();
        assert_eq!(default_base(&root).unwrap(), "master");
    }

    #[test]
    fn git_errors_carry_stderr() {
        let dir = tempfile::tempdir().unwrap();
        let err = repo_root(dir.path()).unwrap_err().to_string();
        assert!(err.contains("not a git repository"), "{err}");
    }

    #[test]
    fn version_is_new_enough() {
        check_version().unwrap();
    }

    #[test]
    fn parses_porcelain_z() {
        let out = "worktree /a b\0HEAD 1111\0branch refs/heads/main\0\0worktree /w\0HEAD 2222\0detached\0locked\0\0worktree /x\0HEAD 3333\0branch refs/heads/x\0locked because\0prunable gitdir file points to non-existent location\0\0";
        let list = parse_list(out);
        assert_eq!(list.len(), 3);
        assert_eq!(
            list[0],
            Worktree {
                path: "/a b".into(),
                head: "1111".into(),
                branch: Some("main".into()),
                locked: None
            }
        );
        assert_eq!(
            (list[1].branch.as_deref(), list[1].locked.as_deref()),
            (None, Some(""))
        );
        assert_eq!(
            (list[2].branch.as_deref(), list[2].locked.as_deref()),
            (Some("x"), Some("because"))
        );
    }

    #[test]
    fn add_lock_remove_cycle() {
        let (dir, root) = repo();
        let wt = dir.path().join("wt");
        add(&root, &wt, "feature", Some("main")).unwrap();
        let entry = list(&root)
            .unwrap()
            .into_iter()
            .find(|w| same_path(&w.path, &wt))
            .unwrap();
        assert!(entry.locked.is_some());
        assert_eq!(entry.branch.as_deref(), Some("feature"));
        lock(&root, &wt).unwrap(); // already locked is fine
        assert_eq!(changes(&wt).unwrap(), 0);
        std::fs::write(wt.join("new.txt"), "x").unwrap();
        assert_eq!(changes(&wt).unwrap(), 1);
        assert!(
            remove(&root, &wt, false).is_err(),
            "dirty worktree needs force"
        );
        assert!(
            list(&root)
                .unwrap()
                .iter()
                .any(|w| same_path(&w.path, &wt) && w.locked.is_some()),
            "re-locked after a failed remove"
        );
        remove(&root, &wt, true).unwrap();
        assert!(!wt.exists());
        delete_merged_branch(&root, "feature").unwrap();
        assert!(!branch_exists(&root, "feature"));
    }

    #[test]
    fn existing_branch_and_detached() {
        let (dir, root) = repo();
        git(&root, &["branch", "old"]).unwrap();
        add(&root, &dir.path().join("a"), "old", None).unwrap();
        assert_eq!(
            current_branch(&dir.path().join("a")).unwrap().as_deref(),
            Some("old")
        );
        let base = dir.path().join("base");
        add_detached(&root, &base, "main").unwrap();
        assert_eq!(current_branch(&base).unwrap(), None);
        assert_eq!(
            resolve(&base, "HEAD").unwrap(),
            resolve(&root, "main").unwrap()
        );
    }

    #[test]
    fn unmerged_branch_is_not_deleted() {
        let (dir, root) = repo();
        let wt = dir.path().join("wt");
        add(&root, &wt, "work", Some("main")).unwrap();
        std::fs::write(wt.join("f"), "x").unwrap();
        git(&wt, &["add", "."]).unwrap();
        commit(&wt);
        remove(&root, &wt, false).unwrap();
        assert!(delete_merged_branch(&root, "work").is_err());
        assert!(branch_exists(&root, "work"));
    }

    #[test]
    fn forget_prunes_a_hand_deleted_locked_worktree() {
        let (dir, root) = repo();
        let wt = dir.path().join("wt");
        add(&root, &wt, "gone", Some("main")).unwrap();
        std::fs::remove_dir_all(&wt).unwrap();
        git(&root, &["worktree", "prune"]).unwrap();
        assert_eq!(
            list(&root).unwrap().len(),
            2,
            "a locked record survives a stray prune"
        );
        forget(&root, &wt).unwrap();
        assert_eq!(list(&root).unwrap().len(), 1);
        forget(&root, &wt).unwrap(); // already gone is fine
    }

    /// Files under the repo's own `.git/objects`.
    fn objects(root: &Path) -> usize {
        fn count(dir: &Path) -> usize {
            std::fs::read_dir(dir)
                .unwrap()
                .map(|e| e.unwrap())
                .map(|e| match e.file_type().unwrap().is_dir() {
                    true => count(&e.path()),
                    false => 1,
                })
                .sum()
        }
        count(&root.join(".git/objects"))
    }

    fn files(shadow: &Shadow, root: &Path, tree: &str) -> Vec<String> {
        let out = shadow.ok(root, None, &["ls-tree", "-r", "--name-only", tree]);
        out.unwrap().lines().map(String::from).collect()
    }

    #[test]
    fn a_worktree_as_a_tree_touches_neither_it_nor_the_repo() {
        let (dir, root) = repo();
        let wt = dir.path().join("wt");
        add(&root, &wt, "x", Some("main")).unwrap();
        let shadow = Shadow::open(&dir.path().join("shadow"), &root).unwrap();
        let index = shadow.index("x");
        assert_eq!(
            shadow.tree(&wt, &index).unwrap(),
            head_tree(&root).unwrap(),
            "an untouched worktree is the base's tree"
        );

        // Committed, staged, unstaged, untracked, deleted and ignored.
        std::fs::write(wt.join("committed.txt"), "1").unwrap();
        git(&wt, &["add", "committed.txt"]).unwrap();
        commit(&wt);
        std::fs::write(wt.join("staged.txt"), "2").unwrap();
        git(&wt, &["add", "staged.txt"]).unwrap();
        std::fs::write(wt.join("untracked.txt"), "3").unwrap();
        std::fs::remove_file(wt.join("README.md")).unwrap();
        exclude(&wt, "/ignored.txt").unwrap();
        std::fs::write(wt.join("ignored.txt"), "4").unwrap();
        let (status, before) = (
            git(&wt, &["status", "--porcelain"]).unwrap(),
            objects(&root),
        );

        let tree = shadow.tree(&wt, &index).unwrap();
        assert_eq!(
            files(&shadow, &root, &tree),
            ["committed.txt", "staged.txt", "untracked.txt"]
        );
        assert_eq!(shadow.tree(&wt, &index).unwrap(), tree);
        let commit = shadow.commit(&wt, &tree).unwrap();
        assert_eq!(shadow.commit(&wt, &tree).unwrap(), commit);
        assert_eq!(
            git(&wt, &["status", "--porcelain"]).unwrap(),
            status,
            "the worktree's own index is as it was"
        );
        assert_eq!(objects(&root), before, "nothing written into the repo");
        assert!(
            git(&root, &["cat-file", "-e", &commit]).is_err(),
            "the commit exists only in Strata's objects"
        );

        // An emptied object folder with a kept index: the objects are written again.
        std::fs::remove_dir_all(dir.path().join("shadow/objects")).unwrap();
        assert_eq!(shadow.tree(&wt, &index).unwrap(), tree);
        assert_eq!(files(&shadow, &root, &tree).len(), 3);
    }

    #[test]
    fn two_worktrees_merge_or_name_the_files_that_cannot() {
        if !merges_trees() {
            return; // git older than 2.38
        }
        let (dir, root) = repo();
        std::fs::write(root.join("f.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(root.join("old.txt"), "a\nb\nc\nd\ne\nf\n").unwrap();
        git(&root, &["add", "."]).unwrap();
        commit(&root);
        let shadow = Shadow::open(&dir.path().join("shadow"), &root).unwrap();
        let agent = |name: &str, edit: &dyn Fn(&Path)| {
            let wt = dir.path().join(name);
            add(&root, &wt, name, Some("main")).unwrap();
            edit(&wt);
            let tree = shadow.tree(&wt, &shadow.index(name)).unwrap();
            shadow.commit(&wt, &tree).unwrap()
        };
        let write = |file: &'static str, text: &'static str| {
            move |wt: &Path| std::fs::write(wt.join(file), text).unwrap()
        };
        let top = agent("top", &write("f.txt", "ONE\ntwo\nthree\n"));
        let bottom = agent("bottom", &write("f.txt", "one\ntwo\nTHREE\n"));
        let other = agent("other", &write("f.txt", "uno\ntwo\nthree\n"));
        let moved = agent("moved", &|wt| {
            std::fs::rename(wt.join("old.txt"), wt.join("new.txt")).unwrap()
        });
        let edited = agent("edited", &write("old.txt", "a\nb\nc\nd\ne\nF\n"));

        let Merge::Clean(tree) = shadow.merge(&root, &top, &bottom).unwrap() else {
            panic!("edits to different lines merge");
        };
        let merged = shadow.ok(&root, None, &["cat-file", "-p", &format!("{tree}:f.txt")]);
        assert_eq!(merged.unwrap(), "ONE\ntwo\nTHREE");
        assert_eq!(
            shadow.merge(&root, &top, &other).unwrap(),
            Merge::Unmerged(vec!["f.txt".into()])
        );
        // A rename in one and an edit of the old path in the other: git follows the rename.
        let Merge::Clean(tree) = shadow.merge(&root, &moved, &edited).unwrap() else {
            panic!("a rename and an edit merge");
        };
        assert_eq!(
            files(&shadow, &root, &tree),
            ["README.md", "f.txt", "new.txt"]
        );
        let followed = shadow.ok(&root, None, &["cat-file", "-p", &format!("{tree}:new.txt")]);
        assert!(followed.unwrap().ends_with('F'));
        assert!(shadow.merge(&root, &top, "0000").is_err());
    }

    #[test]
    fn exclude_from_a_worktree_hides_the_file_in_every_checkout_once() {
        let (dir, root) = repo();
        let wt = dir.path().join("wt");
        add(&root, &wt, "feat", Some("main")).unwrap();
        for d in [&root, &wt] {
            std::fs::create_dir_all(d.join(".claude/rules")).unwrap();
            std::fs::write(d.join(".claude/rules/strata.md"), "note\n").unwrap();
        }
        assert_eq!(changes(&wt).unwrap(), 1);
        exclude(&wt, "/.claude/rules/strata.md").unwrap();
        exclude(&wt, "/.claude/rules/strata.md").unwrap();
        assert_eq!(changes(&wt).unwrap(), 0);
        assert_eq!(changes(&root).unwrap(), 0);
        let shared = std::fs::read_to_string(root.join(".git/info/exclude")).unwrap();
        assert_eq!(shared.matches("/.claude/rules/strata.md").count(), 1);
    }
}
