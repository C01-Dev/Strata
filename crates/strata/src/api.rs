//! Control API: JSON over HTTP/1.1 on the Unix socket.

use crate::{ops, procs};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard};
use strata_checker::broker::Broker;
use strata_core::store::{Store, now};
use strata_core::{Error, Result};
use tokio::sync::Notify;

pub struct Daemon {
    store: Mutex<Store>,
    pub checker: Broker,
    pub procs: procs::Table,
    pub meter: procs::Meter,
    pub routes: crate::ports::Routes,
    /// Proxy tasks by port, started on first use.
    pub proxies: Mutex<std::collections::HashMap<u16, tokio::task::JoinHandle<()>>>,
    /// Linux with systemd delegation: the daemon's cgroup, with one child per agent.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    cgroup: Option<std::path::PathBuf>,
    /// Held for the whole of every worktree operation: Strata's git calls never overlap.
    pub git: Mutex<()>,
    pub shutdown: Notify,
    pub started_at: i64,
}

pub type Shared = Arc<Daemon>;

impl Daemon {
    pub fn new(store: Store, home: &std::path::Path) -> Shared {
        Arc::new(Self {
            store: Mutex::new(store),
            checker: Broker::new(home),
            procs: Default::default(),
            meter: Default::default(),
            routes: Default::default(),
            proxies: Default::default(),
            cgroup: cgroup_root(),
            git: Mutex::new(()),
            shutdown: Notify::new(),
            started_at: now(),
        })
    }

    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }

    /// The agent's cgroup (created on first use), where the daemon has delegation.
    pub fn agent_cgroup(&self, _agent: &str) -> Option<std::path::PathBuf> {
        #[cfg(target_os = "linux")]
        if let Some(root) = &self.cgroup {
            return strata_proc::cgroup::agent(root, _agent).ok();
        }
        None
    }

    pub fn agent_memory_from_cgroup(&self, _agent: &str) -> Option<u64> {
        #[cfg(target_os = "linux")]
        if self.cgroup.is_some() {
            return strata_proc::cgroup::memory(&self.agent_cgroup(_agent)?);
        }
        None
    }

    pub fn kill_agent_cgroup(&self, _agent: &str) {
        #[cfg(target_os = "linux")]
        if let Some(cg) = self.agent_cgroup(_agent) {
            let _ = strata_proc::cgroup::kill(&cg);
        }
    }
}

fn cgroup_root() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    {
        let root = strata_proc::cgroup::setup();
        match &root {
            Some(r) => tracing::info!(cgroup = %r.display(), "per-agent cgroups enabled"),
            None => tracing::info!("no delegated cgroup; tracking process trees by parent pid"),
        }
        root
    }
    #[cfg(not(target_os = "linux"))]
    None
}

pub fn router(daemon: Shared) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/shutdown", post(shutdown))
        .route("/v1/repos", post(register_repo))
        .route("/v1/agents", post(new_agent))
        .route("/v1/agents/adopt", post(adopt))
        .route("/v1/gc", post(gc))
        .route("/v1/agents/{name}", delete(remove))
        .route("/v1/agents/{name}/check", post(check))
        .route("/v1/agents/{name}/procs", post(start_proc))
        .route("/v1/procs/{id}", delete(stop_proc))
        .route("/v1/procs/{id}/logs", get(proc_logs))
        .with_state(daemon)
}

pub struct ApiError(Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Refused(_) | Error::Config { .. } | Error::Git { .. } => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!("{}", self.0);
        }
        (status, Json(json!({ "error": self.0.to_string() }))).into_response()
    }
}

type ApiResult = std::result::Result<Json<Value>, ApiError>;

/// Runs a worktree operation on the blocking pool (it shells out to git).
async fn blocking(
    d: Shared,
    op: impl FnOnce(&Daemon) -> Result<Value> + Send + 'static,
) -> ApiResult {
    match tokio::task::spawn_blocking(move || op(&d)).await {
        Ok(result) => result.map(Json).map_err(ApiError),
        Err(e) => Err(ApiError(Error::Io(std::io::Error::other(e)))),
    }
}

/// As `blocking`, for operations that return a value rather than JSON.
async fn blocking_value<T: Send + 'static>(
    d: Shared,
    op: impl FnOnce(&Daemon) -> Result<T> + Send + 'static,
) -> std::result::Result<T, ApiError> {
    match tokio::task::spawn_blocking(move || op(&d)).await {
        Ok(result) => result.map_err(ApiError),
        Err(e) => Err(ApiError(Error::Io(std::io::Error::other(e)))),
    }
}

