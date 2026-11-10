//! `strata doctor` (PLAN 5.1): checks what Strata depends on and says what to do about each.
//! `--report` prints the same as a Markdown table with home, user and repo paths redacted, to
//! paste into an issue.

use crate::daemon::MAX_SOCKET_PATH;
use serde_json::Value;
use std::net::{TcpListener, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::Command;
use strata_core::config::Config;
use strata_core::{Result, client, detect, strata_home};

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Check {
    name: &'static str,
    level: Level,
    detail: String,
    hint: Option<String>,
}

fn check(name: &'static str, level: Level, detail: impl Into<String>) -> Check {
    Check {
        name,
        level,
        detail: detail.into(),
        hint: None,
    }
}

impl Check {
    fn hint(mut self, hint: impl Into<String>) -> Check {
        self.hint = Some(hint.into());
        self
    }
}

pub fn run(report: bool) -> Result<()> {
    let checks = checks();
    if report {
        print_report(&checks);
    } else {
        for c in &checks {
            let level = match c.level {
                Level::Ok => "ok  ",
                Level::Warn => "warn",
                Level::Fail => "FAIL",
            };
            println!("{level}  {:<11} {}", c.name, c.detail);
            if let Some(h) = &c.hint {
                println!("{:17}{h}", "");
            }
        }
    }
    if checks.iter().any(|c| c.level == Level::Fail) {
        std::process::exit(1);
    }
    Ok(())
}

fn checks() -> Vec<Check> {
    let status = client::get("/v1/status").ok();
    let mut out = vec![daemon(status.as_ref()), git(), node(), socket()];
    match strata_git::repo_root(&std::env::current_dir().unwrap_or_default()) {
        Ok(root) => out.extend(repo(&root, status.as_ref())),
        Err(_) => out.push(check(
            "repo",
            Level::Warn,
            "not inside a git repo; run doctor in one for the repo checks",
        )),
    }
    if cfg!(target_os = "linux") {
        out.extend([kernel(), cgroup(status.as_ref()), inotify()]);
    }
    out
}

fn daemon(status: Option<&Value>) -> Check {
    let cli = env!("CARGO_PKG_VERSION");
    match status {
        None => check("daemon", Level::Warn, "not running").hint(
            "`strata init` in a repo installs and starts it; `strata daemon start` starts it",
        ),
        Some(s) if s["version"] != cli => check(
            "daemon",
            Level::Warn,
            format!("running {} but this CLI is {cli}", s["version"]),
        )
        .hint("restart it: `strata daemon stop`, then `strata daemon start`"),
        Some(s) => check("daemon", Level::Ok, format!("{cli}, pid {}", s["pid"])),
    }
}

fn git() -> Check {
    let version = output("git", &["--version"]).unwrap_or_default();
    match strata_git::check_version() {
        Ok(()) if !strata_git::merges_trees() => check("git", Level::Warn, version).hint(
            "git 2.38 or later lets Strata warn about files two agents changed that git cannot merge",
        ),
        Ok(()) => check("git", Level::Ok, version),
        Err(e) => check("git", Level::Fail, e.to_string()),
    }
}

fn node() -> Check {
    let node = detect::node();
    let Some(version) = output(&node.to_string_lossy(), &["--version"]) else {
        return check("node", Level::Fail, format!("{} not found", node.display()))
            .hint("the shared checker needs Node.js 20 or later (set STRATA_NODE to use another)");
    };
    let major: u32 = version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(0);
    if major < 20 {
        check(
            "node",
            Level::Warn,
            format!("{version}; the checker needs 20 or later"),
        )
    } else {
        check("node", Level::Ok, version)
    }
}

fn socket() -> Check {
    let path = strata_home().join(client::SOCKET);
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH {
        check(
            "socket",
            Level::Fail,
            format!(
                "{} is {len} bytes, over the {MAX_SOCKET_PATH}-byte limit",
                path.display()
            ),
        )
        .hint("set STRATA_HOME to a shorter folder")
    } else {
        check("socket", Level::Ok, path.display().to_string())
    }
}

fn repo(root: &Path, status: Option<&Value>) -> Vec<Check> {
    let found = detect::detect(root);
    let mut out = vec![match (&found.typescript, found.checker_off_reason()) {
        (_, Some(reason)) => check("typescript", Level::Warn, reason),
        (Some(v), None) => check("typescript", Level::Ok, format!("{v}; shared checker on")),
        (None, None) => check("typescript", Level::Warn, "not installed"),
    }];
    if !root.join(strata_core::config::FILE).exists() {
        out.push(
            check(
                "config",
                Level::Warn,
                "this repo is not set up for Strata yet",
            )
            .hint("`strata init` writes strata.toml and creates the base worktree"),
        );
        return out;
    }
    let cfg = match Config::load(root) {
        Ok(cfg) => cfg,
        Err(e) => {
            out.push(check("config", Level::Fail, e.to_string()));
            return out;
        }
    };
    out.push(clone(root, &cfg));
    out.push(proxy(&cfg, status));
    out.push(localhost(&cfg));
    out
}

/// Clones a small file in the worktree folder, which is what seeding will do there.
fn clone(root: &Path, cfg: &Config) -> Check {
    let dir = cfg.worktree_dir(root);
    if !dir.is_dir() {
        return check(
            "clone",
            Level::Warn,
            format!("{} does not exist yet", dir.display()),
        )
        .hint("`strata init` creates it");
    }
    let dev = |p: &Path| std::fs::metadata(p).map(|m| std::os::unix::fs::MetadataExt::dev(&m));
    let other_volume = dev(root).ok() != dev(&dir).ok();
    let (src, dst) = (
        dir.join(format!(".strata-doctor-{}", std::process::id())),
        dir.join(format!(".strata-doctor-{}-clone", std::process::id())),
    );
    let result =
        std::fs::write(&src, b"strata doctor").and_then(|()| strata_seed::clone_tree(&src, &dst));
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&dst);
    let fallback = format!("{:?}", cfg.seed.fallback).to_lowercase();
    match result {
        Ok(()) if other_volume => check(
            "clone",
            Level::Warn,
            format!("clones work in {}, but it is on another volume than the repo", dir.display()),
        )
        .hint("the base worktree installs instead of cloning the main checkout; agents still clone from it"),
        Ok(()) => check("clone", Level::Ok, format!("copy-on-write clones work in {}", dir.display())),
        Err(e) => check(
            "clone",
            Level::Warn,
            format!("cannot clone in {} ({e})", dir.display()),
        )
        .hint(format!(
            "new worktrees get seed.fallback = \"{fallback}\"; a filesystem with clones (APFS, btrfs, XFS with reflink) makes `strata new` much faster"
        )),
    }
}

