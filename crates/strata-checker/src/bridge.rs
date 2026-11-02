//! Client for `packages/tsbridge/bridge.mjs`: spawns `node bridge.mjs <repo>`, checks the
//! protocol with `hello`, matches responses to requests by id, applies timeouts, and fails
//! pending calls when the bridge dies. Restarting is the broker's job (see `alive`).

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use strata_core::{Error, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

pub const SCRIPT: &str = include_str!("../../../packages/tsbridge/bridge.mjs");
pub const PROTOCOL: u64 = 1;
const HELLO_TIMEOUT: Duration = Duration::from_secs(30);

/// Writes the embedded bridge to `<home>/tsbridge/<version>-<hash>/bridge.mjs` (once per build).
pub fn install_script(home: &Path) -> Result<PathBuf> {
    let hash = xxhash_rust::xxh3::xxh3_64(SCRIPT.as_bytes());
    let dir = home
        .join("tsbridge")
        .join(format!("{}-{hash:016x}", env!("CARGO_PKG_VERSION")));
    let path = dir.join("bridge.mjs");
    if !path.exists() {
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("bridge.mjs.{}", std::process::id()));
        std::fs::write(&tmp, SCRIPT)?;
        std::fs::rename(tmp, &path)?;
    }
    Ok(path)
}

/// `$STRATA_NODE`, else `node` from PATH.
pub fn node() -> PathBuf {
    std::env::var_os("STRATA_NODE").map_or_else(|| PathBuf::from("node"), PathBuf::from)
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

pub struct Bridge {
    /// `None` after `shutdown`: dropping it is what closes the pipe (tokio's `shutdown()` on a
    /// child pipe only flushes).
    stdin: tokio::sync::Mutex<Option<ChildStdin>>,
    pending: Pending,
    next: AtomicU64,
    alive: Arc<AtomicBool>,
    child: Mutex<Option<Child>>,
    /// TypeScript version the bridge loaded from the repo.
    pub typescript: String,
    /// The Node process (tsgo runs as its child).
    pub pid: u32,
}

fn restarting() -> Error {
    Error::Refused("checker restarting, retry in a few seconds".into())
}

fn bridge_error(e: &Value) -> Error {
    let message = e["message"].as_str().unwrap_or("bridge error").to_string();
    match e["code"].as_str() {
        Some("not_in_project") => Error::NotFound(message),
        Some("restarting") => restarting(),
        Some("bad_request") => Error::Refused(message),
        _ => Error::Checker(message),
    }
}

impl Bridge {
    pub async fn spawn(node: &Path, script: &Path, repo: &Path) -> Result<Self> {
        let mut child = Command::new(node)
            .arg(script)
            .arg(repo)
            .current_dir(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Checker(format!("cannot start {}: {e}", node.display())))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = child.stdout.take().expect("piped");
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (p, a) = (pending.clone(), alive.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    tracing::warn!("tsbridge sent a non-JSON line: {line}");
                    continue;
                };
                let Some(tx) = msg["id"]
                    .as_u64()
                    .and_then(|id| p.lock().unwrap().remove(&id))
                else {
                    continue;
                };
                let reply = if msg["error"].is_object() {
                    Err(bridge_error(&msg["error"]))
                } else {
                    Ok(msg["result"].clone())
                };
                let _ = tx.send(reply);
            }
            a.store(false, Ordering::SeqCst);
            for (_, tx) in p.lock().unwrap().drain() {
                let _ = tx.send(Err(restarting()));
            }
        });
        let pid = child.id().unwrap_or(0);
        let mut bridge = Self {
            stdin: tokio::sync::Mutex::new(Some(stdin)),
            pending,
            next: AtomicU64::new(0),
            alive,
            child: Mutex::new(Some(child)),
            typescript: String::new(),
            pid,
        };
        let hello = bridge
            .call("hello", json!({ "protocol": PROTOCOL }), HELLO_TIMEOUT)
            .await?;
        if hello["protocol"].as_u64() != Some(PROTOCOL) {
            return Err(Error::Checker(format!(
                "tsbridge speaks protocol {}, Strata needs {PROTOCOL}",
                hello["protocol"]
            )));
        }
        bridge.typescript = hello["typescript"].as_str().unwrap_or_default().to_string();
        Ok(bridge)
    }

    /// False once the bridge has exited; the broker then starts a new one.
    pub fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if !self.alive() {
            return Err(restarting());
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let line = json!({ "id": id, "method": method, "params": params }).to_string() + "\n";
        let written = match self.stdin.lock().await.as_mut() {
            Some(stdin) => stdin.write_all(line.as_bytes()).await.is_ok(),
            None => false,
        };
        if !written {
            self.pending.lock().unwrap().remove(&id);
            return Err(restarting());
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => Err(restarting()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(Error::Refused(format!(
                    "checker timed out after {}s on {method}",
                    timeout.as_secs()
                )))
            }
        }
    }

    /// Stops the bridge (and with it tsgo): closing stdin makes it exit; kill if it lingers.
    pub async fn shutdown(&self) {
        drop(self.stdin.lock().await.take());
        let Some(mut child) = self.child.lock().unwrap().take() else {
            return;
        };
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
    }
}
