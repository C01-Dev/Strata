//! Process supervision (PLAN 3.1). Every command runs in its agent's worktree as the leader of a
//! new process group, with `PORT`, `STRATA_AGENT` and `STRATA_URL` set and output in
//! `$STRATA_HOME/logs/<agent>/`. Strata records every process it sees in the tree (pid and start
//! time), because children can leave the group; stopping signals the group and all of them
//! (SIGTERM, 5 s, SIGKILL). After a daemon restart a process is re-adopted only when both its pid
//! and its start time match.

use crate::api::Daemon;
use crate::ops::log_dir;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use strata_core::store::{Agent, Process};
use strata_core::{Error, Result};
use tokio::sync::watch;

const GRACE: Duration = Duration::from_secs(5);

/// Exit status once known: `Some(code)`; `None` inside means killed by a signal or unknown.
type Done = watch::Sender<Option<Option<i32>>>;

pub struct Tracked {
    pub agent_id: i64,
    pub pgid: u32,
    /// Every process seen in the tree, with its start time.
    pub recorded: Vec<(u32, u64)>,
    stopping: bool,
    /// Re-adopted after a restart: no wait thread, exit is noticed by polling.
    adopted: bool,
    done: Arc<Done>,
}

/// Running processes, by process id.
pub type Table = std::sync::Mutex<HashMap<i64, Tracked>>;

/// `name.log` in the agent's log folder; `run` processes get a timestamped name.
fn log_path(agent: &str, kind: &str) -> std::path::PathBuf {
    let name = match kind {
        "dev" => "dev".to_string(),
        _ => format!(
            "{kind}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ),
    };
    log_dir(agent).join(format!("{name}.log"))
}

/// Starts `cmd` for `agent`. Returns the process id (Strata's, not the OS pid).
pub fn start(
    d: &Arc<Daemon>,
    agent: &Agent,
    kind: &str,
    cmd: &str,
    env: &[(String, String)],
    port: Option<u16>,
) -> Result<i64> {
    let log = log_path(&agent.name, kind);
    let id = d.store().insert_process(agent.id, kind, cmd, &log, port)?;
    let cgroup = d.agent_cgroup(&agent.name);
    let mut child = match strata_proc::spawn(cmd, &agent.path, env, &log, cgroup.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            d.store().set_process_state(id, "exited", None)?;
            return Err(Error::Refused(format!("cannot start `{cmd}`: {e}")));
        }
    };
    let pgid = child.id();
    let start = strata_proc::start_time(pgid).unwrap_or(0);
    d.store().set_process_running(id, pgid, start)?;
    let (done, _) = watch::channel(None);
    let done = Arc::new(done);
    d.procs.lock().unwrap().insert(
        id,
        Tracked {
            agent_id: agent.id,
            pgid,
            recorded: vec![(pgid, start)],
            stopping: false,
            adopted: false,
            done,
        },
    );
    d.store().event(
        Some(agent.id),
        "process.started",
        &format!("{id} {kind}: {cmd}"),
    )?;
    tracing::info!(agent = agent.name, id, pid = pgid, "started {kind}: {cmd}");
    let d = d.clone();
    std::thread::spawn(move || {
        let code = child.wait().ok().and_then(|s| s.code());
        finished(&d, id, code);
    });
    Ok(id)
}

/// The root exited (or was found gone): clear what is left of its tree and record the end.
fn finished(d: &Daemon, id: i64, code: Option<i32>) {
    let Some(t) = d.procs.lock().unwrap().remove(&id) else {
        return;
    };
    let leftovers: Vec<(u32, u64)> = t
        .recorded
        .iter()
        .copied()
        .filter(|&(p, s)| strata_proc::alive(p, s))
        .collect();
    if !leftovers.is_empty() {
        strata_proc::terminate(t.pgid, &leftovers, GRACE);
    }
    let port = d.store().process(id).ok().flatten().and_then(|p| p.port);
    d.routes
        .write()
        .unwrap()
        .retain(|_, addr| Some(addr.port()) != port);
    let state = if t.stopping { "stopped" } else { "exited" };
    let store = d.store();
    let _ = store.set_process_state(id, state, code);
    let _ = store.event(
        Some(t.agent_id),
        &format!("process.{state}"),
        &format!("{id} exit {code:?}"),
    );
    let _ = t.done.send(Some(code));
}

