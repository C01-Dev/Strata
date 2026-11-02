mod api;
mod client;
mod daemon;
mod init;
mod logging;

use clap::{Parser, Subcommand};
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
    /// Detect the repo's package manager, TypeScript and dev command, and write strata.toml
    Init,
    /// Manage the background daemon
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
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
        /// Run in this process instead, logging to the terminal's stdout and STRATA_HOME/logs
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
        Command::Status { json } => status(json),
    };
    if let Err(e) = result {
        eprintln!("strata: {e}");
        std::process::exit(1);
    }
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
