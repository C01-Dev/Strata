mod init;
mod logging;

use clap::{Parser, Subcommand};

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
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init => init::run(),
    };
    if let Err(e) = result {
        eprintln!("strata: {e}");
        std::process::exit(1);
    }
}
