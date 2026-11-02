//! Helpers for end-to-end tests: a temp repo, a temp STRATA_HOME, and the built binary.
//! Git runs without the user's global or system config, so signing or hooks there cannot interfere.

#![allow(dead_code)]

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

pub struct Env {
    pub tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
}

fn isolated(mut cmd: Command) -> Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    cmd
}

pub fn git_out(dir: &Path, args: &[&str]) -> Output {
    let mut cmd = isolated(Command::new("git"));
    cmd.arg("-C").arg(dir).args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "-c",
        "protocol.file.allow=always",
    ]);
    cmd.args(args).output().unwrap()
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = git_out(dir, args);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

pub fn write_file(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
}

pub fn commit_file(dir: &Path, file: &str, text: &str) {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
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

/// A TypeScript repo (TypeScript 7.1 linked from packages/tsbridge, ignored by git) plus
/// `strata init`.
pub fn ts_initialized() -> Env {
    let e = env(&[
        (".gitignore", "node_modules\n"),
        ("package.json", r#"{"name":"app","private":true}"#),
        (
            "tsconfig.json",
            r#"{ "compilerOptions": { "strict": true, "target": "es2022", "module": "esnext", "moduleResolution": "bundler" }, "include": ["src"] }"#,
        ),
        (
            "src/a.ts",
            "/** Adds one. */\nexport function addOne(n: number): number { return n + 1; }\n",
        ),
        (
            "src/b.ts",
            "import { addOne } from \"./a\";\nexport const total = addOne(2);\n",
        ),
    ]);
    let ts = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/tsbridge/node_modules/typescript")
        .canonicalize()
        .expect("run `npm ci --prefix packages/tsbridge`");
    std::fs::create_dir(e.root.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(ts, e.root.join("node_modules/typescript")).unwrap();
    e.strata(&["init", "--no-service"]);
    own_ports(&e);
    e
}

/// `env` plus `strata init` (daemon running, base worktree created).
pub fn initialized() -> Env {
    let e = env(&[]);
    e.strata(&["init", "--no-service"]);
    own_ports(&e);
    e
}

/// Sets `dev.port_range` and `dev.proxy_port` in the repo's strata.toml (tests run in parallel,
/// so each picks its own range).
pub fn set_ports(e: &Env, lo: u16, hi: u16, proxy: u16) {
    let config = std::fs::read_to_string(e.root.join("strata.toml")).unwrap();
    let config: Vec<String> = config
        .lines()
        .map(|l| match l.split_whitespace().next() {
            Some("port_range") => format!("port_range = [{lo}, {hi}]"),
            Some("proxy_port") => format!("proxy_port = {proxy}"),
            _ => l.to_string(),
        })
        .collect();
    std::fs::write(e.root.join("strata.toml"), config.join("\n") + "\n").unwrap();
}

/// Each initialized test repo gets its own port range and proxy port: tests in one binary run
/// in parallel, each with its own daemon (binaries run one after another).
fn own_ports(e: &Env) {
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(30000);
    let lo = NEXT.fetch_add(100, std::sync::atomic::Ordering::SeqCst);
    set_ports(e, lo, lo + 49, lo + 50);
}

/// Starts a dev process through the API (`kind: dev`); returns the response.
pub fn start_dev(e: &Env, agent: &str, cmd: &str) -> Value {
    let body = serde_json::json!({ "repo": e.root, "kind": "dev", "cmd": cmd }).to_string();
    let out = Command::new("curl")
        .args(["-s", "--unix-socket"])
        .arg(e.home.join("strata.sock"))
        .args([
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "-d",
            &body,
            &format!("http://strata/v1/agents/{agent}/procs"),
        ])
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
}

/// A Node HTTP server answering `text`, on `$PORT` unless `port` is given.
pub fn node_server(text: &str, port: Option<u16>) -> String {
    let port = port.map_or("process.env.PORT".to_string(), |p| p.to_string());
    format!("node -e \"require('http').createServer((q, s) => s.end('{text}')).listen({port})\"")
}

/// Polls `check` every 100 ms for up to 10 s.
pub fn wait_until(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

impl Drop for Env {
    /// Never leave a test daemon or a supervised process running.
    fn drop(&mut self) {
        if let Ok(status) = serde_json::from_slice::<Value>(
            &self
                .strata_in(&self.root.clone(), &["status", "--json"])
                .stdout,
        ) {
            for repo in status["repos"].as_array().into_iter().flatten() {
                for a in repo["agents"].as_array().into_iter().flatten() {
                    for p in a["processes"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|p| p["state"] == "running")
                    {
                        let _ = Command::new("kill")
                            .args(["-9", &format!("-{}", p["pid"])])
                            .status();
                    }
                }
            }
        }
        let _ = self.strata_in(&self.root.clone(), &["daemon", "stop"]);
    }
}

impl Env {
    pub fn strata_in(&self, dir: &Path, args: &[&str]) -> Output {
        self.strata_with(dir, args, &[])
    }

    pub fn strata_with(&self, dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut cmd = isolated(Command::new(env!("CARGO_BIN_EXE_strata")));
        cmd.args(args)
            .current_dir(dir)
            .env("STRATA_HOME", &self.home)
            .envs(envs.iter().copied())
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

    /// Where `strata new <name>` puts a worktree (`../.strata/app/<name>`).
    pub fn wt(&self, name: &str) -> PathBuf {
        self.root.parent().unwrap().join(".strata/app").join(name)
    }

    pub fn status(&self) -> Value {
        serde_json::from_str(&self.strata(&["status", "--json"])).unwrap()
    }

    /// The agent's status entry, if it is registered.
    pub fn agent(&self, name: &str) -> Option<Value> {
        let s = self.status();
        s["repos"][0]["agents"]
            .as_array()?
            .iter()
            .find(|a| a["name"] == name)
            .cloned()
    }

    /// `git worktree list` entries as (path, locked).
    pub fn worktrees(&self) -> Vec<(PathBuf, bool)> {
        let out = git(&self.root, &["worktree", "list", "--porcelain"]);
        let mut all: Vec<(PathBuf, bool)> = vec![];
        for line in out.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                all.push((
                    PathBuf::from(p).canonicalize().unwrap_or(PathBuf::from(p)),
                    false,
                ));
            } else if line.starts_with("locked") {
                all.last_mut().unwrap().1 = true;
            }
        }
        all
    }

    pub fn is_worktree(&self, path: &Path) -> bool {
        let path = path.canonicalize().unwrap_or(path.to_path_buf());
        self.worktrees().iter().any(|(p, _)| *p == path)
    }

    pub fn is_locked(&self, path: &Path) -> bool {
        let path = path.canonicalize().unwrap();
        self.worktrees()
            .iter()
            .any(|(p, locked)| *p == path && *locked)
    }
}
