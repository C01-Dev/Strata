//! Process trees: memory, identity, spawning and stopping. macOS memory is `phys_footprint`
//! (Activity Monitor's "Memory") summed over the process and its descendants; Linux is PSS from
//! `/proc/<pid>/smaps_rollup`, or a cgroup's `memory.current` where the daemon has one.

use std::fs::{self, OpenOptions};
use std::io;
use std::net::SocketAddr;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Bytes used by `pid` alone (0 if it is gone).
pub fn process_memory(pid: u32) -> u64 {
    platform::memory(pid)
}

/// Bytes used by `pid` and all its descendants (0 if it is gone).
pub fn tree_memory(pid: u32) -> u64 {
    std::iter::once(pid)
        .chain(descendants(pid))
        .map(platform::memory)
        .sum()
}

/// Every descendant of `pid`, depth first.
pub fn descendants(pid: u32) -> Vec<u32> {
    let mut out = vec![];
    let mut stack = platform::children(pid);
    while let Some(p) = stack.pop() {
        out.push(p);
        stack.extend(platform::children(p));
    }
    out
}

/// When `pid` started (microseconds on macOS, clock ticks since boot on Linux). With the pid, it
/// identifies a process across daemon restarts: a reused pid has another start time.
pub fn start_time(pid: u32) -> Option<u64> {
    platform::start_time(pid)
}

/// Whether the process that had `start` is still running.
pub fn alive(pid: u32, start: u64) -> bool {
    start_time(pid) == Some(start)
}

pub fn cwd(pid: u32) -> Option<PathBuf> {
    platform::cwd(pid)
}

/// Listening TCP sockets with their owner's pid.
pub fn tcp_listeners() -> Vec<(u32, SocketAddr)> {
    use listeners::{Protocol, SocketState};
    let Ok(all) = listeners::get_all() else {
        return vec![];
    };
    all.into_iter()
        .filter(|l| l.protocol == Protocol::TCP && l.state == SocketState::Listen)
        .map(|l| (l.process.pid, l.socket))
        .collect()
}

const ROTATE_BYTES: u64 = 10 * 1024 * 1024;

/// Runs `sh -c cmd` in `cwd` as the leader of a new process group, with output appended to
/// `log` (rotated to `<log>.1` first if it is over 10 MB). On Linux with a delegated cgroup,
/// the child joins `cgroup` before it runs the command.
// ponytail: rotation happens at start only; a long-running server's log grows until its next start.
pub fn spawn(
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
    log: &Path,
    cgroup: Option<&Path>,
) -> io::Result<Child> {
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
    }
    if fs::metadata(log).is_ok_and(|m| m.len() > ROTATE_BYTES) {
        fs::rename(log, log.with_extension("log.1"))?;
    }
    let out = OpenOptions::new().create(true).append(true).open(log)?;
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .process_group(0);
    platform::join_cgroup(&mut command, cgroup)?;
    command.spawn()
}

fn send(pid: u32, sig: i32) {
    // SAFETY: kill has no memory-safety preconditions; errors (ESRCH) are expected and ignored.
    unsafe { libc::kill(pid as i32, sig) };
}

fn group_alive(pgid: u32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::killpg(pgid as i32, 0) == 0 }
}