/// Stops one process tree and waits until it is recorded as stopped.
pub async fn stop(d: &Arc<Daemon>, id: i64) -> Result<()> {
    let (pgid, recorded, mut done) = {
        let mut procs = d.procs.lock().unwrap();
        let t = procs
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound(format!("process {id} is not running")))?;
        t.stopping = true;
        record_descendants(t);
        (t.pgid, t.recorded.clone(), t.done.subscribe())
    };
    d.store().set_process_state(id, "stopping", None)?;
    tokio::task::spawn_blocking(move || strata_proc::terminate(pgid, &recorded, GRACE))
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))?;
    // A child's wait thread records the end; a re-adopted process has none.
    if tokio::time::timeout(Duration::from_secs(2), done.wait_for(|v| v.is_some()))
        .await
        .is_err()
    {
        finished(d, id, None);
    }
    Ok(())
}

/// Stops every running process of an agent (and, on Linux with cgroups, anything left in its cgroup).
pub async fn stop_agent(d: &Arc<Daemon>, agent: &Agent) {
    let ids: Vec<i64> = d
        .procs
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, t)| t.agent_id == agent.id)
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        if let Err(e) = stop(d, id).await {
            tracing::warn!(agent = agent.name, "stopping process {id}: {e}");
        }
    }
    d.kill_agent_cgroup(&agent.name);
}

fn record_descendants(t: &mut Tracked) {
    for pid in strata_proc::descendants(t.pgid) {
        if !t.recorded.iter().any(|(p, _)| *p == pid)
            && let Some(start) = strata_proc::start_time(pid)
        {
            t.recorded.push((pid, start));
        }
    }
}

/// Maintenance tick: record new descendants; notice re-adopted processes that exited.
pub fn refresh(d: &Daemon) {
    let gone: Vec<i64> = {
        let mut procs = d.procs.lock().unwrap();
        for t in procs.values_mut() {
            record_descendants(t);
        }
        procs
            .iter()
            .filter(|(_, t)| t.adopted && !strata_proc::alive(t.recorded[0].0, t.recorded[0].1))
            .map(|(id, _)| *id)
            .collect()
    };
    for id in gone {
        finished(d, id, None);
    }
}

/// At startup: re-adopt processes whose pid and start time both match; mark the rest dead.
/// Returns the dead ones (dev servers may be restarted, see `dev.restart`).
pub fn recover(d: &Daemon) -> Result<Vec<Process>> {
    let mut dead = vec![];
    let live = d.store().live_processes()?;
    for p in live {
        match (p.pid, p.pid_start_time) {
            (Some(pid), Some(start)) if strata_proc::alive(pid, start) => {
                let (done, _) = watch::channel(None);
                let mut t = Tracked {
                    agent_id: p.agent_id,
                    pgid: pid,
                    recorded: vec![(pid, start)],
                    stopping: false,
                    adopted: true,
                    done: Arc::new(done),
                };
                record_descendants(&mut t);
                d.procs.lock().unwrap().insert(p.id, t);
                d.store().set_process_state(p.id, "running", None)?;
                tracing::info!(id = p.id, pid, "re-adopted process");
            }
            _ => {
                d.store().set_process_state(p.id, "dead", None)?;
                d.store().event(
                    Some(p.agent_id),
                    "process.dead",
                    &format!("{} gone after a daemon restart", p.id),
                )?;
                dead.push(p);
            }
        }
    }
    Ok(dead)
}

/// After `recover`: restart dev servers that died with the machine, in repos that set
/// `dev.restart = true` (PLAN: "dev servers restart only if dev.restart = true").
pub fn restart_dev_servers(d: &Arc<Daemon>, dead: Vec<Process>) {
    for p in dead.into_iter().filter(|p| p.kind == "dev") {
        let found = (|| {
            let store = d.store();
            let agent = store.agent_by_id(p.agent_id).ok()??;
            let repo = store
                .repos()
                .ok()?
                .into_iter()
                .find(|r| r.id == agent.repo_id)?;
            Some((agent, repo))
        })();
        let Some((agent, repo)) = found else { continue };
        let restart = strata_core::config::Config::load(&repo.root).is_ok_and(|c| c.dev.restart);
        if !restart || agent.state != strata_core::store::AgentState::Ready {
            continue;
        }
        let mut env = vec![("STRATA_AGENT".to_string(), agent.name.clone())];
        if let Some(port) = p.port {
            env.push(("PORT".into(), port.to_string()));
        }
        match start(d, &agent, "dev", &p.cmd, &env, p.port) {
            Ok(id) => tracing::info!(agent = agent.name, id, "restarted dev server (dev.restart)"),
            Err(e) => tracing::warn!(agent = agent.name, "restarting dev server: {e}"),
        }
    }
}

