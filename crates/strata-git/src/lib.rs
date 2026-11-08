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

/// `worktree list --porcelain -z` needs git 2.36.
pub fn check_version() -> Result<()> {
    let out = Command::new("git").arg("--version").output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut nums = text
        .split_whitespace()
        .nth(2)
        .unwrap_or("")
        .split('.')
        .map(|n| n.parse::<u32>().unwrap_or(0));
    let version = (nums.next().unwrap_or(0), nums.next().unwrap_or(0));
    if version < (2, 36) {
        return Err(Error::Refused(format!(
            "Strata needs git 2.36 or later; found {}",
            text.trim()
        )));
    }
    Ok(())
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
