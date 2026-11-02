//! Control API: JSON over HTTP/1.1 on the Unix socket.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use strata_core::Error;
use strata_core::store::{Store, now};
use tokio::sync::Notify;

pub struct Daemon {
    pub store: Mutex<Store>,
    pub shutdown: Notify,
    pub started_at: i64,
}

pub type Shared = Arc<Daemon>;

impl Daemon {
    pub fn new(store: Store) -> Shared {
        Arc::new(Self {
            store: Mutex::new(store),
            shutdown: Notify::new(),
            started_at: now(),
        })
    }
}

pub fn router(daemon: Shared) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/shutdown", post(shutdown))
        .with_state(daemon)
}

pub struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = match &self.0 {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Refused(_) | Error::Config { .. } => StatusCode::CONFLICT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!("{}", self.0);
        }
        (status, Json(json!({ "error": self.0.to_string() }))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

async fn status(State(d): State<Shared>) -> ApiResult {
    let store = d.store.lock().unwrap();
    let mut repos = vec![];
    for repo in store.repos()? {
        let mut r = json!(repo);
        r["agents"] = json!(store.agents(repo.id)?);
        repos.push(r);
    }
    Ok(Json(json!({
        "pid": std::process::id(),
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": d.started_at,
        "repos": repos,
    })))
}

async fn shutdown(State(d): State<Shared>) -> ApiResult {
    tracing::info!("shutdown requested");
    d.shutdown.notify_one();
    Ok(Json(json!({})))
}
