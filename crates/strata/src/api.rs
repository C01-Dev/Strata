//! Control API: JSON over HTTP/1.1 on the Unix socket.

use crate::ops;
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
            git: Mutex::new(()),
            shutdown: Notify::new(),
            started_at: now(),
        })
    }

    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }
}

pub fn router(daemon: Shared) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/shutdown", post(shutdown))
        .route("/v1/repos", post(register_repo))
        .route("/v1/agents", post(new_agent))
        .route("/v1/agents/adopt", post(adopt))
        .route("/v1/agents/{name}", delete(remove))
        .route("/v1/agents/{name}/check", post(check))
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

async fn status(State(d): State<Shared>) -> ApiResult {
    let repos = {
        let store = d.store();
        let mut repos = vec![];
        for repo in store.repos().map_err(ApiError)? {
            let mut r = json!(repo);
            r["agents"] = json!(store.agents(repo.id).map_err(ApiError)?);
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
    blocking(d, |d| ops::new_agent(d, req)).await
}

async fn adopt(State(d): State<Shared>, Json(req): Json<ops::Adopt>) -> ApiResult {
    blocking(d, |d| ops::adopt(d, req)).await
}

async fn remove(
    State(d): State<Shared>,
    Path(name): Path<String>,
    Json(mut req): Json<ops::Remove>,
) -> ApiResult {
    req.name = name.clone();
    let repo = req.repo.canonicalize().unwrap_or_else(|_| req.repo.clone());
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
