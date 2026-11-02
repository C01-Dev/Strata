mod api;
mod client;
mod daemon;
mod init;
mod logging;
mod ops;

use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use strata_core::Result;

#[derive(Parser)]
#[command(
    name = "strata",
    version,
    about = "Shared type checker and worktree runtime for parallel coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Detect the repo's setup, write strata.toml, start the daemon and create the base worktree
    Init,
    /// Manage the background daemon
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Create a locked worktree for an agent and print its path
    New {
        name: String,
        /// Start the new branch here (default: the base branch)
        #[arg(long)]
        from: Option<String>,
        /// Branch name (default: the agent name); an existing branch is checked out as is
        #[arg(long)]
        branch: Option<String>,
    },
    /// Register a worktree made by another tool
    Adopt { path: PathBuf },
    /// Remove an agent's worktree and, if merged, its branch
    Rm {
        name: String,
        /// Discard uncommitted changes
        #[arg(long)]
        force: bool,
        /// Keep the branch even if it is merged
        #[arg(long)]
        keep_branch: bool,
    },
    /// Show repos and agents
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DaemonCommand {
    /// Start the daemon in the background
    Start {
        /// Run in this process instead
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the daemon
    Stop,
    /// Say whether the daemon is running
    Status,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init => init::run(),
        Command::Daemon {
            command: DaemonCommand::Start { foreground },
        } => daemon::start(foreground),
        Command::Daemon {
            command: DaemonCommand::Stop,
        } => daemon::stop(),
        Command::Daemon {
            command: DaemonCommand::Status,
        } => daemon::status(),
        Command::New { name, from, branch } => new(name, from, branch),
        Command::Adopt { path } => adopt(path),
        Command::Rm {
            name,
            force,
            keep_branch,
        } => rm(name, force, keep_branch),
        Command::Status { json } => status(json),
    };
    if let Err(e) = result {
        eprintln!("strata: {e}");
        std::process::exit(1);
    }
}

fn repo() -> Result<PathBuf> {
    strata_git::repo_root(&std::env::current_dir()?)
}

fn new(name: String, from: Option<String>, branch: Option<String>) -> Result<()> {
    let out = client::post(
        "/v1/agents",
        json!({ "repo": repo()?, "name": name, "from": from, "branch": branch }),
    )?;
    for w in out["warnings"].as_array().into_iter().flatten() {
        eprintln!("warning: {}", w.as_str().unwrap_or_default());
    }
    println!("{}", out["agent"]["path"].as_str().unwrap_or_default());
    Ok(())
}

fn adopt(path: PathBuf) -> Result<()> {
    let out = client::post(
        "/v1/agents/adopt",
        json!({ "path": std::path::absolute(path)? }),
    )?;
    println!(
        "adopted {} as {}",
        out["agent"]["path"].as_str().unwrap_or("?"),
        out["agent"]["name"].as_str().unwrap_or("?")
    );
    Ok(())
}

fn rm(name: String, force: bool, keep_branch: bool) -> Result<()> {
    let body = json!({ "repo": repo()?, "force": force, "keep_branch": keep_branch });
    let out = client::request("DELETE", &format!("/v1/agents/{name}"), Some(body))?;
    println!("removed {name}");
    for note in out["notes"].as_array().into_iter().flatten() {
        println!("note: {}", note.as_str().unwrap_or_default());
    }
    Ok(())
}

fn status(json: bool) -> Result<()> {
    let s = client::get("/v1/status")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
        return Ok(());
    }
    println!(
        "daemon pid {}, version {}",
        s["pid"],
        s["version"].as_str().unwrap_or("?")
    );
    let repos = s["repos"].as_array().cloned().unwrap_or_default();
    if repos.is_empty() {
        println!("no repos; run `strata init` in one");
    }
    for repo in repos {
        println!(
            "\n{} (base {})",
            repo["root"].as_str().unwrap_or("?"),
            repo["base_branch"].as_str().unwrap_or("?")
        );
        let agents = repo["agents"].as_array().cloned().unwrap_or_default();
        if agents.is_empty() {
            println!("  no agents");
        }
        for a in agents {
            let field = |k: &str| a[k].as_str().unwrap_or("?").to_string();
            println!(
                "  {:<20} {:<9} {:<24} {}",
                field("name"),
                field("state"),
                field("branch"),
                field("path")
            );
        }
    }
    Ok(())
}
