# Parallel Agent Runtime: Research & Build Plan

Nov 1, 2026 · @Seif Elkady

## Summary and verdict

**Verdict: go, but narrowed.** Half of the original idea already exists as free tools. The part that matters most, one shared TypeScript type checker serving every agent's worktree, has not been built or announced by anyone we could find, and it became technically possible only in October 2026.

| Layer | Status as of Nov 2026 | Decision |
| --- | --- | --- |
| Copy-on-write node\_modules per worktree | Built many times: [Worktrunk](https://github.com/max-sixty/worktrunk) (Rust, 8.3k stars), cow-cli, parhel, and [pnpm's global virtual store](https://pnpm.io/git-worktrees) | Integrate, don't rebuild |
| Port broker and `<name>.localhost` routing | Built: [Vercel Labs Portless](https://github.com/vercel-labs/portless), portree | Integrate, don't rebuild |
| Shared parse and transform cache | Saves seconds per worktree at most: oxc parses a large TSX file in about 4 ms | Drop |
| One type checker serving N worktrees | Not built, not announced by any vendor or open-source tool found | **Build this. It is the product.** |

**Why now.** [TypeScript 7.1 Beta](https://devblogs.microsoft.com/typescript/announcing-typescript-7-1-beta/) (Oct 20, 2026) ships an API that holds several independent snapshots in one process, each with a filesystem layer checked before the base. That is exactly "one base plus many overlays". Users are already asking for the outcome: [Claude Code issue #87301](https://github.com/anthropics/claude-code/issues/87301) (Aug 2026) requests one language server per project root across sessions, after measuring 2 to 4 GB per session.

**Biggest unknown.** tsgo shares parsed files across snapshots but gives each program its own type checkers, so the real saving could be large or modest. Which language features the beta API offers per snapshot is also unverified. Phase 0 is a one-week benchmark that measures it before anything else is built. Under 40% saved at 5 worktrees, the owner decides whether to continue; under 20%, pivot (see Gate 0 and Risks).

**Working name: Strata** (one base layer, one thin layer per agent). The name has not been checked for conflicts yet.

## The problem in numbers

Every parallel agent runs its own copy of the same toolchain, and the TypeScript language server alone costs roughly 0.5 to 4 GB per copy.

| What was measured | Result | Source |
| --- | --- | --- |
| tsserver per Claude Code session | 0.5 to 4.2 GB across projects, 2 to 4 GB typical; two sessions on one repo about 6.6 GB; one runaway instance reached 61.5 GB of commit charge | [claude-code #87301](https://github.com/anthropics/claude-code/issues/87301), Aug 2026 |
| One worktree at rest, PostHog's coherence | About 614 MB, of which tsserver 485 MB; tsserver alone grows to 0.7 to 1.4 GB | [PostHog/coherence #170](https://github.com/PostHog/coherence/issues/170), Oct 31, 2026 |
| Four parallel agents on a 16 GB machine | Pushed fully into swap | Same issue |
| Next.js dev server, vercel.com dashboard | 21.5 GB before Next.js 16.3, 2 GB after | [Next.js 16.3 post](https://nextjs.org/blog/next-16-3-turbopack), Jun 29, 2026 |

**Rough total (estimate, not measured):** at 5 agents, 2 to 4 GB of tsserver each is 10 to 20 GB spent on type checking alone, much of it on identical code. That is an upper bound: how much is truly shareable is what Phase 0 measures.

**Who has this problem.** A [JetBrains survey of 15,000+ developers](https://blog.jetbrains.com/research/2026/08/ai-coding-agent-adoption-2026/) (May to Jul 2026) found 90% use coding agents weekly and 68% daily, with Claude Code at 39%. No major survey asks how many agents people run at once. A small, self-selected [Product Hunt poll](https://www.producthunt.com/p/claude/how-many-claude-codes-do-you-run-in-parallel?page=2) found 81% run 2 or more Claude Code sessions in parallel. Claude Code added `--worktree` around February 2026, and pnpm now documents worktrees for parallel agents.

**What is shrinking, and what is not.**

- TypeScript 7 is 8 to 12x faster but cuts build memory only about 18% (VS Code repo: 5.2 to 4.2 GB).
- Next.js 16.3 cut dev-server memory by roughly 80 to 90% in Vercel's two published examples.
- pnpm's shared store and the clone tools above already solve most of the disk cost.
- Many dramatic GB figures in issues are leaks, not steady state.

The durable problem is duplication: N copies of the same type checker for N agents. No single vendor fixes that, because each tool optimizes one copy of itself. That is the pitch.

## Prior art: what already exists

Nobody ships a type checker shared across worktrees. Almost everything around it already exists, mostly built in 2026. Star counts were read from GitHub on Nov 1, 2026.

| Category | Main tools | What they do | Overlap with Strata |
| --- | --- | --- | --- |
| Parallel-agent orchestrators | [Conductor](https://www.conductor.build/docs/reference/scripts) ($22M Series A, Mar 2026), [Vibe Kanban](https://github.com/BloopAI/vibe-kanban) (28.1k, sunsetting), [Superset](https://github.com/superset-sh/superset) (14.9k), [Claude Squad](https://github.com/smtg-ai/claude-squad) (8.3k), [Emdash](https://github.com/generalaction/emdash) (6.0k), [Container Use](https://github.com/dagger/container-use) (4.1k), Crystal / Nimbalyst, Sculptor, [gtr](https://github.com/coderabbitai/git-worktree-runner) | Create a worktree or container, run a setup script, copy `.env`, hand out ports | None at runtime. These are integration targets, not competitors |
| Agent vendors' built-in worktrees | Claude Code (`--worktree`, opt-in `symlinkDirectories`), [Cursor](https://cursor.com/docs/configuration/worktrees) (caps at 25 worktrees, advises against symlinking deps), [Codex](https://learn.chatgpt.com/docs/environments/git-worktrees) (separate deps per worktree) | Isolate each agent; vendor effort goes into disk use | No vendor shares language servers or has announced it |
| Copy-on-write and shared deps | [Worktrunk](https://worktrunk.dev/step/) `wt step copy-ignored` (Rust, 8.3k), [parhel](https://github.com/myrk-kale/parhel), [cow](https://github.com/joeinnes/cow), Switchboard PR #107, agentbrew PR #371, [pnpm global virtual store](https://pnpm.io/git-worktrees), aube, Bun | Seed `node_modules` and build output into each worktree at near-zero disk | Direct overlap with the dependency layer. Commodity |
| Shared build caches | [Turborepo 2.8](https://turborepo.dev/blog/2-8) (shares `.turbo/cache` across worktrees), sccache `basedirs` | Reuse task outputs across checkouts | Overlaps the parse-cache layer, which we drop |
| LSP multiplexers | [lspmux](https://codeberg.org/p2502/lspmux), codescout (Rust, Kotlin), [typemux-cc](https://github.com/k-dash/typemux-cc), Serena, catenary-mcp, lspmux-rust-cc | Share one server per identical workspace path; catenary-mcp puts every root on one server as separate workspace folders | Closest to the core, but none shares a base plus per-worktree diff overlays across different roots |
| Port routing | [Portless](https://github.com/vercel-labs/portless) (Vercel Labs), [portree](https://github.com/fairy-pitta/portree), worktree-devservers, Conductor port ranges | Route `<branch>.localhost` to each dev server, with WebSocket and hot-reload forwarding | Overlaps routing. Portless's MCP deliberately cannot start dev servers |
| Memory workarounds | [coherence #170](https://github.com/PostHog/coherence/issues/170) (cap, LRU eviction, idle timeout), [agent-flock](https://dev.to/koenvg/when-worktrees-share-ram-gba) (serialize heavy commands), [Trigger.dev](https://trigger.dev/blog/parallel-agents-gitbutler) (dropped worktrees for GitButler) | Limit or avoid duplication | Proves demand. None shares anything |

**What nobody does yet:**

1. One type checker serving many worktrees whose files differ: a base plus one overlay per agent.
2. A daemon that owns each agent's dev servers and test runs, enforces a per-agent memory budget, and hands the agent a URL through MCP.
3. Clone seeding done properly: picking a donor by lockfile hash, fixing absolute paths and timestamps automatically, and telling the user which tier (clone, hardlink, copy) they got. This is a quality gap, not an existence gap.

An agent vendor could build item 1 for its own agent. An agent-agnostic tool still has room, because no vendor has a reason to build shared infrastructure for competitors' agents. Private or unannounced work cannot be ruled out.

## Technical feasibility, layer by layer

Every layer except the checker is buildable on stable APIs. The checker works only through TypeScript 7.1's beta snapshot API, whose per-snapshot hover, definition, references and symbols are not yet verified; the LSP route and classic tsserver both fail.

### Shared type checker (the core)

**What does not work:**

- **Classic tsserver (TS 6 and earlier):** it keeps one text per path and updates shared parsed files in place, so two worktrees cannot hold different contents for the same file ([documentRegistry.ts](https://github.com/microsoft/TypeScript/blob/050880ce59e30b356b686bd3144efe24f875ebc8/src/services/documentRegistry.ts)).
- **tsgo through LSP:** one overlay map for the whole process, a single working directory, and no way to say a file was deleted ([server.go](https://github.com/microsoft/TypeScript/blob/6161772c1370bdd4a82f808e6823bd755eb1870d/tsc/internal/lsp/server.go#L1748)).

**What works: the TypeScript 7.1 API** ([7.1 Beta post](https://devblogs.microsoft.com/typescript/announcing-typescript-7-1-beta/)):

- `api.createSnapshot` makes an independent snapshot; `snapshot.update(...)` derives a new one.
- `createFileSystemLayer(files)` overrides selected files and falls back to the base snapshot or the disk. Added files appear in directory listings, and `removedPaths` hides deleted files.
- The server speaks MessagePack over stdio or a Unix socket. A `callbackFS` mode lets the client answer `readFile`, `fileExists` and directory listings itself, which is an alternative way to serve overlays.
- Snapshots must be disposed, or memory leaks on the Go side.

**What actually gets shared.** tsgo's parse cache is keyed by file name, path, an xxh3 content hash and a few module-detection options ([parsecache.go](https://github.com/microsoft/typescript-go/blob/89d5d5b2849a0db0957065889ca58536fa6d2e4a/internal/project/parsecache.go#L15)). Identical content at a different path is not shared, so every worktree's files must be presented at the base checkout's paths. Parsing and binding are shared; type-checker state is not. Each program has its own checker pool (default 4). Diagnostics and temporary checkers are freed after 30 s idle, but the API checker never is, so an API snapshot keeps its checker memory until it is disposed ([checkerpool.go](https://github.com/microsoft/typescript-go/blob/89d5d5b2849a0db0957065889ca58536fa6d2e4a/internal/project/checkerpool.go#L17)).

**The design.** A checker group is one tsgo process plus one base snapshot, keyed by repo, resolved TypeScript version and `dep_key` (a hash of the lockfile, every `package.json`, workspace and package-manager config such as `pnpm-workspace.yaml`, `.npmrc` and `.yarnrc.yml`, the `patches/` folder, and the Node ABI and platform). The base is a clean worktree that Strata owns at `<worktree_dir>/.base`, detached at the head of the `base` branch, so the user's own edits and branch switches never disturb agents. Each agent gets its own snapshot whose layer is that worktree's content-hash diff against base, re-rooted at base paths, with deletions in `removedPaths`. Every path in requests and responses is translated between base and worktree paths.

**The trap in re-rooting.** Every import then resolves into the base's `node_modules`. Anything that differs per worktree under the same `dep_key` (a generated Prisma client, patch output, a workspace package's built `dist/` that its `types` field points at) must be listed in `checker.extra_paths` and hashed into the layer; otherwise the agent is marked unshared. An agent whose `dep_key` differs gets an unshared snapshot at its own worktree paths in the same process, reported as "sharing: none (dependencies differ)". Nested worktrees, such as Claude Code's `.claude/worktrees/`, are excluded from base scanning and listed in the base snapshot's `removedPaths`.

| Change in a worktree | Handled by one shared process? | How |
| --- | --- | --- |
| Edit to a tracked file | Yes | File goes in the agent's layer |
| Added file | Yes | Layer merges directory listings, so `include` globs see it |
| Deleted or renamed file | Yes | `removedPaths` (plus the new path in the layer) |
| Untracked or generated file | Yes | Diff the filesystem by content hash, not `git diff` |
| `tsconfig` change | Usually | Separate program inside that snapshot; parsed files still shared unless JSX or module-detection options change |
| `package.json` `exports` or `type` change | Usually | Resolved per snapshot (per-snapshot resolution caches not yet verified) |
| Different lockfile or `node_modules` | No | Unshared snapshot at the worktree's own paths (different dep\_key) |
| Different TypeScript version | No | Separate process |
| `tsc --build` with `.tsbuildinfo` | No | Run per worktree; out of scope for the checker |

**Open questions the Phase 0 spike must answer:**

- How tsgo's memory splits between shared syntax trees and per-snapshot checker state. Nothing is published.
- Which language features (diagnostics, hover, definition, references, completions, rename) the API offers per snapshot, and how many snapshots can run at once.
- Whether a native Rust client is practical. No Rust client exists, and the protocol is in beta, so the plan starts with a small Node sidecar using the official `typescript/async` client.
- Only the 7.1 Beta exists today; the RC is expected about a month after Oct 20, 2026. Strata uses the repo's own TypeScript (7.1 or later); the bridge pins the exact prerelease for its own tests, and the parity suite reruns on every TypeScript release.

### Copy-on-write seeding

- **macOS:** one `clonefile(2)` call clones a whole tree atomically, though Apple's man page discourages it; `copyfile(3)` with `COPYFILE_CLONE` and `COPYFILE_RECURSIVE` is the sanctioned route and can skip subtrees like `.git`. Real timing: 213k files cloned in 13.4 s versus a 44 s fresh install on an M4 Pro ([pnpm #14041](https://github.com/orgs/pnpm/discussions/14041)).
- **Linux:** `ioctl(FICLONE)` clones single files on reflink-capable filesystems (btrfs, XFS with reflink=1, OpenZFS 2.2+ with block cloning), so the daemon walks the tree itself. ext4 cannot clone: fall back to a real install by default, with copy and hardlink as opt-in tiers.
- **Fix-ups:** rewrite absolute paths in `node_modules/.pnpm-workspace-state-v1.json`, reset `lastValidatedTimestamp`, and accept a donor only when lockfile and manifests are byte-identical. A stale donor took 78 s, slower than a fresh install.
- **Trap:** cloning a `node_modules` made of symlinks into a global store copies the links, not the files, so there is no isolation.
- **Unknown:** nobody has confirmed that cloned files share RAM page cache. Sell this layer on disk and setup time, never on memory.
- Pre-warm a small pool of seeded worktrees off the critical path, because cloning is metadata-bound, not instant.

### Process ownership, memory budgets and routing

- **Process trees:** `process-wrap` for process groups. On Linux, a systemd-delegated cgroup v2 per agent gives an unescapable `cgroup.kill` plus `memory.current` and `memory.peak`. macOS has no cgroups: sum `phys_footprint` (Activity Monitor's figure) over all descendants via `proc_pid_rusage`, because `sysinfo` reports RSS instead.
- **Ports:** the daemon owns a registry, assigns ports, then confirms the real listener on both `127.0.0.1` and `::1`. Probe-then-assign is likely to race when 5 agents start at once.
- **Routing:** `*.localhost` resolves to loopback in Chrome, Firefox and Edge; Safari works on macOS 26 but not 15.7 ([WebKit 160504](https://bugs.webkit.org/show_bug.cgi?id=160504)). Forward WebSockets with `hyper::upgrade` so hot reload works. Default to an unprivileged high port. Google and Apple OAuth reject `.localhost` redirect URIs.
- **Agent interface:** `rmcp` 3.8.0 implements MCP 2026-07-28. Because the daemon owns processes, an MCP call can start a dev server and return its URL immediately, which Portless cannot.

### Dropped: shared parse cache

oxc parses a large real TSX file in about 4 ms ([benchmark](https://github.com/oxc-project/bench-javascript-parser-written-in-rust)), so a cross-worktree parse cache saves seconds at most. Rust-built syntax trees also cannot feed TypeScript's Go checker. Turborepo 2.8 already shares task caches across worktrees.

## Product definition

Strata is a local Rust daemon that lets several coding agents work on one TypeScript repo while sharing a single type checker and handing their dev servers to one supervisor. **Target, to be confirmed in Phase 0:** type-checking memory for 5 agents at no more than 50% of 5 separate tsgo processes (about 2.5x one agent).

**Who it is for:**

- **Primary:** developers running 2 to 10 coding agents (Claude Code, Codex, Cursor, others) in parallel on a TypeScript repo, typically Next.js or a monorepo, on a 16 to 36 GB Mac or Linux machine.
- **Secondary:** builders of orchestrators (Conductor, Superset, Claude Squad, Emdash) who call Strata from their setup scripts or API instead of rebuilding it.
- **Later:** sandbox and CI hosts that run many agents per machine.

**Core flow:**

1. `strata init` in the repo writes `strata.toml`, creates Strata's own clean base worktree, installs the per-user background service and starts the daemon.
2. `strata new fix-login` creates a locked worktree, seeds `node_modules` by clone (reporting which tier it got), registers a checker snapshot and reserves a port.
3. The agent works in that folder. Its MCP config runs `strata mcp`, so it can call `strata_diagnostics`, `strata_hover`, `strata_definition`, `strata_references` and `strata_start_dev_server`, which returns a URL like `http://fix-login.myapp.localhost:7355`.
4. `strata status` shows each agent's memory, processes, URL and snapshot state, plus the total saved versus separate checkers.
5. `strata rm fix-login` stops its processes, disposes its snapshot and removes the worktree.

Worktrees created by other tools join with `strata adopt <path>`, for example from Conductor's setup script or after a plain `git worktree add`. Claude Code's `WorktreeCreate` and `WorktreeRemove` hooks replace its own git logic, so those hooks call `strata new` and `strata rm` instead.

**MVP scope (v0.1):**

- macOS on Apple silicon, Linux on x86\_64 and arm64.
- TypeScript and JavaScript repos using npm, pnpm, Bun, or Yarn with nodeLinker set to node-modules (in Yarn PnP repos the checker is off with a message), on TypeScript 7.1 or later.
- Shared checker exposed through MCP: diagnostics per file and per project, hover, go to definition, find references, document symbols. Each feature ships only if Phase 0 confirms it works per snapshot; diagnostics are the minimum.
- Worktree lifecycle with clone seeding, donor selection by lockfile hash and tier reporting.
- Process supervision for dev servers and any command, with per-agent memory reporting and budgets (warn, or stop that agent's processes).
- Port registry and a `.localhost` proxy with WebSocket forwarding.
- CLI, MCP server and a plain-text `strata status`.

**After v0.1:** a per-worktree LSP shim so editors share the checker too, completions and rename, Windows support, and other languages if the pattern proves out.

**Non-goals:**

- Not an orchestrator: no task queue, chat UI, review or merge flow.
- Not a package manager, and not a type checker. Strata never reimplements TypeScript; it drives Microsoft's tsgo.
- No support for classic tsserver (TS 6), because it cannot share.
- No containers or VMs, no cloud service, and no telemetry by default.

**Naming.** "Strata" is a working name. Before Phase 1, check crates.io, npm, Homebrew, the GitHub organization and the domain.

## Architecture

One Rust binary, `strata`, acts as the CLI, the MCP server and the background daemon. The daemon drives one tsgo API process per repository through a small Node bridge, and owns every worktree, process and port it hands out.

&#91;embedded content: Strata architecture · clients, daemon modules, external processes\]

Agents reach the daemon through MCP, you through the CLI, and the browser through the proxy. Only the checker broker is new technology; everything else is integration on mature crates.

### Components

| Component | Job | Key crates and APIs |
| --- | --- | --- |
| CLI and MCP shim | `strata <command>` and `strata mcp` turn requests into calls on the daemon's socket. The MCP shim identifies the agent from MCP roots, then its working directory, then STRATA\_AGENT; if none match it fails rather than falling back to base | `clap`, `rmcp` 3.8.0 |
| Control API | JSON over HTTP/1.1 on a Unix socket at `$STRATA_HOME/strata.sock` (default `~/.strata`) | `axum` 0.8, `tokio` 1.54 |
| Worktree manager | Shells out to `git worktree add --lock`, `list --porcelain -z`, `remove`, `prune`. Serializes repo-wide git operations | `tokio::process`; `gix` 0.90 for read-only queries |
| Seeder | Picks a donor by dep\_key, clones each node\_modules folder, falls back to a real install by default (copy and hardlink are opt-in), applies fix-ups, keeps a warm pool | `reflink-copy` 0.1.31, `libc` (`clonefile`), `rustix` (`FICLONE`) |
| Overlay tracker | Watches each worktree's source folders (never `node_modules`), with one non-recursive watch per directory (skipping .git, build output and other worktrees), keeps content hashes, computes the layer: changed and added files plus removed paths versus base, plus any checker.extra\_paths | `notify` 8.2 + `notify-debouncer-full`, `xxhash-rust` |
| Checker broker | One checker group (one tsgo process plus a base snapshot) per repo, resolved TypeScript version and dep\_key. Holds the base snapshot and one snapshot per agent, translates paths both ways, disposes idle snapshots | Node bridge (`packages/tsbridge`) on the official `typescript/async` client; JSON lines over stdio |
| Process supervisor | Starts commands in the agent's worktree with `PORT` and `STRATA_AGENT` set, tracks the whole process tree, writes logs, stops trees | `process-wrap` 10; `cgroups-rs` on Linux; `libproc` on macOS |
| Memory meter | Samples memory per agent every 2 s and enforces budgets: warn first, then stop that agent's dev and test processes | cgroup `memory.current`; macOS `phys_footprint` |
| Port registry and proxy | Assigns ports from a range, confirms the real listener, routes `<agent>.<app>.localhost:<proxy_port>` with WebSocket upgrade | `hyper` 1.12, `hyper-util`, `listeners` 0.6 |
| State store | Durable record of repos, agents, snapshots, processes, ports and donors. After a restart, re-adopts a process only when both its pid and start time match | `rusqlite` 0.41 in WAL mode |
| Service install | Per-user LaunchAgent on macOS, `systemd --user` unit on Linux | `service-manager` 0.11 |

### Data model

```sql
CREATE TABLE repos (id INTEGER PRIMARY KEY, root TEXT UNIQUE NOT NULL, base_branch TEXT NOT NULL, ts_version TEXT, created_at INTEGER NOT NULL);
CREATE TABLE agents (id INTEGER PRIMARY KEY, repo_id INTEGER NOT NULL REFERENCES repos(id), name TEXT NOT NULL, path TEXT NOT NULL, branch TEXT NOT NULL, dep_key TEXT, seed_tier TEXT, budget_mb INTEGER, state TEXT NOT NULL, created_at INTEGER NOT NULL, UNIQUE (repo_id, name));
CREATE TABLE snapshots (agent_id INTEGER PRIMARY KEY REFERENCES agents(id), group_key TEXT NOT NULL, layer_hash TEXT NOT NULL, changed_files INTEGER NOT NULL, removed_files INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE processes (id INTEGER PRIMARY KEY, agent_id INTEGER NOT NULL REFERENCES agents(id), kind TEXT NOT NULL, cmd TEXT NOT NULL, pid INTEGER, pid_start_time INTEGER, port INTEGER, state TEXT NOT NULL, log_path TEXT NOT NULL, started_at INTEGER, exit_code INTEGER);
CREATE TABLE ports (port INTEGER PRIMARY KEY, agent_id INTEGER REFERENCES agents(id), state TEXT NOT NULL);
CREATE TABLE donors (repo_id INTEGER NOT NULL REFERENCES repos(id), dep_key TEXT NOT NULL, path TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY (repo_id, dep_key));
CREATE TABLE events (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, agent_id INTEGER, kind TEXT NOT NULL, detail TEXT);
```

### CLI

| Command | What it does |
| --- | --- |
| `strata init` | Detects package manager, TypeScript version and dev command; writes `strata.toml`; installs and starts the service |
| `strata daemon start`, `stop`, `status` | Manages the background daemon |
| `strata new <name> [--from <ref>] [--branch <b>]` | Creates the worktree, seeds dependencies, opens a snapshot, reserves a port, prints the path |
| `strata adopt <path>` | Registers a worktree made by another tool |
| `strata rm <name> [--keep-branch]` | Stops processes, disposes the snapshot, removes the worktree |
| `strata status [--json]` | Agents, memory, processes, URLs, and memory saved versus separate checkers |
| `strata dev <name>` | Starts the configured dev server and prints its URL |
| `strata run <name> -- <cmd>` | Runs any command in that worktree under supervision |
| `strata logs <name> [process]` | Tails logs |
| `strata check <name> [file]` | Prints diagnostics from the shared checker |
| `strata doctor` | Checks clone support, inotify limits, TypeScript version, `.localhost` resolution and the socket |
| `strata gc` | Removes orphaned worktrees, donors, ports and stale snapshots |
| `strata mcp` | Stdio MCP server for agents |
| `strata bench` | Runs the memory benchmark from Phase 0, kept as a feature |

### MCP tools

Lines and columns are 1-based, with columns in UTF-16 code units. Paths inside the worktree are relative to it; files outside it, such as TypeScript's lib files, come back as absolute paths. Process tools act only on the calling agent's processes, and process\_id is Strata's id, not the OS pid. Whole-project diagnostics return at most 200 items plus a total count.

| Tool | Input | Returns |
| --- | --- | --- |
| `strata_diagnostics` | `file` (optional; omit for the whole project) | Errors and warnings with file, line, column, code, message |
| `strata_hover` | `file`, `line`, `column` | Type and documentation |
| `strata_definition` | `file`, `line`, `column` | Locations |
| `strata_references` | `file`, `line`, `column` | Locations |
| `strata_symbols` | `file` | Outline of declarations |
| `strata_start_dev_server` | `command` (optional) | URL, port and process id, returned immediately |
| `strata_run` | `command`, `timeout_s` | Exit code and the last 200 lines of output |
| `strata_stop` | `process_id` | Confirmation |
| `strata_logs` | `process_id`, `lines` | Log tail |
| `strata_status` | none | This agent's memory, processes, URL and checker state |

### Control API

```
POST   /v1/repos                    register or init a repo
GET    /v1/status                   full state as JSON
POST   /v1/agents                   {name, from?, branch?}
POST   /v1/agents/adopt             {path}
DELETE /v1/agents/{name}
POST   /v1/agents/{name}/check      {op: diagnostics|hover|definition|references|symbols, file?, line?, column?}
POST   /v1/agents/{name}/procs      {kind: dev|run, cmd?, timeout_s?}
DELETE /v1/procs/{id}
GET    /v1/procs/{id}/logs?lines=200
GET    /v1/events                   server-sent events stream
```

### Config file

```toml
[repo]
base = "main"                     # branch Strata's own base worktree follows
worktree_dir = "../.strata/myapp" # where agent worktrees live

[seed]
paths = ["node_modules", "apps/*/node_modules", "packages/*/node_modules"]
fallback = "install"              # after clone: install | copy | hardlink
warm_pool = 1

[checker]
typescript = "auto"              # the repo's own TypeScript; must be 7.1 or later
tsconfig = ["tsconfig.json"]
idle_dispose_secs = 900
huge_diff_ratio = 0.2             # above this share of changed files, the agent gets its own snapshot
group_memory_mb = 6144            # above this, dispose least recently used snapshots
extra_paths = ["node_modules/.prisma"]  # per-worktree files under node_modules to layer

[dev]
command = "pnpm dev"
port_env = "PORT"
restart = false                   # restart dev servers after a reboot
port_range = [4100, 4999]
proxy_port = 7355                 # not 1355, which Portless uses
app = "myapp"                     # gives <agent>.myapp.localhost

[budget]
per_agent_mb = 4096
action = "warn"                   # warn | stop
```

## Edge cases and failure modes

Each case below needs a test and a clear message. Rule for all of them: never lose a user's work silently, and when sharing degrades, say so in `strata status`.

| Area | Case | Expected behavior |
| --- | --- | --- |
| Git | Branch is already checked out in another worktree | Refuse; suggest a new branch name |
| Git | Worktree name or path already exists | Refuse; suggest another name |
| Git | User deletes a worktree folder by hand | Watcher or `gc` marks the agent orphaned, stops its processes, disposes its snapshot, runs `git worktree unlock` then `git worktree prune` |
| Git | `strata rm` on a worktree with uncommitted changes | Refuse unless `--force`; never discard changes silently |
| Git | Base branch moves (main is pulled) | Advance Strata's own base worktree, build the new base snapshot, re-create agent snapshots on it, then dispose the old base |
| Git | User edits or switches branches in their main checkout | No effect on agents; the base is Strata's own `.base` worktree |
| Git | Agent rebases or merges inside its worktree | Treated as file changes; layer recomputed |
| Git | Several agents run git at once (fetch, gc) | Strata serializes its own git calls; worktrees stay locked so a stray `prune` cannot remove them, and Strata unlocks before its own remove |
| Git | Worktrees nested inside the repo (Claude Code's `.claude/worktrees/`) | Excluded from base scanning and watching; listed in the base snapshot's `removedPaths` |
| Git | Repo uses submodules | Seed and track normally; warn if a submodule is not initialized; warm pool disabled |
| Git | Worktree created by Conductor, Claude Code or plain git | `strata adopt <path>`; for Claude Code, its `WorktreeCreate` hook calls `strata new` |
| Seeding | Filesystem cannot clone (ext4, NTFS) | Fall back per config; print the tier received in `strata new` and `strata status` |
| Seeding | No donor matches the `dep_key` | Run a real install in the worktree, then register it as the donor for that key. Never seed from a stale donor (measured slower than a fresh install) |
| Seeding | Agent changes dependencies mid-task | `dep_key` changes; the agent moves to an unshared snapshot at its own paths, or to a matching group; report it |
| Seeding | `node_modules` is pnpm's global virtual store (symlinks) | Do not clone; report tier "shared store, not isolated" |
| Seeding | Yarn PnP repo (no `node_modules`) | Checker and seeding off with a message; worktrees and processes still work |
| Seeding | State files with absolute paths | Rewrite known files (`.pnpm-workspace-state-v1.json`, `.bin` shims); reset `lastValidatedTimestamp` |
| Seeding | Native addons built against the donor's paths | Detect absolute donor paths in addon build files; mark the worktree "needs rebuild" and run the package manager's rebuild |
| Seeding | Disk fills during or after a clone (`ENOSPC`) | Abort, delete the partial folder, report free space needed |
| Seeding | Worktree folder on another volume (`EXDEV`) | `init` and `doctor` warn; fall back to install |
| Seeding | Timestamp-validated caches (`.tsbuildinfo`, `.next`) | Do not clone them by default; they rebuild per worktree |
| Checker | TypeScript older than 7.1 | Checker off for that repo with a clear message; every other feature still works |
| Checker | A language feature does not work per snapshot in the 7.1 API | That MCP tool is not offered; recorded at Phase 0 |
| Checker | Two agents edit the same file differently | Each snapshot has its own layer; no conflict |
| Checker | Agent adds, deletes or renames files | Added files go in the layer, deleted ones in `removedPaths` |
| Checker | Generated or gitignored files that tsconfig includes | Overlay tracker diffs the tsconfig include roots by content hash, not `git status` |
| Checker | Per-worktree files under `node_modules` with the same `dep_key` (generated Prisma client, patch output, a workspace package's built `dist/`) | Hash the paths in `checker.extra_paths` into the layer; if they differ and cannot be layered, mark the agent unshared |
| Checker | Agent's diff gets huge (above `huge_diff_ratio`, default 20% of project files) | Give that agent its own snapshot at its own paths; report reduced sharing |
| Checker | `tsconfig` changed in a worktree | Separate program inside that snapshot; report if sharing drops |
| Checker | Monorepo with several tsconfigs or project references | Load the projects listed in config; `tsc --build` stays per worktree |
| Checker | tsgo process crashes | Restart it, rebuild base, re-create snapshots from current layers; calls during restart return "checker restarting, retry in N s" |
| Checker | Checker memory grows under heavy queries | Dispose idle snapshots after `idle_dispose_secs`; above `group_memory_mb`, dispose least recently used first |
| Checker | Paths inside message text, not just location fields | Translate every known path field, then replace the base root prefix inside message strings |
| Checker | Symlinks and realpaths (pnpm) | Normalize base and worktree prefixes by realpath before translating |
| Checker | File outside every configured project | Return "file not in project", never an empty result |
| Checker | Agent tools that filter paths through `git check-ignore` | MCP returns paths relative to the agent's own worktree |
| Checker | MCP server cannot tell which agent is calling | Return an explicit error; never answer from base |
| Processes | Dev server ignores `PORT` | Per-framework flags (`--port` for Vite and Astro); confirm the real listener and update the route |
| Processes | Port already taken by an outside process | Registry skips it and assigns the next free port |
| Processes | A child escapes its process group | Linux: `cgroup.kill`. macOS and Linux without cgroups: walk descendants by parent pid and record every child pid |
| Processes | Daemon crashes or is upgraded | Reload state; re-adopt a process only if pid and start time both match; mark the rest dead and free their ports |
| Processes | Machine sleeps or reboots | Service restarts the daemon; dev servers restart only if `dev.restart = true` |
| Processes | Agent exceeds its memory budget | Warn through status, events and MCP; with `budget.action = "stop"`, stop that agent's dev and test processes, never the agent itself |
| Processes | Agent starts a dev server itself, outside Strata | `status` lists unmanaged listeners whose working directory is in a worktree and suggests `strata_start_dev_server` |
| Daemon | A second daemon starts | File lock; the second exits with a message |
| Daemon | Another local user or process reaches the socket or proxy | Socket file mode 0600; proxy binds loopback only |
| Daemon | Socket path longer than the OS limit (about 104 bytes) | Default to `~/.strata`; `doctor` checks the length |
| Routing | Another program (such as Portless) holds the proxy port | Daemon start and `doctor` fail with a clear message naming the port and process |
| Routing | Safari on macOS 15 cannot resolve `*.localhost` | `doctor` detects it and prints the `/etc/hosts` lines for the user to add |
| Routing | OAuth callback rejects `.localhost` | `strata dev` also prints the direct `http://localhost:<port>` URL |
| Platform | Linux inotify watch limit | One non-recursive watch per source directory; `doctor` shows usage and how to raise the limit |
| Platform | Linux without cgroup delegation | Memory from PSS over the process tree, tree kill by walking descendants; `doctor` explains how to enable delegation |
| Platform | Windows | Not supported in v0.1; exit with a clear message |

## Build plan

v0.1 is roughly 7 to 11 weeks of work for one developer driving a coding agent, and Phase 0 decides in the first week whether to continue.

&#91;embedded content: Build roadmap · 6 phases, 6 gates, then launch\]

Phase 2 builds the part nobody else has; every other phase is integration on mature crates. Task-level steps for each phase are in the agent execution guide below.

## AI agent execution guide

Export this doc as Markdown, commit it as `PLAN.md` in an empty repository, and give a coding agent the starter prompt below. The agent works phase by phase; every task ends with passing tests and a commit, and every phase ends at a gate it must report before continuing. The two drawings do not survive a Markdown export; the components table and the phase headings below carry the same information.

### Starter prompt

```
You are building Strata, a local Rust daemon described in PLAN.md. Read PLAN.md in full before writing code.
The "Edge cases and failure modes" table is your acceptance list.
Start with Phase 0, Task 0.1, and follow the ground rules exactly.
After each task: run fmt, clippy and tests, update NOTES.md, and commit.
At each phase gate, stop and report the gate result with measured numbers. Do not start the next phase until the owner gives an explicit go.
```

### Ground rules

1. Work one task at a time, in the order below.
2. Write tests first where practical. Before every commit run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`. Commit messages follow `phase-N/task-M: <summary>`.
3. Never reimplement TypeScript. All type information comes from tsgo through the bridge.
4. Pin versions: the Rust toolchain in `rust-toolchain.toml`, crate versions in `Cargo.toml`, the exact TypeScript version in `packages/tsbridge/package.json`.
5. Keep `NOTES.md` as a log of decisions, deviations from this plan and every measured number.
6. If a gate fails, stop and write the finding in `NOTES.md`. Never work around a failed gate silently.
7. Tests create temporary repos; no test touches anything outside its temp folder, except system tests (service install, loop-device mounts, proxy ports), which run only in CI or when STRATA\_SYSTEM\_TESTS=1 is set.
8. Use the `git` command line for worktree operations, not a git library.
9. Put platform code behind `cfg(target_os)` with one shared trait, and run CI on both macOS and Linux.
10. When this plan and reality disagree (an API behaves differently), trust reality, record it in `NOTES.md`, and adjust.

### Repository layout

```
strata/
  PLAN.md  NOTES.md  README.md  LICENSE-MIT  LICENSE-APACHE
  Cargo.toml                 # workspace
  rust-toolchain.toml
  crates/
    strata/                  # the single binary: CLI, daemon and mcp subcommands
    strata-core/             # config, shared types, errors, state store (rusqlite)
    strata-git/              # worktree operations through the git CLI
    strata-seed/             # clone, copy and install tiers, donors, fix-ups
    strata-overlay/          # watcher, hashing, layer computation
    strata-checker/          # broker, bridge client, path translation
    strata-proc/             # supervisor, memory meter, budgets
    strata-proxy/            # port registry and .localhost reverse proxy
    strata-mcp/              # MCP tool definitions (rmcp)
  packages/
    tsbridge/                # Node sidecar on typescript/async, TS 7.1 pinned
    npm/                     # npm launcher and per-platform packages
  bench/                     # benchmark harness and fixture scripts
  tests/
    fixtures/                # small TS repos: plain, monorepo, Next.js app
    e2e/                     # CLI and MCP end-to-end tests
  .github/workflows/         # ci.yml (macOS + Ubuntu), release.yml (dist)
```

### Phase 0: feasibility spike (1 week, go or no-go)

1. **0.1 Fixtures.** `bench/setup.sh` clones two real open-source TypeScript repos at pinned commits (candidates: cal.com as a large Next.js monorepo with 200k+ files in `node_modules`, excalidraw as a medium app), moves them to the TypeScript 7.1 Beta with minimal fixes recorded in `NOTES.md` (parity needs equal results, not zero errors), installs dependencies, and creates 5 worktrees, each with a scripted realistic diff: 5 files edited, 1 added, 1 deleted. Record the exact TypeScript version string.
2. **0.2 Baseline.** 5 tsgo API processes, each holding one snapshot of its own worktree at its own paths. Also run 5 tsgo language-server processes for the launch comparison.
3. **0.3 Shared.** One tsgo API process driven by a throwaway Node script: a base snapshot plus 5 snapshots whose filesystem layers hold each worktree's diff re-rooted at base paths, with deletions in `removedPaths`.
4. **0.4 Measurement protocol, identical for 0.2 and 0.3.** Run all 5 agents' queries concurrently: whole-project diagnostics, then hover on 20 fixed positions. Sample memory every 250 ms for the peak, then record warm (10 s after the last query) and idle (60 s and 15 min). Use the same `GOGC` and `GOMEMLIMIT` on both sides and count the Node client inside "shared". Memory means macOS `phys_footprint` summed over the process tree, or Linux PSS from `smaps_rollup`.
5. **0.5 Correctness.** Translate paths back and compare diagnostics per worktree: file, range, code and message must match exactly as sets.
6. **0.6 Feature probe.** List which calls work per snapshot (diagnostics, hover, definition, references, symbols, completions, rename) and the snapshot count at which errors or slowdowns appear.
7. **0.7 Report.** `bench/REPORT.md` with a CSV of every measurement and a short conclusion.

**Gate 0, the single source for go or no-go.** S = 1 − shared ÷ baseline, computed on both the peak and the warm readings at 5 worktrees. Pass: S of at least 40% on both, no diagnostic mismatch after path translation, and per-snapshot diagnostics available; then stop and wait for the owner's go. S between 20% and 40%: stop and report; the owner decides. S under 20%, a mismatch not caused by path translation, or no per-snapshot diagnostics: stop and see Risks. Any other feature that does not work per snapshot is dropped from v0.1's MCP tools and recorded.

### Phase 1: daemon, state and worktree lifecycle (1 to 2 weeks)

1. **1.1 Skeleton.** Cargo workspace, CI (fmt, clippy, test and `cargo deny` on `macos-latest` and `ubuntu-latest`), one error type (`thiserror`), logging with `tracing` to `$STRATA_HOME/logs`.
2. **1.2 Config and init.** Parse and validate `strata.toml` with `serde` and `toml`. `strata init` detects the package manager from the lockfile (turning the checker off for Yarn PnP), resolves TypeScript from the repo's `node_modules`, and reads the dev script from `package.json`.
3. **1.3 State store.** Migrations for the data model above, a small repository layer, unit tests.
4. **1.4 Daemon.** `strata daemon start` (with `--foreground` for debugging), single-instance lock, `axum` on the Unix socket with file mode 0600, `/v1/status`, and the CLI client.
5. **1.5 Worktree manager.** `new`, `adopt`, `rm` and `status` through the `git` CLI (2.36 or later), git calls serialized. Create with `git worktree add --lock`. `rm`: check the tree is clean, then `git worktree unlock` and `git worktree remove`, adding `--force` only when the user passed it. Hand-deleted folder: `git worktree unlock <path>`, then `git worktree prune`. `strata init` also creates the base worktree at `<worktree_dir>/.base`.
6. **1.6 Service.** Per-user LaunchAgent or `systemd --user` unit. The Linux unit sets `Delegate=yes`; write the unit file directly if `service-manager` cannot express it.
7. **1.7 Crash recovery.** Kill the daemon with `SIGKILL` mid-operation, restart it, and check that state is consistent.

**Gate 1:** the git-level part of every Git row in the edge-case table passes end to end on macOS and Linux. The checker and process parts of those rows are tested at Gates 2 and 3.

### Phase 2: shared checker (2 to 3 weeks)

1. **2.1 tsbridge.** A small Node program embedded in the Rust binary with `include_str!` and written to `$STRATA_HOME/tsbridge/<version>/` on first run. It loads `typescript/async` from the repo's own `node_modules` with `createRequire`, so agents see the same TypeScript the repo uses. Its own dev dependency pins the exact 7.1 prerelease string, for tests only. Protocol below.
2. **2.2 Bridge client.** In `strata-checker`, spawn Node with the bridge, check the protocol version with `hello`, match responses by id with `tokio`, apply timeouts, and restart on crash per the edge-case table.
3. **2.3 Overlay tracker.** Walk the tsconfig include roots in base and in each worktree, adding one non-recursive `notify` watch per directory and skipping `node_modules` (except `checker.extra_paths`), `.git`, build output and nested worktrees; add watches as new directories appear. Hash files with xxh3. Layer = files that differ or exist only in the worktree; removed = files only in base. 200 ms debounce.
4. **2.4 Path translator.** Two-way mapping with realpath normalization, applied to every location field and to message text. Property tests (`proptest`) for round trips.
5. **2.5 Broker.** Checker groups keyed by repo, resolved TypeScript version and `dep_key`; snapshot lifecycle; idle disposal; the `huge_diff_ratio` fallback; LRU eviction above `group_memory_mb`. When base changes, create the new base and re-create agent snapshots on it before disposing the old base. Report sharing stats.
6. **2.6 MCP.** `strata mcp` with `rmcp` over stdio. Resolve the agent from MCP `roots/list`, then by walking up from the working directory, then `STRATA_AGENT` or `--agent`; with no match, return an explicit error. Tools: the checker tools that passed Gate 0, plus `strata_status`. Add `strata check` to the CLI.
7. **2.7 Parity suite.** On three fixtures (cal.com, excalidraw and `tests/fixtures/monorepo`), compare shared-checker answers with a dedicated snapshot per worktree. They must match.
8. **2.8 `strata bench`.** Port the Phase 0 harness onto the daemon, with the same fixtures and CSV format.

**tsbridge protocol (task 2.1).** JSON lines over stdio. Paths in the bridge are always base paths; translation happens in Rust. Positions are 0-based here; the MCP layer converts to 1-based.

```ts
type Req = { id: number; method: string; params: object };
type Res = { id: number; result?: object; error?: { code: "not_in_project" | "restarting" | "bad_request" | "internal"; message: string } };
type Loc = { file: string; line: number; character: number; endLine: number; endCharacter: number };
type Diag = Loc & { code: number; category: "error" | "warning" | "suggestion"; message: string };
type Sym = { name: string; kind: string; loc: Loc; children?: Sym[] };

// method(params): result
hello({ protocol: 1 }): { protocol: 1; typescript: string }
openBase({ group, root, tsconfigs: string[] }): { files: number }
createSnapshot({ group, agent, files: Record<basePath, worktreePath>, removed: basePath[] }): { layered: number }
updateSnapshot({ agent, files, removed }): { layered: number }   // sends the full layer, not a delta
dispose({ agent } | { group }): {}
diagnostics({ agent, file? }): { items: Diag[]; total: number }
hover({ agent, file, line, character }): { text: string } | null
definition({ agent, file, line, character }): { locations: Loc[] }
references({ agent, file, line, character }): { locations: Loc[] }
symbols({ agent, file }): { symbols: Sym[] }
stats({}): { snapshots: number; memoryBytes: number }
```

**Gate 2:** parity passes on all three fixtures, and `strata bench` reproduces the Phase 0 saving within 10 percentage points.

### Phase 3: processes, ports, proxy and budgets (1 to 2 weeks)

1. **3.0 Install tier.** Pull the seeder's plain-install tier forward: `strata new` runs the package manager's install in the worktree, so Phase 3 tests have real dependencies. Phase 4 adds cloning on top.
2. **3.1 Supervisor.** Spawn with `process-wrap` in a new process group, set `PORT`, `STRATA_AGENT` and `STRATA_URL`, write rotated logs to `$STRATA_HOME/logs/<agent>/<process>.log`. Stop means `SIGTERM`, wait 5 s, then `SIGKILL` the whole tree. On Linux with delegation, move the daemon into a leaf cgroup, write `+memory` to `cgroup.subtree_control`, and give each agent its own child cgroup.
3. **3.2 Memory meter.** Linux with cgroups: `memory.current`, with anonymous memory and file cache split from `memory.stat`; poll the running maximum where `memory.peak` is missing on older kernels. Without cgroups: sum PSS from `/proc/<pid>/smaps_rollup` over the tracked tree. macOS: sum `phys_footprint` over all descendants. Report the shared checker as its own line. Enforce budgets per `budget.action`.
4. **3.3 Port registry.** Assign from `port_range`, probe `127.0.0.1` and `::1`, then confirm within 30 s that the listener belongs to the process tree; if the server chose another port, update the route.
5. **3.4 Proxy.** `hyper` bound to loopback only on `proxy_port`; fail with a clear message if another program, such as Portless, holds that port. Route by Host header `<agent>.<app>.localhost`, pass WebSocket upgrades through, show a page listing live agents for a missing route, and return 508 on loops.
6. **3.5 CLI and MCP.** `strata dev`, `strata run` and `strata logs`, plus MCP `strata_start_dev_server`, `strata_run` (with timeout), `strata_stop` and `strata_logs`, limited to the caller's own processes.
7. **3.6 Presets.** Next.js (reads `PORT`; check whether its dev server blocks cross-origin requests from `*.localhost` and set allowed origins if so), Vite and Astro (`--port` flag), and a generic preset.

**Gate 3:** 5 Next.js dev servers run behind the proxy with working hot reload (checked by an end-to-end WebSocket test), and after `strata rm` no process or port is left behind.

### Phase 4: dependency seeding (1 week)

1. **4.1 Donors.** Key = `dep_key` as defined in the feasibility section. The base worktree's `node_modules` is the donor when its key matches.
2. **4.2 Clone engine.** macOS: `clonefile` each `node_modules` folder to a temporary name, then rename it into place; use recursive `copyfile` with `COPYFILE_CLONE` and `COPYFILE_STATE_FORBID_CROSS_MOUNT` only as a fallback. Benchmark both and record the results. Linux: a parallel tree walker (`rayon`) that reflinks files with `reflink-copy`, recreates directories and symlinks, and keeps file modes.
3. **4.3 Fallback tiers.** Clone, then install by default; `copy` and `hardlink` are opt-ins, with a warning that hardlinked files are shared when written.
4. **4.4 Fix-ups.** Rewrite absolute paths in known state files, reset validation timestamps, detect native addons and run the package manager's rebuild when needed.
5. **4.5 Warm pool.** Keep `warm_pool` pre-seeded worktrees on a detached HEAD. `strata new` unlocks one, moves it with `git worktree move`, re-locks it, re-runs fix-ups, checks out the branch, and re-seeds if `dep_key` changed. Disabled for repos with submodules.
6. **4.6 Shared-store detection.** Recognize pnpm's global virtual store and report it as "shared store, not isolated".
7. **4.7 Page cache.** Measure whether 5 dev servers on cloned `node_modules` use less memory than on separately installed copies, and record the answer in `NOTES.md`.

**Gate 4:** on the cal.com fixture (200k+ files), `strata new` with a matching donor takes under 15 s on APFS and on XFS with reflink, and the fixture's own tests pass inside the seeded worktree.

### Phase 5: hardening and packaging (1 to 2 weeks)

1. **5.1 Doctor.** `strata doctor` checks clone support, inotify usage, git 2.36 or later, Node version, TypeScript version and Yarn PnP, cgroup delegation and kernel version, `.localhost` resolution, socket path length and who holds the proxy port. `--report` prints a redacted summary to paste into issues.
2. **5.2 Cleanup and status.** `strata gc`, plus a readable `strata status` table and `--json`.
3. **5.3 Releases.** `dist` for macOS arm64 and Linux x86\_64 and arm64; a Homebrew formula in a tap; an npm launcher with per-platform `optionalDependencies` and embedded checksums. The bridge ships inside the binary (task 2.1); Node is a documented requirement.
4. **5.4 Docs.** README with quickstart, config reference, and recipes for Claude Code, Codex, Cursor and Conductor.
5. **5.5 Robustness.** Fuzz and property tests for the path translator and config parser.
6. **5.6 Soak.** 5 agents making scripted edits for 8 hours, run locally or on a self-hosted runner; memory growth under 10%.

**Gate 5:** a scripted run on clean macOS and Ubuntu VMs goes from install to an agent using the shared checker in under 5 minutes, using only README commands. The owner then repeats it by hand.

### Integration recipes

Agents connect through MCP. For Claude Code, add to `.mcp.json` in the repo:

```json
{ "mcpServers": { "strata": { "command": "strata", "args": ["mcp"] } } }
```

Worktrees created by other tools join with one line: `strata adopt .` in Conductor's setup script, or in a shell alias around `git worktree add`. Claude Code's `WorktreeCreate` hook replaces its own git logic, so it should call `strata new <name>` and return the path in the format the hook expects; its `WorktreeRemove` hook calls `strata rm <name>`. Check each tool's current hook format when writing the recipe.

### Definition of done for v0.1

- [ ] Every row of the edge-case table has an automated test or a documented manual check
- [ ] Benchmark published with a script anyone can rerun
- [ ] CI green on macOS and Linux; zero clippy warnings; `cargo deny` clean
- [ ] Install to first working agent under 5 minutes on both platforms

## Testing, benchmarks and success metrics

The benchmark is both the go/no-go test and the launch asset, so publish the script with the chart.

**Test layers:**

- **Unit:** config parsing, state store, layer computation, port registry, and the path translator with property tests.
- **Integration:** the bridge client against a real tsgo on fixtures; seeding in temp folders. On Linux CI, mount a loop-device XFS volume with `reflink=1` to test the clone tier and an ext4 volume to test fallbacks; macOS runners use APFS.
- **Parity:** shared-checker answers compared with a dedicated tsgo per worktree on every fixture.
- **End-to-end:** CLI and MCP flows (an MCP client inside the tests), and the proxy with a WebSocket hot-reload check.
- **Chaos:** `SIGKILL` the daemon, kill tsgo, fill a small loop volume, delete a worktree folder by hand.
- **Soak:** 5 agents making scripted edits for 8 hours, run locally or on a self-hosted runner.

**Benchmark protocol** (`bench/`, rerunnable by anyone):

- Pinned fixtures and commits; record chip, RAM, OS and filesystem with every run.
- Scenarios at 1, 3, 5 and 8 agents: separate tsgo per worktree versus Strata.
- Measure type-checker memory (steady and peak), time to first diagnostics for a new agent, diagnostics latency after an edit (p50 and p95), worktree creation time and disk added.
- Report dev-server memory per agent, but claim no saving there unless page-cache sharing is proven.
- Output a CSV plus one chart for the README.

**v0.1 targets** (targets, not measurements):

| Metric | Target |
| --- | --- |
| Type-checker memory, 5 agents, versus 5 separate tsgo | At most 50% (Gate 0 requires at most 60%) |
| Time to first diagnostics for a new agent, base warm | Under 3 s |
| Diagnostics after an edit, p95, medium fixture | Under 1 s |
| `strata new` with a matching donor, monorepo fixture | Under 15 s |
| Disk added per new worktree, clone tier | Under 100 MB beyond the agent's own changes |
| Leftover processes after `strata rm` | 0 |
| Memory growth over the 8-hour soak | Under 10% |
| Install to first working agent | Under 5 minutes |

After launch, track GitHub stars, weekly installs, orchestrators that integrate Strata, and issues filed from real repos.

## Risks, open questions and kill criteria

The biggest risk is that the saving is too small to matter; Phase 0 exists to find that out in a week instead of a quarter.

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| Memory saving too small because checker state, not parsed files, dominates | Medium | Fatal | Gate 0. If it fails, pivot to a per-agent memory governor: budgets, eviction and process ownership without the shared checker |
| An agent vendor ships its own sharing (for example, a fix for claude-code #87301) | Medium | High | Stay agent-agnostic and integrate with every orchestrator. A vendor fix for one agent likely shares only identical paths, not different worktrees |
| TypeScript 7.1 API changes before final, or beta bugs | High | Medium | Pin the exact version; keep the protocol inside the bridge; run the parity suite on every TypeScript release |
| Microsoft adds multi-worktree support to tsgo's own language server | Low to medium | High | Value shifts to process ownership, seeding and budgets. Contribute upstream and stay compatible |
| Path-translation bugs give agents wrong answers silently | Medium | High | Parity suite, property tests, and an automatic fallback to a dedicated snapshot when a mismatch is detected |
| Audience limited to TypeScript repos on 7.1 or later | High today, falling | Medium | TS 7 adoption should grow once 7.1 ships its API; keep the broker interface language-neutral for later |
| Clone tier unavailable on ext4 and NTFS (an estimated 55 to 65% of developers) | High | Low | Lead with the checker, not clones; install fallback works everywhere |
| Maintainer time | High | Medium | Strict v0.1 scope: macOS and Linux, TypeScript only, MCP before LSP |

**Kill or rethink if:**

- Gate 0 shows under 20% saving, or diagnostic mismatches not explained by path translation.
- A major vendor ships cross-worktree checker sharing before Phase 2 ends.
- TypeScript 7.1 final removes independent snapshots or filesystem layers.

**Open questions to settle during the build:**

- Do cloned `node_modules` trees share RAM page cache? Measure in Phase 4; if yes, it becomes a second memory claim.
- Do agents use MCP code-intelligence tools willingly, or do they prefer reading files and running `tsc`? Watch real sessions in Phase 2, and consider a Phase 6 LSP shim if agents ignore MCP.
- License: MIT or Apache-2.0 dual license, matching most Rust tooling.

## Launch and distribution

Launch on the benchmark chart, not the feature list. In 2026, small "agents in worktrees" Show HN posts mostly scored 1 to 13 points, while gigabyte charts from Vercel and swap-usage bug reports are what moved vendors.

1. **Before launch:** confirm the name, register the GitHub organization and domain, and record a 30-second clip of `strata status` with 5 agents and the memory saved.
2. **Benchmark post:** "5 agents, one type checker: X GB to Y GB", with the rerunnable script linked.
3. **Channels:** Show HN, r/ClaudeAI, r/cursor, X and dev.to. Add a short, useful comment on [claude-code #87301](https://github.com/anthropics/claude-code/issues/87301) and [coherence #170](https://github.com/PostHog/coherence/issues/170), where people asked for related fixes.
4. **Integrations as distribution:** open recipe PRs or docs for Conductor, Superset, Claude Squad, Emdash and Worktrunk, and make sure Strata's proxy never collides with Portless's port.
5. **Install paths:** `brew install`, `npx strata`, `cargo install strata`, and the `dist` shell installer.
6. **Feedback loop:** `strata doctor --report` produces a redacted environment summary to paste into issues.
7. **Later, not v0.1:** team features, hosted agent sandboxes or sponsorship. The common outcome for tools like this has been acquisition by a platform: Turborepo by Vercel, Bun by Anthropic, Astral by OpenAI, VoidZero by Cloudflare.

## Sources

The key pages behind this plan, all opened between Nov 1 and Nov 2, 2026. The full research report with 181 cited sources is attached in the chat.

**Demand and problem size**

- [claude-code #87301](https://github.com/anthropics/claude-code/issues/87301): 2 to 4 GB tsserver per session, not shared even for the same project root
- [PostHog/coherence #170](https://github.com/PostHog/coherence/issues/170): per-worktree memory; 4 agents swap a 16 GB machine
- [Next.js 16.3 Turbopack post](https://nextjs.org/blog/next-16-3-turbopack): dev-server memory before and after
- [JetBrains: AI coding agent adoption 2026](https://blog.jetbrains.com/research/2026/08/ai-coding-agent-adoption-2026/): 90% weekly agent use
- [Product Hunt poll on parallel Claude Code](https://www.producthunt.com/p/claude/how-many-claude-codes-do-you-run-in-parallel?page=2): 81% run two or more
- [Conductor Series A](https://www.conductor.build/blog/series-a): funding and growth of the orchestrator category
- [Trigger.dev: parallel agents with GitButler](https://trigger.dev/blog/parallel-agents-gitbutler): 9.82 GB across two worktrees
- [agent-flock post](https://dev.to/koenvg/when-worktrees-share-ram-gba): serializing heavy commands to avoid swap

**Orchestrators and vendor features**

- [Conductor scripts docs](https://www.conductor.build/docs/reference/scripts) · [Vibe Kanban](https://github.com/BloopAI/vibe-kanban) · [Superset](https://github.com/superset-sh/superset) · [Claude Squad](https://github.com/smtg-ai/claude-squad) · [Emdash](https://github.com/generalaction/emdash) · [Container Use](https://github.com/dagger/container-use) · [gtr](https://github.com/coderabbitai/git-worktree-runner)
- [Claude Code worktrees docs](https://code.claude.com/docs/en/worktrees.md) and [settings reference](https://code.claude.com/docs/en/settings-reference): per-worktree installs, `symlinkDirectories`, `WorktreeCreate` hooks
- [Cursor worktrees docs](https://cursor.com/docs/configuration/worktrees): advises against symlinking dependencies; 25-worktree cap
- [Codex git worktrees docs](https://learn.chatgpt.com/docs/environments/git-worktrees): separate dependencies per worktree

**Copy-on-write and package managers**

- [Worktrunk](https://github.com/max-sixty/worktrunk) and [`wt step` docs](https://worktrunk.dev/step/): reflinking ignored files into worktrees
- [parhel](https://github.com/myrk-kale/parhel) · [cow](https://github.com/joeinnes/cow)
- [pnpm git worktrees](https://pnpm.io/git-worktrees) · [pnpm global virtual store](https://pnpm.io/global-virtual-store) · [pnpm node\_modules settings](https://pnpm.io/settings/node-modules)
- [pnpm discussion #14041](https://github.com/orgs/pnpm/discussions/14041): clone timings and fix-ups on a 213k-file workspace
- [Bun install docs](https://bun.com/docs/pm/cli/install) · [Turborepo 2.8](https://turborepo.dev/blog/2-8)
- [clonefile(2)](https://keith.github.io/xcode-man-pages/clonefile.2.html) · [copyfile(3)](https://keith.github.io/xcode-man-pages/copyfile.3.html) · [ioctl\_ficlone(2)](https://man7.org/linux/man-pages/man2/ioctl_ficlone.2.html)

**TypeScript and language servers**

- [Announcing TypeScript 7.1 Beta](https://devblogs.microsoft.com/typescript/announcing-typescript-7-1-beta/): snapshots, filesystem layers, `removedPaths`
- [Announcing TypeScript 7.0](https://devblogs.microsoft.com/typescript/announcing-typescript-7-0/): speed and memory numbers
- [tsgo parsecache.go](https://github.com/microsoft/typescript-go/blob/89d5d5b2849a0db0957065889ca58536fa6d2e4a/internal/project/parsecache.go#L15) · [checkerpool.go](https://github.com/microsoft/typescript-go/blob/89d5d5b2849a0db0957065889ca58536fa6d2e4a/internal/project/checkerpool.go#L17) · [api/server.go](https://github.com/microsoft/typescript-go/blob/89d5d5b2849a0db0957065889ca58536fa6d2e4a/internal/api/server.go)
- [tsgo LSP server.go](https://github.com/microsoft/TypeScript/blob/6161772c1370bdd4a82f808e6823bd755eb1870d/tsc/internal/lsp/server.go#L1748) · [tsserver documentRegistry.ts](https://github.com/microsoft/TypeScript/blob/050880ce59e30b356b686bd3144efe24f875ebc8/src/services/documentRegistry.ts)
- [lspmux](https://codeberg.org/p2502/lspmux) · [typemux-cc](https://github.com/k-dash/typemux-cc) · [Glean incremental indexing](https://glean.software/blog/incremental/)
- [oxc parser benchmark](https://github.com/oxc-project/bench-javascript-parser-written-in-rust)

**Routing, processes and Rust crates**

- [Portless](https://github.com/vercel-labs/portless) · [portree](https://github.com/fairy-pitta/portree) · [WebKit bug 160504](https://bugs.webkit.org/show_bug.cgi?id=160504)
- [cgroup v2 docs](https://docs.kernel.org/admin-guide/cgroup-v2.html) · [systemd cgroup delegation](https://systemd.io/CGROUP_DELEGATION/) · [footprint(1)](https://keith.github.io/xcode-man-pages/footprint.1.html)
- [rmcp](https://crates.io/crates/rmcp) · [reflink-copy](https://crates.io/crates/reflink-copy) · [hyper](https://crates.io/crates/hyper) · [process-wrap](https://crates.io/crates/process-wrap) · [notify](https://crates.io/crates/notify) · [rusqlite](https://crates.io/crates/rusqlite) · [gitoxide status](https://github.com/GitoxideLabs/gitoxide/blob/main/crate-status.md)

**Precedents**

- [OpenAI acquires Astral](https://siliconangle.com/2026/03/19/openai-acquires-open-source-python-tooling-startup-astral/) · [VoidZero joins Cloudflare](https://voidzero.dev/posts/voidzero-cloudflare) · [Turborepo joins Vercel](https://turborepo.dev/blog/joining-vercel)
