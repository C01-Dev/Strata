# Contributing to Strata

Thanks for helping. This page gets you from nothing to a passing test run, then shows where things are and how to send a change.

Strata runs on macOS and Linux. On Windows, work inside WSL2 (Windows' built-in Linux), as described below.

## Set up

### macOS or Linux (Ubuntu, Debian)

```bash
git clone https://github.com/C01-Dev/Strata && cd Strata
```

```bash
scripts/dev-setup.sh
```

The script installs what is missing (build tools, Rust through rustup, Node.js 22, the checker's TypeScript), then runs the same checks as CI. The first build takes a few minutes. On macOS, install the Xcode Command Line Tools (`xcode-select --install`), Rust from [rustup.rs](https://rustup.rs) and Node 20+ yourself, then run `npm ci --prefix packages/tsbridge` and `cargo test`.

### Windows, through WSL2

1. **Install WSL2 with Ubuntu.** Open PowerShell as administrator and run:

   ```powershell
   wsl --install -d Ubuntu-24.04
   ```

   Restart when asked, then open **Ubuntu** from the Start menu and pick a Linux user name and password.

2. **Clone into your Linux home, not a Windows drive.** In the Ubuntu window:

   ```bash
   cd ~ && git clone https://github.com/C01-Dev/Strata && cd Strata
   ```

   Keep the code under `~` (for example `/home/you/Strata`), never under `/mnt/c/...`. Windows drives are slow from WSL and do not behave like Linux disks; the setup script refuses to run there.

3. **Run the setup script.** It asks for the Linux password you just chose:

   ```bash
   scripts/dev-setup.sh
   ```

4. **Edit with your Windows editor.** VS Code with the WSL extension opens the Linux folder directly: run `code .` in the Ubuntu window. Run commands in the Ubuntu terminal, not PowerShell.

Two things that are normal on WSL:
- WSL's disk is ext4, which cannot clone folders. Each new worktree installs its dependencies instead of cloning them, so `strata new` is slower than on macOS. Setting `seed.fallback = "copy"` in a repo's `strata.toml` copies them from the base instead, which is usually faster than a fresh install.
- If systemd is off in your WSL distro, `strata init` cannot install its background service. The setup script tells you how to turn it on, or use `strata init --no-service`.

## Everyday commands

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

That is what CI runs, and a change needs all three to pass. While working on one part:

```bash
cargo test -p strata --test worktrees
```

runs one test file (`crates/strata/tests/worktrees.rs`); add a test name after `--test <file>` to run just that test. Four heavy tests are skipped by default: `cargo test -p strata --test nextjs -- --ignored` runs five Next.js dev servers, and the parity runs need `bench/setup.sh` first.

## Try Strata on a real project

```bash
cargo build --release
```

Then, in any git repo with a `package.json` (TypeScript 7.1 in its `node_modules` turns on the shared checker):

```bash
~/Strata/target/release/strata init --no-service
```

```bash
~/Strata/target/release/strata new try-it
```

```bash
~/Strata/target/release/strata status
```

If something looks wrong, `strata doctor --report` prints a summary with your paths removed. Paste it into an issue.

## Where things are

| Folder | What it holds |
| --- | --- |
| `crates/strata` | The `strata` binary: CLI, daemon, HTTP API, MCP server; end-to-end tests in `tests/` |
| `crates/strata-core` | Config (`strata.toml`), repo detection, the state store, the client the CLI uses |
| `crates/strata-git` | Worktree operations through the git CLI |
| `crates/strata-seed` | Cloning, copying and fixing up `node_modules` |
| `crates/strata-overlay` | Each worktree's changed files, by content hash |
| `crates/strata-checker` | The shared type checker: broker, Node bridge client, path translation |
| `crates/strata-proc` | Processes, memory metering, cleanup |
| `crates/strata-proxy` | The `.localhost` reverse proxy |
| `crates/strata-mcp` | The MCP tools agents call |
| `packages/tsbridge` | The Node script that talks to `tsgo` |
| `apps/macos` | The macOS menu bar app (Swift; macOS only) |
| `site` | strata.c01.dev: one page, updated on every push to `main` |
| `bench` | Benchmarks and their results |

`PLAN.md` is the design, and `NOTES.md` records each decision with the numbers measured.

## Send a change

1. Fork the repo on GitHub and clone your fork.
2. Make a branch: `git checkout -b short-name`.
3. Change the code, and add a test that fails without your change.
4. Run the three checks above.
5. Push and open a pull request against `main`. CI runs the same checks on macOS and Linux.

Keep a pull request to one change, and say in it what you saw before and after.

## Good first contributions

- **Run Strata on WSL2 or Linux x86_64 and report what happens.** It has been tested on macOS and on Linux arm64, and not yet on WSL. Open an issue with `strata doctor --report` and anything that surprised you.
- **Docs.** If a step in the README or this page did not work as written, fix it.
- **The website** (`site/index.html`), which needs nothing but a browser to check.
- **Later: Windows support.** It is planned for after v0.1 and is a big job: Windows process handling, named pipes in place of Unix sockets, and Windows' own file cloning on a Dev Drive. Open an issue to talk it through before starting.

## License

By contributing, you agree that your work is licensed under MIT OR Apache-2.0, like the rest of Strata.
