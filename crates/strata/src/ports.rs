//! Port registry (PLAN 3.3). Each agent owns one port from `dev.port_range`, chosen when it is
//! created: the lowest port no agent holds that is also free on 127.0.0.1 and ::1 (so a port an
//! outside program uses is skipped). Its dev server gets it as `PORT`; within 30 s the daemon
//! confirms which process of the tree actually listens, and routes to wherever it is.

use crate::api::Daemon;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use strata_core::config::{Config, to_dns_label};
use strata_core::store::{Agent, Repo};
use strata_core::{Error, Result};

/// Host label (`<agent>.<app>`) -> where its dev server listens. The proxy reads it.
pub type Routes = Arc<std::sync::RwLock<HashMap<String, SocketAddr>>>;

/// Serializes assignment, so agents created at the same moment never get the same port.
static ASSIGN: Mutex<()> = Mutex::new(());

const CONFIRM: Duration = Duration::from_secs(30);

fn free(port: u16) -> bool {
    let v4 = TcpListener::bind(("127.0.0.1", port)).is_ok();
    // A machine without IPv6 cannot have the port taken there.
    let v6 = match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
        Ok(_) => true,
        Err(e) => e.kind() == std::io::ErrorKind::AddrNotAvailable,
    };
    v4 && v6
}

/// The agent's port, reserving one if it has none.
pub fn ensure(d: &Daemon, agent: &Agent, cfg: &Config) -> Result<u16> {
    let _one_at_a_time = ASSIGN.lock().unwrap();
    if let Some(port) = d.store().agent_port(agent.id)? {
        return Ok(port);
    }
    let [lo, hi] = cfg.dev.port_range;
    let held = d.store().ports()?;
    for port in lo..=hi {
        if !held.contains(&port) && free(port) && d.store().reserve_port(port, agent.id)? {
            return Ok(port);
        }
    }
    Err(Error::Refused(format!(
        "no free port in dev.port_range [{lo}, {hi}]"
    )))
}

/// `dev.app`, or the repo folder as a hostname label.
pub fn app(repo: &Repo, cfg: &Config) -> String {
    cfg.dev.app.clone().unwrap_or_else(|| {
        to_dns_label(&repo.root.file_name().unwrap_or_default().to_string_lossy())
    })
}

pub fn host(agent: &str, repo: &Repo, cfg: &Config) -> String {
    format!("{agent}.{}", app(repo, cfg))
}

pub fn url(agent: &str, repo: &Repo, cfg: &Config) -> String {
    format!(
        "http://{}.localhost:{}",
        host(agent, repo, cfg),
        cfg.dev.proxy_port
    )
}

/// Where to connect for a listener: wildcard addresses become loopback of the same family.
fn reachable(addr: SocketAddr) -> SocketAddr {
    match addr {
        SocketAddr::V4(a) if a.ip().is_unspecified() => {
            SocketAddr::from(([127, 0, 0, 1], a.port()))
        }
        SocketAddr::V6(a) if a.ip().is_unspecified() => {
            SocketAddr::from((Ipv6Addr::LOCALHOST, a.port()))
        }
        a => a,
    }
}

/// Polls until a process of the tree listens (up to 30 s), then routes `host` to it. A server
/// that ignored `PORT` is routed where it really listens, and its record updated.
pub async fn confirm(d: Arc<Daemon>, process_id: i64, host: String, wanted: u16) {
    let deadline = Instant::now() + CONFIRM;
    while Instant::now() < deadline {
        let tree: Vec<u32> = match d.procs.lock().unwrap().get(&process_id) {
            Some(t) => t
                .recorded
                .iter()
                .map(|(p, _)| *p)
                .chain(strata_proc::descendants(t.pgid))
                .collect(),
            None => return, // exited before listening
        };
        let mut mine: Vec<SocketAddr> = strata_proc::tcp_listeners()
            .into_iter()
            .filter(|(pid, _)| tree.contains(pid))
            .map(|(_, a)| a)
            .collect();
        mine.sort_by_key(|a| (a.port() != wanted, a.port(), a.is_ipv6()));
        if let Some(&addr) = mine.first() {
            let target = reachable(addr);
            if addr.port() != wanted {
                let _ = d.store().set_process_port(process_id, addr.port());
                let detail = format!(
                    "{host}: dev server listens on {} instead of {wanted}; route updated",
                    addr.port()
                );
                tracing::warn!("{detail}");
                if let Some(t) = d.procs.lock().unwrap().get(&process_id) {
                    let _ = d.store().event(Some(t.agent_id), "route.updated", &detail);
                }
            }
            tracing::info!(host, %target, "route confirmed");
            d.routes.write().unwrap().insert(host, target);
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    tracing::warn!(host, "no listener from the dev server within 30 s");
    if let Some(t) = d.procs.lock().unwrap().get(&process_id) {
        let _ = d.store().event(
            Some(t.agent_id),
            "listener.missing",
            &format!("{host}: nothing listening after 30 s"),
        );
    }
}

/// After a daemon restart: confirm (and so route) every running dev server again.
pub async fn reconfirm(d: Arc<Daemon>) {
    let mut work = vec![];
    {
        let store = d.store();
        for repo in store.repos().unwrap_or_default() {
            let Ok(cfg) = Config::load(&repo.root) else {
                continue;
            };
            for agent in store.agents(repo.id).unwrap_or_default() {
                for p in store.processes(agent.id).unwrap_or_default() {
                    if p.kind == "dev"
                        && p.state == "running"
                        && let Some(port) = p.port
                    {
                        work.push((p.id, host(&agent.name, &repo, &cfg), port));
                    }
                }
            }
        }
    }
    for (id, host, port) in work {
        tokio::spawn(confirm(d.clone(), id, host, port));
    }
}

/// Listening sockets whose process runs inside an agent's worktree but is not one Strata
/// started (PLAN: status suggests `strata_start_dev_server`).
pub fn unmanaged(d: &Daemon, agents: &[Agent]) -> Vec<Value> {
    let managed: Vec<u32> = d
        .procs
        .lock()
        .unwrap()
        .values()
        .flat_map(|t| t.recorded.iter().map(|(p, _)| *p))
        .collect();
    let mut out = vec![];
    for (pid, addr) in strata_proc::tcp_listeners() {
        if managed.contains(&pid) || pid == std::process::id() {
            continue;
        }
        let Some(cwd) = strata_proc::cwd(pid) else {
            continue;
        };
        if let Some(a) = agents.iter().find(|a| cwd.starts_with(&a.path)) {
            out.push(json!({
                "agent": a.name,
                "pid": pid,
                "port": addr.port(),
                "hint": "started outside Strata: start it with strata_start_dev_server (MCP) or `strata dev` so it gets a URL, a budget and cleanup",
            }));
        }
    }
    out.sort_by_key(|v| {
        (
            v["agent"].as_str().unwrap_or_default().to_string(),
            v["port"].as_u64(),
        )
    });
    out.dedup();
    out
}