/// Budget level per agent: below 90% of the budget, from 90%, above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warning,
    Over,
}

/// The latest reading per agent id: bytes used by its processes, and its budget level.
pub type Meter = std::sync::Mutex<HashMap<i64, (u64, Level)>>;

/// PLAN 3.2, every maintenance tick: memory per agent (its cgroup's `memory.current` where the
/// daemon has one, else the footprint/PSS of every live process recorded in its trees), then
/// budgets: a warning event at 90% of `budget.per_agent_mb`, an event above it, and with
/// `budget.action = "stop"` its dev and run processes are stopped (never the agent itself).
pub async fn meter(d: &Arc<Daemon>) {
    let dd = d.clone();
    let Ok(readings) = tokio::task::spawn_blocking(move || measure(&dd)).await else {
        return;
    };
    for (agent_id, bytes) in readings {
        let Some((agent, budget)) = budget_for(d, agent_id) else {
            continue;
        };
        let limit = budget.per_agent_mb * 1024 * 1024;
        let level = if bytes > limit {
            Level::Over
        } else if bytes * 10 >= limit * 9 {
            Level::Warning
        } else {
            Level::Ok
        };
        let previous = d
            .meter
            .lock()
            .unwrap()
            .insert(agent_id, (bytes, level))
            .map(|(_, l)| l);
        if previous == Some(level) || level == Level::Ok {
            continue;
        }
        let detail = format!(
            "{} uses {} MB of its {} MB budget",
            agent.name,
            bytes / 1048576,
            budget.per_agent_mb
        );
        let kind = if level == Level::Over {
            "budget.exceeded"
        } else {
            "budget.warning"
        };
        tracing::warn!("{detail}");
        let _ = d.store().event(Some(agent_id), kind, &detail);
        if level == Level::Over && budget.action == strata_core::config::BudgetAction::Stop {
            stop_agent(d, &agent).await;
            let _ = d.store().event(
                Some(agent_id),
                "budget.stopped",
                &format!("stopped {}'s processes: {detail}", agent.name),
            );
            d.meter.lock().unwrap().insert(agent_id, (0, Level::Ok));
        }
    }
}

fn measure(d: &Daemon) -> Vec<(i64, u64)> {
    let trees: Vec<(i64, Vec<(u32, u64)>)> = {
        let mut procs = d.procs.lock().unwrap();
        procs
            .values_mut()
            .map(|t| {
                record_descendants(t);
                (t.agent_id, t.recorded.clone())
            })
            .collect()
    };
    let mut per_agent: HashMap<i64, Vec<(u32, u64)>> = HashMap::new();
    for (agent, recorded) in trees {
        per_agent.entry(agent).or_default().extend(recorded);
    }
    per_agent
        .into_iter()
        .map(|(agent, mut pids)| {
            pids.sort();
            pids.dedup();
            let cgroup = d
                .store()
                .agent_by_id(agent)
                .ok()
                .flatten()
                .and_then(|a| d.agent_memory_from_cgroup(&a.name));
            let bytes = cgroup.unwrap_or_else(|| {
                pids.iter()
                    .filter(|&&(p, s)| strata_proc::alive(p, s))
                    .map(|&(p, _)| strata_proc::process_memory(p))
                    .sum()
            });
            (agent, bytes)
        })
        .collect()
}

fn budget_for(d: &Daemon, agent_id: i64) -> Option<(Agent, strata_core::config::Budget)> {
    let store = d.store();
    let agent = store.agent_by_id(agent_id).ok()??;
    let repo = store
        .repos()
        .ok()?
        .into_iter()
        .find(|r| r.id == agent.repo_id)?;
    Some((
        agent,
        strata_core::config::Config::load(&repo.root).ok()?.budget,
    ))
}

