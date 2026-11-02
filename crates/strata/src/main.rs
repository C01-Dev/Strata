mod logging;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "strata",
    version,
    about = "Shared type checker and worktree runtime for parallel coding agents"
)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