fn proxy(cfg: &Config, status: Option<&Value>) -> Check {
    let port = cfg.dev.proxy_port;
    if TcpListener::bind(("127.0.0.1", port)).is_ok() {
        return check("proxy", Level::Ok, format!("port {port} is free"));
    }
    let holder = strata_proxy::holder(port);
    let ours = status
        .and_then(|s| s["pid"].as_u64())
        .is_some_and(|pid| holder.ends_with(&format!("(pid {pid})")));
    if ours {
        check(
            "proxy",
            Level::Ok,
            format!("port {port} is served by the Strata daemon"),
        )
    } else {
        check(
            "proxy",
            Level::Fail,
            format!("port {port} is held by {holder}"),
        )
        .hint("stop it, or set dev.proxy_port in strata.toml")
    }
}

/// Agent URLs are `<agent>.<app>.localhost`. Browsers resolve `*.localhost` themselves; other
/// clients (curl, Node's fetch) need the system resolver to.
fn localhost(cfg: &Config) -> Check {
    let app = cfg.dev.app.clone().unwrap_or_else(|| "app".into());
    let name = format!("doctor.{app}.localhost");
    let loopback = (name.as_str(), cfg.dev.proxy_port)
        .to_socket_addrs()
        .is_ok_and(|mut a| a.all(|a| a.ip().is_loopback()));
    if loopback {
        check(
            "localhost",
            Level::Ok,
            format!("{name} resolves to loopback"),
        )
    } else {
        check(
            "localhost",
            Level::Warn,
            format!("{name} does not resolve here; browsers resolve it anyway"),
        )
        .hint(format!(
            "for curl or scripts use the direct URL `strata dev` prints (http://localhost:<port>), or add a line per agent to /etc/hosts, which has no wildcards: `127.0.0.1 <agent>.{app}.localhost`"
        ))
    }
}

