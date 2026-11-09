---
name: strata
description: Work alongside other coding agents on one TypeScript repo with Strata, which gives each agent its own git worktree and shares one type checker between them. Use when Strata's MCP tools (strata_diagnostics, strata_conflicts, strata_start_dev_server) are available, or when the user wants to run several agents on a TypeScript repo at once. Covers checking types without starting tsc, checking your changes against other agents' unmerged work before you report back, starting a dev server with its own URL, and setting Strata up for Claude Code, Codex or Cursor.
---

# Strata

Strata runs several coding agents on one TypeScript repo at once, each in its own git worktree. It shares one TypeScript checker (tsgo, TypeScript 7.1) between every worktree. In the background it checks every two agents' changes together for clashes before either is merged. It also gives each worktree a dev server with its own port and URL.

## When the Strata tools are available

If you have tools named `strata_*`, the Strata MCP server is connected, and this worktree is one agent's:

- **Type errors:** call `strata_diagnostics`, for one file, or with no `file` for the whole project. Use it instead of running `tsc`, `tsgo` or the repo's type-check script, even when the repo's own instructions name one. It answers from the shared checker in about a second. A separate `tsc` starts a new checker, which can take over 1 GB of memory.
- **Questions about the code:** `strata_references`, `strata_definition`, `strata_hover` and `strata_symbols` answer from the same checker. Lines and columns are 1-based.
- **Before you report back, call `strata_conflicts`.** It checks your changes against every other agent's unmerged changes. It reports type errors that appear only once both sets of changes are combined, and files you both changed that git cannot merge.
  - If it finds any, name the other agent, the file and the error in your summary.
  - Do not change your code to match work that is not merged yet. The user decides which branch lands first.
  - `strata_diagnostics` answers also carry clashes that Strata found in the background.
- **Dev server:** call `strata_start_dev_server` instead of running the dev command yourself. It returns this worktree's URL. `strata_logs` shows the server's output, and `strata_stop` stops it.
- **Commands:** `strata_run` runs a shell command under Strata's supervision, which counts toward this agent's memory budget.
- **State:** `strata_status` shows this worktree's branch, port, memory and checker state.

## When they are not

Strata may not be set up. Do not install software or change the user's configuration without asking. Tell the user what Strata would do for them and offer these steps.

Requirements:
- macOS (Apple silicon) or Linux, with git 2.36+ and Node.js 20+.
- For the shared checker, the repo's own TypeScript 7.1 or later: `npm install --save-dev typescript@7.1.0-beta`. Without it, worktrees and dev servers still work.

1. Install Strata, with one of these:
   - `brew install c01-dev/tap/strata`
   - `npm install -g stratagents`
   - `curl -fsSL https://strata.c01.dev/install.sh | sh`
2. In the repo, run `strata init`. It registers the repo and starts the background daemon.
3. Connect the MCP server for the agent:
   - **Claude Code:** `claude mcp add --transport stdio --scope project strata -- strata mcp`
   - **Codex:** `codex mcp add strata -- strata mcp`
   - **Cursor:** put `{ "mcpServers": { "strata": { "command": "strata", "args": ["mcp"] } } }` in `.cursor/mcp.json`.
4. Give each agent its own worktree: `strata new <name>` prints the worktree's path. Start the agent inside it, for example `cd "$(strata new fix-login)" && codex`.
5. `strata status` shows every agent, its memory and dev-server URL, and any clashes between agents' changes. `strata rm <name>` removes a worktree, and refuses if it has uncommitted changes.

More, including Claude Code's worktree hooks and Conductor: https://github.com/C01-Dev/strata#recipes