async fn status(State(d): State<Shared>) -> ApiResult {
    let all_agents: Vec<_> = {
        let store = d.store();
        let repos = store.repos().map_err(ApiError)?;
        let mut all = vec![];
        for r in repos {
            all.extend(store.agents(r.id).map_err(ApiError)?);
        }
        all
    };
    let dd = d.clone();
    let unmanaged = tokio::task::spawn_blocking(move || crate::ports::unmanaged(&dd, &all_agents))
        .await
        .unwrap_or_default();
    let repos = {
        let store = d.store();
        let mut repos = vec![];
        for repo in store.repos().map_err(ApiError)? {
            let cfg = strata_core::config::Config::load(&repo.root).ok();
            let mut r = json!(repo);
            let mut agents = vec![];
            for agent in store.agents(repo.id).map_err(ApiError)? {
                let mut a = json!(agent);
                a["port"] = json!(store.agent_port(agent.id).map_err(ApiError)?);
                let mut ps = store.processes(agent.id).map_err(ApiError)?;
                ps.truncate(10);
                if let Some(cfg) = &cfg
                    && ps.iter().any(|p| p.kind == "dev" && p.state == "running")
                {
                    a["url"] = json!(crate::ports::url(&agent.name, &repo, cfg));
                }
                a["processes"] = json!(ps);
                let (bytes, level) = d
                    .meter
                    .lock()
                    .unwrap()
                    .get(&agent.id)
                    .copied()
                    .unwrap_or((0, procs::Level::Ok));
                a["memory_bytes"] = json!(bytes);
                a["budget"] = json!(level);
                agents.push(a);
            }
            r["agents"] = json!(agents);
            r["warm_pool"] = json!(crate::pool::count(&repo.root));
            repos.push(r);
        }
        repos
    };
    Ok(Json(json!({
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": d.started_at,
        "repos": repos,
        "checker": d.checker.status().await,
        "events": d.store().events(20).map_err(ApiError)?,
        "unmanaged": unmanaged,
        "cgroup": d.cgroup,
        "routes": d.routes.read().unwrap().iter().map(|(h, a)| (h.clone(), json!(a.to_string()))).collect::<serde_json::Map<_, _>>(),
    })))
}

async fn shutdown(State(d): State<Shared>) -> ApiResult {
    tracing::info!("shutdown requested");
    d.shutdown.notify_one();
    Ok(Json(json!({})))
}

async fn register_repo(State(d): State<Shared>, Json(req): Json<ops::RegisterRepo>) -> ApiResult {
    blocking(d, |d| ops::register_repo(d, req)).await
}

async fn new_agent(State(d): State<Shared>, Json(req): Json<ops::NewAgent>) -> ApiResult {
    let out = blocking(d.clone(), |d| ops::new_agent(d, req)).await?;
    // Refill the warm pool off the critical path.
    if let Some(repo_id) = out["agent"]["repo_id"].as_i64() {
        tokio::task::spawn_blocking(move || ops::fill_pool(&d, repo_id));
    }
    Ok(out)
}

async fn adopt(State(d): State<Shared>, Json(req): Json<ops::Adopt>) -> ApiResult {
    blocking(d, |d| ops::adopt(d, req)).await
}

async fn gc(State(d): State<Shared>) -> ApiResult {
    blocking(d, ops::gc).await
}

async fn remove(
    State(d): State<Shared>,
    Path(name): Path<String>,
    Json(mut req): Json<ops::Remove>,
) -> ApiResult {
    req.name = name.clone();
    let repo = req.repo.canonicalize().unwrap_or_else(|_| req.repo.clone());
    // Validate first, then stop the agent's processes, then remove the worktree.
    let check = ops::Remove {
        repo: req.repo.clone(),
        name: req.name.clone(),
        force: req.force,
        keep_branch: req.keep_branch,
    };
    let agent = blocking_value(d.clone(), move |d| ops::removable(d, &check)).await?;
    procs::stop_agent(&d, &agent).await;
    let out = blocking(d.clone(), |d| ops::remove(d, req)).await?;
    d.checker.dispose_agent(&repo, &name).await;
    Ok(out)
}

async fn check(
    State(d): State<Shared>,
    Path(name): Path<String>,
    Json(req): Json<ops::Check>,
) -> ApiResult {
    ops::check(&d, name, req).await.map(Json).map_err(ApiError)
}

async fn start_proc(
    State(d): State<Shared>,
    Path(name): Path<String>,
    Json(req): Json<procs::ProcRequest>,
) -> ApiResult {
    procs::request(&d, name, req)
        .await
        .map(Json)
        .map_err(ApiError)
}

#[derive(serde::Deserialize, Default)]
struct Owner {
    agent: Option<String>,
    lines: Option<usize>,
}

async fn stop_proc(
    State(d): State<Shared>,
    Path(id): Path<i64>,
    body: Option<Json<Owner>>,
) -> ApiResult {
    let owner = body.map(|Json(o)| o).unwrap_or_default();
    procs::owned(&d, id, owner.agent.as_deref()).map_err(ApiError)?;
    procs::stop(&d, id).await.map_err(ApiError)?;
    Ok(Json(json!({ "stopped": id })))
}

async fn proc_logs(
    State(d): State<Shared>,
    Path(id): Path<i64>,
    axum::extract::Query(q): axum::extract::Query<Owner>,
) -> ApiResult {
    let p = procs::owned(&d, id, q.agent.as_deref()).map_err(ApiError)?;
    Ok(Json(
        json!({ "process": p, "lines": procs::tail(&p.log_path, q.lines.unwrap_or(200)) }),
    ))
}
