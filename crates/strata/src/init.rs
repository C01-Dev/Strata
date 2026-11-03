//! `strata init`: detect the repo's setup, write `strata.toml`, install the per-user service
//! (which starts the daemon) and register the repo (which creates the base worktree).

use crate::{daemon, service};
use serde_json::json;
use std::fs;
use strata_core::client;
use strata_core::config::{self, Config, to_dns_label};
use strata_core::{Result, detect};

pub fn run(no_service: bool) -> Result<()> {
    let root = strata_git::repo_root(&std::env::current_dir()?)?;
    let found = detect::detect(&root);
    let path = root.join(config::FILE);
    if path.exists() {
        Config::load(&root)?;
        println!("kept {}", path.display());
    } else {
        let folder = root.file_name().unwrap_or_default().to_string_lossy();
        let base = strata_git::default_base(&root)?;
        let text = config::template(
            &base,
            &config::default_worktree_dir(&root, &strata_core::strata_home()),
            found.dev_command.as_deref(),
            &to_dns_label(&folder),
        );
        fs::write(&path, text)?;
        println!("wrote {}", path.display());
    }
    println!(
        "package manager: {}",
        found.package_manager.map_or("none found", |pm| pm.name())
    );
    println!(
        "typescript: {}",
        found.typescript.as_deref().unwrap_or("not installed")
    );
    match found.checker_off_reason() {
        Some(reason) => println!("checker: off. {reason}"),
        None => println!("checker: on"),
    }
    println!(
        "dev command: {}",
        found
            .dev_command
            .as_deref()
            .unwrap_or("none (set dev.command in strata.toml)")
    );
    if no_service {
        if client::get("/v1/status").is_err() {
            daemon::start(false)?;
        }
    } else {
        // The service manager owns the daemon from now on; a hand-started one would hold the lock.
        if client::get("/v1/status").is_ok() {
            daemon::stop()?;
        }
        println!("service: {}", service::install()?.display());
        daemon::wait_ready()?;
    }
    let registered = client::post("/v1/repos", json!({ "root": root }))?;
    println!(
        "base worktree: {} (dependencies: {})",
        registered["base_worktree"].as_str().unwrap_or("?"),
        registered["seed"].as_str().unwrap_or("?")
    );
    for w in registered["warnings"].as_array().into_iter().flatten() {
        eprintln!("warning: {}", w.as_str().unwrap_or_default());
    }
    Ok(())
}