/// The last `n` lines of a log file.
pub fn tail(path: &Path, n: usize) -> Vec<String> {
    let text = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

/// Waits for a process to end, up to `timeout`: `Some(exit code)` once it ended, `None` on timeout.
pub async fn wait(d: &Daemon, id: i64, timeout: Duration) -> Option<Option<i32>> {
    let done = d.procs.lock().unwrap().get(&id).map(|t| t.done.subscribe());
    let ended = || d.store().process(id).ok().flatten().map(|p| p.exit_code);
    let Some(mut done) = done else { return ended() }; // already finished
    match tokio::time::timeout(timeout, done.wait_for(|v| v.is_some())).await {
        Ok(Ok(v)) => *v,
        Ok(Err(_)) => ended(),
        Err(_) => None,
    }
}

#[derive(Deserialize)]
pub struct ProcRequest {
    pub repo: std::path::PathBuf,
    /// dev | run
    pub kind: String,
    pub cmd: Option<String>,
    pub timeout_s: Option<u64>,
}

/// `POST /v1/agents/{name}/procs`. `dev` starts the dev server (or returns the running one) and
/// returns at once; `run` waits for the command, up to `timeout_s`, and returns its output.
pub async fn request(d: &Arc<Daemon>, name: String, req: ProcRequest) -> Result<Value> {
    let (repo, agent, cfg) = crate::ops::ready_agent(d, &req.repo, &name)?;
    let mut env = vec![("STRATA_AGENT".to_string(), agent.name.clone())];
    match req.kind.as_str() {
        "dev" => {
            if let Some(p) = d
                .store()
                .processes(agent.id)?
                .into_iter()
                .find(|p| p.kind == "dev" && p.state == "running")
            {
                return Ok(json!({ "process": p, "already_running": true }));
            }
            let cmd = req
                .cmd
                .or(cfg.dev.command.clone())
                .or_else(|| strata_core::detect::detect(&agent.path).dev_command)
                .ok_or_else(|| {
                    Error::Refused(
                        "no dev command: set dev.command in strata.toml or add a `dev` script"
                            .into(),
                    )
                })?;
            crate::ports::ensure_proxy(d, cfg.dev.proxy_port).await?;
            let port = crate::ports::ensure(d, &agent, &cfg)?;
            let url = crate::ports::url(&agent.name, &repo, &cfg);
            env.push(("PORT".into(), port.to_string()));
            env.push(("STRATA_URL".into(), url.clone()));
            let id = start(d, &agent, "dev", &cmd, &env, Some(port))?;
            tokio::spawn(crate::ports::confirm(
                d.clone(),
                id,
                crate::ports::host(&agent.name, &repo, &cfg),
                port,
            ));
            Ok(
                json!({ "process": d.store().process(id)?, "port": port, "url": url, "direct_url": format!("http://localhost:{port}") }),
            )
        }
        "run" => {
            let cmd = req
                .cmd
                .ok_or_else(|| Error::Refused("run needs a command".into()))?;
            let id = start(d, &agent, "run", &cmd, &env, None)?;
            let timeout = Duration::from_secs(req.timeout_s.unwrap_or(600));
            let outcome = wait(d, id, timeout).await;
            if outcome.is_none() {
                stop(d, id).await?;
            }
            let p = d
                .store()
                .process(id)?
                .ok_or_else(|| Error::NotFound(format!("process {id}")))?;
            Ok(json!({
                "process": p,
                "exit_code": outcome.flatten(),
                "timed_out": outcome.is_none(),
                "output": tail(&p.log_path, 200),
            }))
        }
        other => Err(Error::Refused(format!(
            "unknown process kind {other:?} (dev or run)"
        ))),
    }
}

/// The process, if `agent` (when given) owns it.
pub fn owned(d: &Daemon, id: i64, agent: Option<&str>) -> Result<Process> {
    let store = d.store();
    let p = store
        .process(id)?
        .ok_or_else(|| Error::NotFound(format!("no process {id}")))?;
    if let Some(name) = agent {
        let owner = store.agent_by_id(p.agent_id)?;
        if owner.as_ref().map(|a| a.name.as_str()) != Some(name) {
            return Err(Error::NotFound(format!(
                "no process {id} for agent {name:?}"
            )));
        }
    }
    Ok(p)
}
