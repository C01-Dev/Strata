//! Worktree operations through the git CLI (PLAN ground rule 8: no git library).

use std::path::{Path, PathBuf};
use std::process::Command;
use strata_core::{Error, Result};

/// Runs `git -C <dir> <args>` and returns stdout without the trailing newline.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
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

/// `main`, else `master`, else the current branch.
pub fn default_base(root: &Path) -> Result<String> {
    for branch in ["main", "master"] {
        if git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_ok()
        {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A temp repo on `main` with one commit.
    pub fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("app");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(root.join("README.md"), "hi\n").unwrap();
        git(&root, &["add", "."]).unwrap();
        git(
            &root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        )
        .unwrap();
        let root = root.canonicalize().unwrap();
        (dir, root)
    }

    #[test]
    fn repo_root_from_a_linked_worktree() {
        let (dir, root) = repo();
        let wt = dir.path().join("wt");
        git(
            &root,
            &["worktree", "add", "-q", "-b", "x", wt.to_str().unwrap()],
        )
        .unwrap();
        assert_eq!(repo_root(&wt).unwrap(), root);
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
}
