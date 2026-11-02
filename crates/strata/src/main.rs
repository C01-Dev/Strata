mod api;
mod bench;
mod daemon;
mod init;
mod logging;
mod ops;
mod ports;
mod procs;
mod service;

use clap::{Parser, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use strata_core::{Result, client};

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
    /// Detect the repo's setup, write strata.toml, install the service and create the base worktree
    Init {
        /// Do not install the per-user service; start the daemon directly
        #[arg(long)]
        no_service: bool,
    },
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
    /// Print diagnostics for an agent's worktree from the shared checker
    Check {
        name: String,
        /// One file (relative to the worktree); default: the whole project
        file: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// MCP server on stdio for an agent (put `strata mcp` in the agent's MCP config)
    Mcp {
        /// Agent name, when neither MCP roots nor the working directory identify it
        #[arg(long)]
        agent: Option<String>,
    },
    /// Memory benchmark (Phase 0 protocol): a separate tsgo per worktree vs the shared checker
    Bench {
        /// Fixture folder with base/ and wt1..wtN/ (bench/setup.sh)
        fixture: PathBuf,
        /// Project to check, relative to the checkout
        #[arg(long, default_value = "tsconfig.json")]
        tsconfig: String,
        #[arg(long, default_value_t = 5)]
        agents: usize,
        /// whole: every agent checks the whole project at once; files: each checks its edited files
        #[arg(long, default_value = "whole", value_parser = ["whole", "files"])]
        workload: String,
        #[arg(long, value_delimiter = ',', default_value = "baseline,shared")]
        modes: Vec<String>,
        /// Extra readings this many seconds after the last query, e.g. 60,900
        #[arg(long, value_delimiter = ',')]
        idle: Vec<u64>,
        /// Append results to this CSV (same columns as bench/results/phase0.csv)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Start the agent's dev server (dev.command or the `dev` script) and print its URL
    Dev {
        name: String,
        /// Command instead of the configured one
        #[arg(long)]
        cmd: Option<String>,
    },
    /// Run a command in an agent's worktree under supervision; prints its output, exits with its code
    Run {
        name: String,
        /// Stop it after this many seconds
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Print the end of a process's log (default: the agent's newest process)
    Logs {
        name: String,
        /// Process id from `strata status`
        process: Option<i64>,
        #[arg(long, default_value_t = 200)]
        lines: usize,
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
    /// Install the per-user service (LaunchAgent or systemd --user) that runs the daemon
    Install,
    /// Remove the per-user service
    Uninstall,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init { no_service } => init::run(no_service),
        Command::Daemon { command } => match command {
            DaemonCommand::Start { foreground } => daemon::start(foreground),
            DaemonCommand::Stop => daemon::stop(),
            DaemonCommand::Status => daemon::status(),
            DaemonCommand::Install => {
                service::install().map(|f| println!("installed {}", f.display()))
            }
            DaemonCommand::Uninstall => service::uninstall().map(|()| println!("uninstalled")),
        },
        Command::New { name, from, branch } => new(name, from, branch),
        Command::Adopt { path } => adopt(path),
        Command::Rm {
            name,
            force,
            keep_branch,
        } => rm(name, force, keep_branch),
        Command::Check { name, file, json } => check(name, file, json),
        Command::Mcp { agent } => tokio::runtime::Runtime::new()
            .map_err(Into::into)
            .and_then(|rt| rt.block_on(strata_mcp::serve(agent))),
        Command::Bench {
            fixture,
            tsconfig,
            agents,
            workload,
            modes,
            idle,
            out,
        } => {
            let args = bench::Args {
                fixture,
                tsconfig,
                agents,
                workload,
                modes,
                idle,
                out,
            };
            tokio::runtime::Runtime::new()
                .map_err(Into::into)
                .and_then(|rt| rt.block_on(bench::run(args)))
        }
        Command::Dev { name, cmd } => dev(name, cmd),
        Command::Run { name, timeout, cmd } => run(name, timeout, cmd),
        Command::Logs {
            name,
            process,
            lines,
        } => logs(name, process, lines),
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
    // stderr: stdout is the path alone, for `cd "$(strata new x)"`.
    eprintln!("dependencies: {}", out["seed"].as_str().unwrap_or("?"));
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

/// Joins arguments into one `sh -c` command, quoting those the shell would split or expand.
fn shell_join(args: &[String]) -> String {
    let safe = |a: &str| {
        !a.is_empty()
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c))
    };
    args.iter()
        .map(|a| {
            if safe(a) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn dev(name: String, cmd: Option<String>) -> Result<()> {
    let out = client::post(
        &format!("/v1/agents/{name}/procs"),
        json!({ "repo": repo()?, "kind": "dev", "cmd": cmd }),
    )?;
    let note = if out["already_running"] == true {
        " (already running)"
    } else {
        ""
    };
    println!("{}{note}", out["url"].as_str().unwrap_or("?"));
    println!("direct: {}", out["direct_url"].as_str().unwrap_or("?"));
    println!(
        "process {}: {}",
        out["process"]["id"],
        out["process"]["cmd"].as_str().unwrap_or("?")
    );
    Ok(())
}

fn run(name: String, timeout: u64, cmd: Vec<String>) -> Result<()> {
    let cmd = if cmd.len() == 1 {
        cmd[0].clone()
    } else {
        shell_join(&cmd)
    };
    let out = client::post(
        &format!("/v1/agents/{name}/procs"),
        json!({ "repo": repo()?, "kind": "run", "cmd": cmd, "timeout_s": timeout }),
    )?;
    for line in out["output"].as_array().into_iter().flatten() {
        println!("{}", line.as_str().unwrap_or_default());
    }
    if out["timed_out"] == true {
        eprintln!("strata: stopped after {timeout} s");
        std::process::exit(124);
    }
    std::process::exit(out["exit_code"].as_i64().unwrap_or(1) as i32);
}

fn logs(name: String, process: Option<i64>, lines: usize) -> Result<()> {
    let id = match process {
        Some(id) => id,
        None => {
            let status = client::get("/v1/status")?;
            let root = repo()?;
            let repo = status["repos"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|r| r["root"] == root.to_string_lossy().as_ref());
            let agent = repo.and_then(|r| {
                r["agents"]
                    .as_array()?
                    .iter()
                    .find(|a| a["name"] == name.as_str())
                    .cloned()
            });
            let agent = agent.ok_or_else(|| {
                strata_core::Error::NotFound(format!(
                    "no agent named {name:?}; see `strata status`"
                ))
            })?;
            agent["processes"][0]["id"].as_i64().ok_or_else(|| {
                strata_core::Error::NotFound(format!("{name} has no processes yet"))
            })?
        }
    };
    let out = client::get(&format!("/v1/procs/{id}/logs?lines={lines}&agent={name}"))?;
    for line in out["lines"].as_array().into_iter().flatten() {
        println!("{}", line.as_str().unwrap_or_default());
    }
    Ok(())
}

/// Exits 1 when there are errors, like tsc.
fn check(name: String, file: Option<String>, json: bool) -> Result<()> {
    let out = client::post(
        &format!("/v1/agents/{name}/check"),
        json!({ "repo": repo()?, "op": "diagnostics", "file": file }),
    )?;
    if json {
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else {
        println!("{}", strata_mcp::format::diagnostics(&out));
    }
    if out["items"]
        .as_array()
        .is_some_and(|items| items.iter().any(|d| d["category"] == "error"))
    {
        std::process::exit(1);
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
            if let Some(tier) = a["seed_tier"].as_str() {
                println!("      dependencies: {tier}");
            }
            if a["memory_bytes"].as_u64().unwrap_or(0) > 0 {
                let note = match a["budget"].as_str() {
                    Some("over") => " (over budget)",
                    Some("warning") => " (near budget)",
                    _ => "",
                };
                println!(
                    "      memory {} MB{note}",
                    a["memory_bytes"].as_u64().unwrap_or(0) / 1048576
                );
            }
            let running = a["processes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["state"] == "running");
            for p in running {
                let kind = p["kind"].as_str().unwrap_or("?");
                println!(
                    "      process {} ({kind}, pid {}): {}",
                    p["id"],
                    p["pid"],
                    p["cmd"].as_str().unwrap_or("?")
                );
            }
        }
    }
    Ok(())
}