fn kernel() -> Check {
    let release = output("uname", &["-r"]).unwrap_or_default();
    let mut nums = release
        .split(['.', '-'])
        .map(|n| n.parse::<u32>().unwrap_or(0));
    let version = (nums.next().unwrap_or(0), nums.next().unwrap_or(0));
    if version < (5, 14) {
        check(
            "kernel",
            Level::Warn,
            format!("{release}; cgroup.kill needs 5.14+"),
        )
        .hint("agents' leftover processes are then found by parent pid only")
    } else {
        check("kernel", Level::Ok, release)
    }
}

fn cgroup(status: Option<&Value>) -> Check {
    match status.map(|s| &s["cgroup"]) {
        None => check("cgroup", Level::Warn, "unknown: the daemon is not running"),
        Some(Value::String(path)) => check("cgroup", Level::Ok, format!("per-agent cgroups under {path}")),
        Some(_) => check(
            "cgroup",
            Level::Warn,
            "no delegated cgroup; processes are tracked by parent pid and memory by PSS",
        )
        .hint("run the daemon as the systemd user service (`strata init` installs it with Delegate=yes)"),
    }
}

/// Dev servers watch files with inotify; several agents' servers can run out of instances or
/// watches long before memory.
fn inotify() -> Check {
    let limit = |f: &str| -> u64 {
        std::fs::read_to_string(format!("/proc/sys/fs/inotify/{f}"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    };
    let (max_instances, max_watches) = (limit("max_user_instances"), limit("max_user_watches"));
    let (instances, watches) = inotify_use();
    let detail =
        format!("{instances}/{max_instances} instances, {watches}/{max_watches} watches in use");
    if instances * 10 >= max_instances * 8 || watches * 10 >= max_watches * 8 {
        check("inotify", Level::Warn, detail).hint(
            "raise fs.inotify.max_user_instances and fs.inotify.max_user_watches (sysctl), e.g. 1024 and 524288",
        )
    } else {
        check("inotify", Level::Ok, detail)
    }
}

/// This user's inotify instances and watches, from `/proc/*/fd` and `fdinfo`.
fn inotify_use() -> (u64, u64) {
    let (mut instances, mut watches) = (0, 0);
    for proc in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let fds = proc.path().join("fd");
        for fd in std::fs::read_dir(&fds).into_iter().flatten().flatten() {
            if std::fs::read_link(fd.path()).is_ok_and(|t| t == Path::new("anon_inode:inotify")) {
                instances += 1;
                let info = proc.path().join("fdinfo").join(fd.file_name());
                watches += std::fs::read_to_string(info).map_or(0, |t| {
                    t.lines().filter(|l| l.starts_with("inotify wd:")).count()
                }) as u64;
            }
        }
    }
    (instances, watches)
}

fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn print_report(checks: &[Check]) {
    let home = std::env::var("HOME").unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_default();
    let root: Option<PathBuf> =
        strata_git::repo_root(&std::env::current_dir().unwrap_or_default()).ok();
    let redact = |s: &str| {
        let mut s = s.to_string();
        if let Some(root) = &root {
            s = s.replace(&root.to_string_lossy().to_string(), "<repo>");
            if let Some(parent) = root.parent() {
                s = s.replace(&parent.to_string_lossy().to_string(), "<repo-parent>");
            }
        }
        if !home.is_empty() {
            s = s.replace(&home, "~");
        }
        if user.len() > 2 {
            s = s.replace(&user, "<user>");
        }
        s.replace('|', "\\|")
    };
    println!(
        "strata {} · {} {} · {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        output("uname", &["-sr"]).unwrap_or_default()
    );
    println!("| check | result | detail |\n| --- | --- | --- |");
    for c in checks {
        let level = match c.level {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "**fail**",
        };
        println!("| {} | {level} | {} |", c.name, redact(&c.detail));
    }
}
