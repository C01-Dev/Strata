//! Helpers for end-to-end tests: a temp repo, a temp STRATA_HOME, and the built binary.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// A repo `app` on `main` with one commit, plus `files`.
pub fn env(files: &[(&str, &str)]) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("app");
    std::fs::create_dir(&root).unwrap();
    git(&root, &["init", "-q", "-b", "main"]);
    for (path, text) in [("README.md", "hi\n")].iter().chain(files) {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "-q", "-m", "init"]);
    let root = root.canonicalize().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("home");
    Env { tmp, root, home }
}

impl Env {
    /// Runs `strata <args>` in `dir` with this env's STRATA_HOME.
    pub fn strata_in(&self, dir: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_strata"))
            .args(args)
            .current_dir(dir)
            .env("STRATA_HOME", &self.home)
            .output()
            .unwrap()
    }

    /// Runs `strata <args>` in the repo root and returns stdout; panics on failure.
    pub fn strata(&self, args: &[&str]) -> String {
        let out = self.strata_in(&self.root, args);
        assert!(
            out.status.success(),
            "strata {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Runs `strata <args>` expecting failure; returns stderr.
    pub fn strata_err(&self, args: &[&str]) -> String {
        let out = self.strata_in(&self.root, args);
        assert!(
            !out.status.success(),
            "strata {args:?} should fail; stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        String::from_utf8_lossy(&out.stderr).into_owned()
    }
}
