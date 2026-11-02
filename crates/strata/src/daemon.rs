//! `strata daemon start|stop|status`, and the daemon process itself.

use crate::{api, logging, ops};
use std::fs::{self, DirBuilder, File, OpenOptions, Permissions, TryLockError};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};
use strata_core::client::{self, SOCKET};
use strata_core::store::Store;
use strata_core::{Error, Result, strata_home};
use tokio::signal::unix::{SignalKind, signal};

/// `sun_path` holds 104 bytes on macOS (108 on Linux), including the trailing NUL.
const MAX_SOCKET_PATH: usize = 103;
const WAIT: Duration = Duration::from_secs(10);

pub fn start(foreground: bool) -> Result<()> {
    if foreground {
        return run();
    }
    if client::get("/v1/status").is_ok() {
        println!("already running");
        return Ok(());
    }
    let home = strata_home();
    create_home(&home)?;
    let out_path = home.join("logs/daemon.out");
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&out_path)?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["daemon", "start", "--foreground"])
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .process_group(0) // detach from the terminal's Ctrl-C
        .spawn()?;
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(status) = client::get("/v1/status") {
            println!("started (pid {})", status["pid"]);
            return Ok(());
        }
        if let Some(code) = child.try_wait()? {
            return Err(Error::Refused(format!(
                "the daemon exited ({code}); see {}",
                out_path.display()
            )));
        }
        if Instant::now() > deadline {
            return Err(Error::Refused(format!(
                "the daemon did not answer within {WAIT:?}; see {}",
                out_path.display()
            )));
        }
        sleep(Duration::from_millis(50));
    }
}

/// Waits for a daemon started elsewhere (by the service manager) to answer.
pub fn wait_ready() -> Result<()> {
    let deadline = Instant::now() + WAIT;
    while client::get("/v1/status").is_err() {
        if Instant::now() > deadline {
            let out = strata_home().join("logs/daemon.out");
            return Err(Error::Refused(format!(
                "the daemon did not answer within {WAIT:?}; see {}",
                out.display()
            )));
        }
        sleep(Duration::from_millis(50));
    }
    Ok(())
}

pub fn stop() -> Result<()> {
    match client::post("/v1/shutdown", serde_json::json!({})) {
        Err(Error::NotFound(_)) => {
            println!("not running");
            return Ok(());
        }
        r => r?,
    };
    let socket = strata_home().join(SOCKET);
    let deadline = Instant::now() + WAIT;
    while socket.exists() && Instant::now() < deadline {
        sleep(Duration::from_millis(50));
    }
    println!("stopped");
    Ok(())
}

pub fn status() -> Result<()> {
    let s = client::get("/v1/status")?;
    println!(
        "running (pid {}, version {})",
        s["pid"],
        s["version"].as_str().unwrap_or("?")
    );
    Ok(())
}

fn create_home(home: &Path) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home.join("logs"))?;
    Ok(())
}

/// Single-instance lock, held for the daemon's lifetime. The file holds the owner's pid.
fn lock(home: &Path) -> Result<File> {
    let path = home.join("daemon.lock");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0)?;
            write!(file, "{}", std::process::id())?;
            Ok(file)
        }
        Err(TryLockError::WouldBlock) => {
            let pid = fs::read_to_string(&path).unwrap_or_default();
            Err(Error::Refused(format!(
                "another Strata daemon is running (pid {}); stop it with `strata daemon stop`",
                pid.trim()
            )))
        }
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

fn run() -> Result<()> {
    let home = strata_home();
    create_home(&home)?;
    let socket = home.join(SOCKET);
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        return Err(Error::Refused(format!(
            "socket path {} is longer than {MAX_SOCKET_PATH} bytes; set STRATA_HOME to a shorter folder",
            socket.display()
        )));
    }
    let _lock = lock(&home)?;
    logging::init(&home, "daemon")?;
    let daemon = api::Daemon::new(Store::open(&home.join("state.db"))?, &home);
    if let Err(e) = ops::recover(&daemon) {
        tracing::error!("crash recovery: {e}");
    }
    // We hold the lock, so a socket file left behind belongs to a dead daemon.
    let _ = fs::remove_file(&socket);
    tokio::runtime::Runtime::new()?.block_on(async {
        let listener = tokio::net::UnixListener::bind(&socket)?;
        fs::set_permissions(&socket, Permissions::from_mode(0o600))?;
        tracing::info!(pid = std::process::id(), socket = %socket.display(), "daemon started");
        tokio::spawn(ops::maintain(daemon.clone()));
        let mut term = signal(SignalKind::terminate())?;
        let stop = daemon.clone();
        let checker = daemon.clone();
        axum::serve(listener, api::router(daemon))
            .with_graceful_shutdown(async move {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                    _ = stop.shutdown.notified() => {}
                }
            })
            .await?;
        checker.checker.shutdown().await;
        Ok::<_, Error>(())
    })?;
    let _ = fs::remove_file(&socket);
    tracing::info!("daemon stopped");
    Ok(())
}