/// Stops a process tree: SIGTERM to the process group and to every recorded process (pid with
/// start time, so a reused pid is never hit), up to `grace` to exit, then SIGKILL. Recorded
/// processes cover children that left the group (PLAN: "walk descendants and record every child").
pub fn terminate(pgid: u32, recorded: &[(u32, u64)], grace: Duration) {
    let signal = |sig| {
        // SAFETY: as in `send`.
        unsafe { libc::killpg(pgid as i32, sig) };
        for &(pid, start) in recorded {
            if alive(pid, start) {
                send(pid, sig);
            }
        }
    };
    signal(libc::SIGTERM);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline
        && (group_alive(pgid) || recorded.iter().any(|&(p, s)| alive(p, s)))
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    signal(libc::SIGKILL);
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::mem::MaybeUninit;

    fn pidinfo<T>(pid: u32, flavor: i32) -> Option<T> {
        let mut info = MaybeUninit::<T>::zeroed();
        let size = size_of::<T>() as i32;
        // SAFETY: the buffer is a T of `size` bytes, which is what `flavor` writes.
        let n =
            unsafe { libc::proc_pidinfo(pid as i32, flavor, 0, info.as_mut_ptr().cast(), size) };
        // SAFETY: zero-initialized, and fully written when the call returns `size`.
        (n == size).then(|| unsafe { info.assume_init() })
    }

    pub fn memory(pid: u32) -> u64 {
        let mut info = MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: the buffer is a rusage_info_v2, which is what RUSAGE_INFO_V2 writes.
        let rc = unsafe {
            libc::proc_pid_rusage(pid as i32, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast())
        };
        // SAFETY: zero-initialized, and fully written on success.
        if rc == 0 {
            unsafe { info.assume_init() }.ri_phys_footprint
        } else {
            0
        }
    }

    pub fn children(pid: u32) -> Vec<u32> {
        let mut buf = vec![0 as libc::pid_t; 4096];
        let size = (buf.len() * size_of::<libc::pid_t>()) as i32;
        // SAFETY: buf has room for `size` bytes; the call returns how many pids it wrote.
        let n = unsafe { libc::proc_listchildpids(pid as i32, buf.as_mut_ptr().cast(), size) };
        buf.truncate(n.max(0) as usize);
        buf.into_iter().map(|p| p as u32).collect()
    }

    pub fn start_time(pid: u32) -> Option<u64> {
        let info: libc::proc_bsdinfo = pidinfo(pid, libc::PROC_PIDTBSDINFO)?;
        Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        let info: libc::proc_vnodepathinfo = pidinfo(pid, libc::PROC_PIDVNODEPATHINFO)?;
        let raw: Vec<u8> = info
            .pvi_cdir
            .vip_path
            .iter()
            .flatten()
            .map(|&c| c as u8)
            .take_while(|&c| c != 0)
            .collect();
        (!raw.is_empty()).then(|| PathBuf::from(String::from_utf8_lossy(&raw).into_owned()))
    }

    pub fn join_cgroup(_: &mut Command, _: Option<&Path>) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;

    fn stat_fields(pid: u32) -> Option<Vec<String>> {
        // Field 2 (comm) may contain spaces; everything after the last ')' is space-separated,
        // starting with field 3.
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        Some(
            stat.rsplit_once(')')?
                .1
                .split_whitespace()
                .map(String::from)
                .collect(),
        )
    }

    pub fn memory(pid: u32) -> u64 {
        let Ok(text) = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) else {
            return 0;
        };
        let kb = text
            .lines()
            .find_map(|l| l.strip_prefix("Pss:"))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok());
        kb.unwrap_or(0) * 1024
    }

    pub fn children(pid: u32) -> Vec<u32> {
        let Ok(dir) = fs::read_dir("/proc") else {
            return vec![];
        };
        dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&p| stat_fields(p).and_then(|f| f.get(1)?.parse::<u32>().ok()) == Some(pid))
            .collect()
    }

    pub fn start_time(pid: u32) -> Option<u64> {
        stat_fields(pid)?.get(19)?.parse().ok() // field 22, starttime
    }

    pub fn cwd(pid: u32) -> Option<PathBuf> {
        fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }

    /// The child writes "0" (itself) to the cgroup's `cgroup.procs` between fork and exec. The file
    /// is opened before forking so the child only makes a `write` call (async-signal-safe).
    pub fn join_cgroup(command: &mut Command, cgroup: Option<&Path>) -> io::Result<()> {
        use std::os::fd::IntoRawFd;
        let Some(cg) = cgroup else { return Ok(()) };
        let fd = OpenOptions::new()
            .write(true)
            .open(cg.join("cgroup.procs"))?
            .into_raw_fd();
        // SAFETY: the closure only calls write(2) on an open descriptor.
        unsafe {
            command.pre_exec(move || {
                if libc::write(fd, b"0".as_ptr().cast(), 1) == 1 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
        Ok(())
    }
}

/// Linux cgroup v2 with systemd delegation (`Delegate=yes`): one child cgroup per agent, so a
/// tree can be killed whole (`cgroup.kill`) and measured (`memory.current`).
#[cfg(target_os = "linux")]
pub mod cgroup {
    use super::*;

    /// Sets up the daemon's delegated cgroup: moves the daemon into a `daemon` leaf (cgroup v2
    /// allows no processes beside child cgroups that use controllers), then enables the memory
    /// controller for children. `None` when not delegated or not cgroup v2.
    pub fn setup() -> Option<PathBuf> {
        let text = fs::read_to_string("/proc/self/cgroup").ok()?;
        let rel = text
            .lines()
            .find_map(|l| l.strip_prefix("0::"))?
            .trim()
            .trim_start_matches('/')
            .to_string();
        let mut ours = Path::new("/sys/fs/cgroup").join(rel);
        if ours.file_name().is_some_and(|n| n == "daemon") {
            ours.pop(); // a restarted daemon is already in its leaf
        }
        let leaf = ours.join("daemon");
        fs::create_dir_all(&leaf).ok()?;
        fs::write(leaf.join("cgroup.procs"), std::process::id().to_string()).ok()?;
        fs::write(ours.join("cgroup.subtree_control"), "+memory").ok()?;
        Some(ours)
    }

    pub fn agent(root: &Path, agent: &str) -> io::Result<PathBuf> {
        let dir = root.join(format!("agent-{agent}"));
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn memory(cg: &Path) -> Option<u64> {
        fs::read_to_string(cg.join("memory.current"))
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Kills every process in the cgroup, wherever it moved in the process tree (Linux 5.14+).
    pub fn kill(cg: &Path) -> io::Result<()> {
        fs::write(cg.join("cgroup.kill"), "1")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_self_and_children() {
        let me = tree_memory(std::process::id());
        assert!(me > 1_000_000, "{me}");
        let mut child = Command::new("sleep").arg("5").spawn().unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(descendants(std::process::id()).contains(&child.id()));
        assert!(tree_memory(std::process::id()) > me);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(tree_memory(u32::MAX - 1), 0, "gone pid");
    }

    #[test]
    fn identity_and_cwd() {
        let me = std::process::id();
        let start = start_time(me).unwrap();
        assert!(alive(me, start));
        assert!(!alive(me, start + 1));
        assert_eq!(
            cwd(me).unwrap().canonicalize().unwrap(),
            std::env::current_dir().unwrap().canonicalize().unwrap()
        );
    }

    #[test]
    fn spawn_logs_and_terminate_reaches_escaped_children() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("logs/a/dev.log");
        // The shell starts a child that leaves the process group (its own session), then waits.
        let cmd = "echo hello; echo \"PORT=$PORT\"; perl -e 'use POSIX; setsid(); sleep 60' & wait";
        let env = vec![("PORT".to_string(), "4123".to_string())];
        let mut child = spawn(cmd, dir.path(), &env, &log, None).unwrap();
        let pgid = child.id();
        std::thread::sleep(Duration::from_millis(500));
        let recorded: Vec<(u32, u64)> = descendants(pgid)
            .into_iter()
            .filter_map(|p| Some((p, start_time(p)?)))
            .collect();
        assert!(!recorded.is_empty());
        terminate(pgid, &recorded, Duration::from_secs(5));
        child.wait().unwrap();
        assert!(
            recorded.iter().all(|&(p, s)| !alive(p, s)),
            "the escaped child is gone too"
        );
        let text = fs::read_to_string(&log).unwrap();
        assert!(text.contains("hello\nPORT=4123"), "{text}");
    }

    #[test]
    fn finds_its_own_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(
            tcp_listeners()
                .iter()
                .any(|(pid, addr)| *pid == std::process::id() && addr.port() == port)
        );
    }
}
