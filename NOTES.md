# NOTES

Log of decisions, deviations from PLAN.md, and every measured number.

## Environment (2026-11-02)

- Apple M5 Pro, 24 GB RAM, macOS (Darwin 27.0.0), APFS
- git 2.52.0, Node 22.22.2, npm 10.9.7, pnpm 12.11.2, yarn 1.22.22, bun 1.3.11
- No Rust toolchain installed yet (needed from Phase 1). Phase 0 is Node + tsgo only.
- npm `typescript` dist-tags: latest 7.0.2, beta 7.1.0-beta, next 7.1.0-dev.20261101.1

## Phase 0 — API probe (before task 0.1)

Installed `typescript@7.1.0-beta` (gitHead 3e9236361fa97a9e6c685f4833ac457ae2d8c50a) and probed a toy project.

- Entry points: `typescript/async` (`API`, `Snapshot`, `Project`, `Program`, `Checker`, `LanguageService`), `typescript/fs` (`createFileSystemLayer`, `createFileSystem`). Binary comes from `@typescript/typescript-<platform>` optionalDependency.
- `new API({ cwd })` spawns one tsgo per instance. `api.createSnapshot({ openProjects: [tsconfig] })` loads a configured project.
- `base.update({ fileSystem: createFileSystemLayer(files, { removedPaths }), ensurePrograms: true })` gives an agent snapshot: edits, added files (picked up by `include` globs) and removed files all behave correctly, and the base snapshot is unchanged afterwards. `api.createSnapshot({ fileSystem: layer, openProjects })` (independent) gives the same answers.
- **Gotcha:** without `ensurePrograms: true` the derived snapshot's program is the stale base program (no diagnostics change). Deviation from PLAN: every update must pass `ensurePrograms`.
- Diagnostics come back as `{ fileName, pos, end, startPosition?, code, category, text, messageChain?, relatedInformation? }`.
- **Per-snapshot language features in the API (no LSP-style hover/definition calls):**
  - diagnostics: `program.getSyntacticDiagnostics / getSemanticDiagnostics / getGlobalDiagnostics(file?)`
  - hover: build from `checker.getSymbolAtPosition` + `getTypeOfSymbol` + `typeToString` + `symbol.getDocumentationComment`
  - definition: `symbol.declarations` (node handles with `path`, `index`)
  - references: `languageService.getReferencedSymbolsForNode(node, pos)`, or `checker.getReferencesToSymbolInFile`
  - completions: `languageService.getCompletionsAtPosition`
  - symbols: walk the remote AST from `program.getSourceFile`
  - rename: not exposed

## Phase 0 — Task 0.1 fixtures

`bench/setup.sh` (idempotent). Fixtures live in `bench/fixtures/` (gitignored), shallow-fetched at pinned SHAs:

| Fixture | Repo @ SHA | Project checked | Package manager | Files in program | Lines |
| --- | --- | --- | --- | --- | --- |
| excalidraw | excalidraw/excalidraw @ 8bcdca4526867433f99fdfcb55a063668db0c9dc | `tsconfig.json` | yarn 1.22 | 1,519 | 461,394 |
| calcom | calcom/cal.com @ f972feefa1376cf47631177231c671da9a9dd09f | `apps/web/tsconfig.json` | yarn 4.12 (node-modules linker) | 10,445 | 1,456,736 |

TypeScript used by the harness: exactly `7.1.0-beta` (pinned in `bench/package.json`). Both repos ship on TS 5.9.3; tsgo ignores the repo's own `typescript` package for checking.

TS 7.1 migration fixes (`bench/patches/*.sh`, committed in each base as `strata: ts7.1 migration fixes`, so every worktree inherits them):

- excalidraw: removed `"baseUrl": "."` (TS5102). Paths were already `./`-relative. Result: **0 errors**.
- calcom `apps/web/tsconfig.json`: removed `baseUrl`, prefixed bare `paths` targets with `./` (TS5090), added `"*": ["./*"]` (the documented `baseUrl` replacement; bare imports such as `app/_utils` depend on it), and overrode `target: es2017` and `moduleResolution: bundler` because the inherited `@calcom/tsconfig` sets `es5` and `node` (TS5108, both removed in TS 7). Only `apps/web` was changed, not the shared configs.
- calcom post-install (`bench/patches/calcom.post.sh`): `prisma generate` (scripts are disabled during install) and `@calcom/trpc build:server` (apps/web imports the emitted router declarations; built with the repo's own TS 5.9.3, 11 s).
- calcom still reports **730 errors** under tsgo 7.1: ~300 TS2339 where tRPC's router type degrades to its "collides with a built-in method" error union (a TS 7 / `bundler` resolution difference, not investigated further), ~270 TS7006 implicit-any that cascade from it, plus jest-dom matcher typings and `@hookform/resolvers/zod` (TS7016). Per the plan, parity needs equal results, not zero errors. Consequence to keep in mind: tRPC inference partly collapses, so checker work on calcom is somewhat **understated** relative to a fully typed tree.
- Full `tsc --noEmit` memory (tsgo `--extendedDiagnostics`, one process): excalidraw ~1.0 GB, calcom ~2.5 GB.

Worktrees: `git worktree add -b agent-N`, then every git-ignored path in base (node_modules, Prisma output, tRPC declarations, `.yarn/cache`) is copied with `cp -c -R` (APFS clone). `bench/diff.mjs` applies a deterministic per-agent diff, committed as `strata: agent diff N`: 5 tracked .ts/.tsx files under the project root edited (appended helper functions; the first also gets a deliberate TS2322), 1 file added next to the first edit (imports it), 1 file deleted (importers then fail with TS2307). Each worktree's diff is different.
- Seeding timing (useful for Phase 4, not a Phase 0 gate): `cp -c -R` of every ignored path took ~10 s per excalidraw worktree and **~51 s per calcom worktree** (3.4 GB node_modules plus `.yarn/cache`), on an M5 Pro / APFS. `cp -c` clones file by file; Phase 4's single `clonefile` per directory should be much faster (pnpm #14041 reports 13.4 s for 213k files).
- cal.com's husky pre-commit hook (lint-staged, `biome lint` only, no `--write`) ran on the first diff commits; diffs were unaffected. `setup.sh` now commits with `--no-verify`.

## Phase 0 — harness decisions (tasks 0.2–0.6)

- One mode per Node process; memory = `phys_footprint` summed over the harness process tree every 250 ms (`bench/footprint`, cross-checked: 734 MB vs `/usr/bin/footprint -p` 734 MB on one tsgo). The Node client is counted on both sides. `GOGC`/`GOMEMLIMIT` left at Go defaults on both sides and recorded per run.
- Whole-project diagnostics = config-parsing + global + syntactic + semantic diagnostics for the program. Hover = `getSymbolAtPosition` → `typeToString(getTypeOfSymbol)`. 20 hover targets are identifiers in 10 files no agent touched, so offsets are valid everywhere.
- Parity keys: path relative to the agent's own root, `pos-end`, code, full message chain with the root prefix replaced. **Path translation finding:** results embed absolute paths outside location fields (e.g. a module symbol's *name* is its quoted absolute path), confirming the plan's "replace root prefix inside message strings" rule.
- **Path translation finding 2:** `NodeHandle.path` (definitions, references) is a canonical `PathKey`, lowercased on case-insensitive APFS. The translator must match the canonical form of the base root (`program.getCanonicalFileName(root)`), not the display path, and map back to real casing (the definition/reference probe needed this to match).
- Feature probe smoke (excalidraw, agent 1, layered vs dedicated snapshot, 5 positions): diagnostics, hover, definition, references (via `languageService.getReferencedSymbolsForNode` on a real AST node; astnav's synthesized tokens are rejected with "getNodeId requires a RemoteNode") and document symbols all work and match 5/5. Completions fail with "completion list needs auto imports" on 4/5 positions even with `openFiles` + `prepareAutoImports`; completions are post-v0.1 per PLAN, so not pursued. Rename is not exposed by the 7.1 API.

## Phase 0 — measurement findings

- **Seeding bug found and fixed (affected calcom only).** `git ls-files --others --ignored --directory` lists both `packages/prisma/zod/` and files under it, so `cp -c -R` copied some directories twice into themselves (`zod/modelSchema/modelSchema/`, `generated/prisma/prisma/`, ...). The duplicates are outside the `apps/web` program (parity unaffected), but the spike's layer diff walks the whole repo, so every calcom agent layer carried **482 files / 22.4 MB instead of 6 files / 9–39 KB**. Removed the duplicates, `setup.sh` now skips paths that already exist, and the calcom shared run was repeated. The biased run is kept as `results/raw/calcom-shared-5-junklayer*` for transparency. Lesson for the overlay tracker (task 2.3): restrict the layer to tsconfig include roots plus `extra_paths`, as PLAN already says; a whole-repo walk picks up unrelated generated trees.
- Memory curves: both modes hold their peak until Go's periodic forced GC and scavenger run (drops at ~4 and ~6–7 min), then sit flat. "idle 60 s" equals "warm" in every run; "idle 15 min" is the settled state.
- Per-snapshot cost (excalidraw scaling, one process, whole-project diagnostics on every snapshot): ~516 MB per added snapshot, versus ~738 MB per dedicated tsgo process. Parsed and bound files are ~30% of a dedicated process's peak; per-program checker state is ~70% and is not shared. That caps the peak saving near 30% at large N on this fixture.
- Scaling: 32 layered snapshots in one tsgo process with no errors (excalidraw). Creation is cheap (~35 ms per snapshot up to 16, 97 ms at 32 under memory pressure; calcom 170–200 ms). Whole-project diagnostics time grows linearly with snapshot count (~1.25 s each, CPU-bound); 32 snapshots reached 16.6 GB on a 24 GB machine and slowed to 56 s.
- Timing (5 agents, concurrent whole-project diagnostics): excalidraw baseline 2.2 s vs shared 6.8 s; calcom baseline 10.6 s vs shared ~19 s. One process serves the five requests with less parallelism than five processes.
- LSP launch comparison: 5 × `tsc --lsp` with 10 files open each peaks at 1.06 GB (excalidraw) and 5.64 GB (calcom). It checks only open files, so it is a lower-bound workload, not comparable to whole-project diagnostics.
- calcom scaling: no errors up to 16 snapshots (stopped by the 14 GB guard at 17.3 GB); ~1.29 GB per added snapshot vs ~2.10 GB per dedicated process (38% marginal saving).
- Feature probe on calcom: diagnostics, hover, definition, references, symbols 5/5 match; completions work 5/5 and match 4/5 (excalidraw: 1/5 work). Rename not exposed.

## Gate 0 result (2026-11-02): NOT PASSED — stopped, waiting for the owner

As first reported (shared side inflated by the layer harness, see below): calcom 26% / 26% / 64% (peak / warm / idle 15 min), excalidraw 18% / 18% / 57%. Corrected:

| Fixture | S peak | S warm | S idle 15 min | Parity |
| --- | --- | --- | --- | --- |
| calcom | 30% | 30% | 63% | 0 mismatches |
| excalidraw | 21% | 22% | 68% | 0 mismatches |

Both fixtures fall in the 20–40% band (owner decides); excalidraw is within run-to-run GC noise of the 20% line. Correctness and per-snapshot diagnostics pass. Cause: checker state (~60–70% of a dedicated process's peak) is per snapshot; only parsed and bound files are shared. Full write-up and options in `bench/REPORT.md`. Phase 1 not started.

## Task 0.8 (owner follow-up after Gate 0): per-file agent workload

Not in PLAN; the owner chose option 3 ("measure agent workload"). Committed as `phase-0/task-8`.

- Worktrees 6–8 added with `AGENTS=8 bench/setup.sh` (calcom: 51 s seeding each). `fixtureInfo` no longer cycles 5 worktrees.
- Workload (`run.mjs --workload files`, driver `bench/agents.sh`): each agent requests syntactic + semantic diagnostics for the 6 .ts/.tsx files its diff added or modified (`git diff --diff-filter=AM strata-base`), one file at a time, agents concurrent; then the same 20 hovers. Hover targets exclude all 8 diffs so they are identical at every agent count. LSP mode opens the edited files and requests `textDocument/diagnostic` on each (like-for-like with the API modes). No idle readings.
- **Harness bug found:** `layerFor` read every file in the repo in Node to diff contents. The calcom 1-agent trace showed 899 MiB after the base load and 1,492 MiB after layer computation with tsgo idle (~590 MiB of Node heap), counted against the shared side. That also biased the Gate 0 shared runs. `layerFor` now reads `git diff --name-status strata-base` plus untracked files: only changed files are read, as the overlay tracker will do. Same layers (6 files + 1 deletion per agent). Reran the per-file matrix and both whole-project shared runs; old ones kept as `results/raw/*-shared-5-walklayer*`. Baseline and LSP runs compute no layers and are unaffected. The task 0.6 scaling runs carried the overhead as a constant Node offset; the per-snapshot marginal costs stand.
- Results (S peak vs separate API processes; vs separate `tsc --lsp` within 1–3 points):

| Agents | calcom | excalidraw |
| --- | --- | --- |
| 1 | −28% | −16% |
| 3 | 42% | 49% |
| 5 | 61% | 65% |
| 8 | 74% | 75% |

- Marginal cost per extra agent (5 → 8): shared ~55 MiB calcom / ~24 MiB excalidraw; separate processes ~1.05 GiB / ~290 MiB. Fixed cost of sharing at 1 agent: +0.31 GiB calcom, +0.05 GiB excalidraw.
- Parity: 0 mismatches at every agent count on both fixtures.
- Per-file timing: 0.02–0.3 s per agent at 1 agent; calcom at 8 agents 1.0 s shared vs 0.56 s separate.
- Reading: the shared parsed/bound program dominates when agents check only what they edit; checker state dominates only when every agent checks the whole project at once. Per-file meets PLAN's success metric (≤50% of separate processes at 5 agents: calcom 39%, excalidraw 35%); the Gate 0 protocol does not.

## Owner decision after Gate 0 (2026-11-02): go to Phase 1

The owner chose "Go to Phase 1" after the task 0.8 follow-up, i.e. REPORT option 2: proceed on the per-file workload result (≈60% saved at 5 agents, 75% at 8) with the narrower claim, accepting that Gate 0's whole-project protocol measured 21–30%. Governor features that only one process allows (limit concurrent whole-project checks, dispose idle checkers) stay in scope for Phase 2.

## Phase 1 — before task 1.1

- **Name check (PLAN: "before Phase 1").** "strata" is taken on crates.io (dormant search-engine crate, 2019), npm (HTTP server, 0.20.1), as a GitHub organization, and as strata.sh. No Homebrew formula or cask. Kept as the working name; every crate is `publish = false` until the owner picks the release name (needed by Phase 5 packaging).
- Rust toolchain installed with Homebrew `rustup` (keg-only; commands use `/opt/homebrew/opt/rustup/bin`), toolchain pinned in `rust-toolchain.toml`.

## Phase 1 — task 1.1 skeleton

- Workspace with only the crates Phase 1 needs: `strata` (binary), `strata-core`, `strata-git`. Deviation from PLAN's layout: the other crates (`strata-seed`, `-overlay`, `-checker`, `-proc`, `-proxy`, `-mcp`) are created in the phase that fills them, not as empty shells. End-to-end tests live in `crates/strata/tests/` (Cargo runs them against the built binary) instead of a top-level `tests/e2e/`.
- Toolchain pinned to 1.99.1, edition 2024. Crate versions pinned in `[workspace.dependencies]`, exact versions in the committed `Cargo.lock`.
- `cargo deny check`: advisories, bans, licenses, sources all ok. Our own crates' license is left open (PLAN open question) with `private.ignore`.
- CI (`.github/workflows/ci.yml`, macOS + Ubuntu) is written but has never run: the repo has no remote yet.

## Phase 1 — task 1.2 config and init

- `strata.toml` parses into PLAN's full schema with serde defaults; unknown keys are errors, and validation checks the port range, that the proxy port is outside it, `huge_diff_ratio` in (0, 1], a non-empty `tsconfig` list and `dev.app` as a DNS label (it becomes `<agent>.<app>.localhost`).
- `strata init` resolves the main checkout even when run inside a linked worktree. It writes `strata.toml` once (never overwrites) with `repo.base` = `main`, else `master`, else the current branch, plus `worktree_dir = "../.strata/<folder>"`, the dev command and `dev.app` from the folder name.
- Detection: lockfile order pnpm, bun, yarn, npm, then `packageManager`, then npm if a `package.json` exists. Yarn Berry is Plug'n'Play unless `.yarnrc.yml` sets `nodeLinker: node-modules` (Yarn 1 never is). TypeScript comes from `node_modules/typescript/package.json`; 7.1 prereleases count as 7.1.
- Deviation: "checker off" is not written into the config. It is derived from detection each time (Yarn PnP, TypeScript missing or older than 7.1), so a repo that upgrades TypeScript turns the checker on without editing `strata.toml`. `init` prints the reason.
- Registering the repo with the daemon, the base worktree and the service install are added to `init` in tasks 1.4–1.6.

## Phase 1 — task 1.3 state store

- `strata-core::store`: rusqlite 0.41 (bundled SQLite) in WAL mode, `foreign_keys` on, 5 s busy timeout. PLAN's data model is migration 1; migrations are append-only and tracked with `PRAGMA user_version`.
- Repository layer covers only what Phase 1 uses: repos (upsert, by root, list), agents (insert, by name, by path, list, set state, delete) and the event log. Agent states: `creating` and `removing` mark an operation in flight so crash recovery (task 1.7) can finish or undo it; `ready`; `orphaned` (folder deleted by hand).
- A duplicate agent name maps to a `Refused` error with a message, not a raw constraint failure. Paths must be UTF-8 (refused otherwise).
- `events.agent_id` has no foreign key (PLAN's schema), so events outlive their agent.

## Phase 1 — task 1.4 daemon

- `strata daemon start` spawns `strata daemon start --foreground` in its own process group (stdout/stderr to `logs/daemon.out`, `tracing` to `logs/daemon.log`) and waits up to 10 s for `/v1/status` to answer. `stop` posts `/v1/shutdown` (added to PLAN's API list) and waits for the socket to go; SIGTERM and Ctrl-C also shut down gracefully.
- Single instance: `std::fs::File::try_lock` on `$STRATA_HOME/daemon.lock` (holds the owner's pid). A second daemon exits with a message naming that pid. Holding the lock is what makes it safe to delete a stale socket left by a SIGKILLed daemon.
- `$STRATA_HOME` is created 0700 and the socket chmodded 0600 after bind. The socket path is checked against 103 bytes before binding.
- CLI client: about 30 lines of blocking HTTP/1.1 over `std::os::unix::net::UnixStream` (`Connection: close`, read to EOF) instead of a hyper client; the daemon answers with fixed-length JSON. Error mapping: 404 → not found, other non-2xx → the daemon's message.
- No leftover test daemons: every e2e `Env` stops its daemon on drop.

## Phase 1 — task 1.5 worktree manager

- `strata new|adopt|rm|status` go through the daemon. Each operation holds one daemon-wide git lock for its whole run (`ponytail`: one lock for all repos), so Strata's own git calls never overlap; `LC_ALL=C` on every git call because a few error messages are matched.
- `strata init` now also starts the daemon if needed and registers the repo, which creates the base worktree at `<worktree_dir>/.base`, locked and detached at the base branch (the branch itself is usually checked out in the user's checkout, so it cannot be checked out twice).
- `new`: name must be a DNS label; refuses an existing agent name or folder with a free `-2` style suggestion; branch defaults to the name; an existing branch is checked out as is (refused if checked out in another worktree, with a suggestion, or if `--from` is also given); a new branch starts at `--from` or the base branch. The row is inserted as `creating` before `git worktree add --lock` and deleted if git fails. Uninitialized submodules come back as warnings (PLAN: warn); Strata does not init them.
- `adopt`: must be the top folder of a linked worktree of a registered repo (not the main checkout, not `.base`); the agent name is the folder name as a DNS label, suffixed if taken; Strata locks it.
- `rm`: refuses uncommitted changes (modified or untracked, `git status --porcelain`) unless `--force`; marks `removing`, unlocks, `git worktree remove [--force]`, re-locks if that fails. Then deletes the branch with `git branch -d` only, so an unmerged branch is kept with a note (never the base branch). A folder already deleted by hand: just forget the record.
- Maintenance pass every 2 s (`ponytail`: polling git; the Phase 2 watcher can trigger it): moves `.base` when the base branch moves (re-creates it if deleted), marks agents whose folder vanished as `orphaned` and runs `git worktree unlock` + `git worktree prune`, and records branch switches inside worktrees.
- API deviations: requests carry the repo root (`repo` in the JSON body) since agent names are unique per repo, and `DELETE /v1/agents/{name}` takes a JSON body (`repo`, `force`, `keep_branch`) instead of a query string, so paths need no URL encoding.
- Bug found by the e2e tests: `for x in d.store().list()? { d.store()... }` deadlocks, because a `for` keeps its iterator expression's temporaries (the mutex guard) alive for the whole loop. Fixed by binding the list first. Side finding: a graceful-shutdown SIGTERM never completes while a request is stuck; task 1.7 notes it.
- 14 end-to-end tests cover every Git row of the edge-case table (git-level part); stable over 3 repeated runs.

## Phase 1 — task 1.6 service

- Files written directly, no `service-manager` crate. macOS: `~/Library/LaunchAgents/dev.strata.daemon.plist`, loaded with `launchctl bootstrap gui/<uid>` (`bootout` first on reinstall). Linux: `$XDG_CONFIG_HOME/systemd/user/strata.service` with `Delegate=yes`, then `daemon-reload`, `enable`, `restart`. Both capture `STRATA_HOME` and the installing shell's `PATH` (launchd's default PATH lacks Homebrew, where Node will live in Phase 2).
- Restart only on failure (`KeepAlive.SuccessfulExit = false`, `Restart=on-failure`), so `strata daemon stop` (exit 0) stays stopped and a crash is restarted.
- `strata init` installs the service by default and lets it start the daemon. It first stops a hand-started daemon: otherwise the service's daemon would lose the lock, exit 1 and be relaunched every 10 s. `--no-service` starts the daemon directly (all e2e tests use it). Added `strata daemon install|uninstall` (not in PLAN's CLI table) so the service can be removed.
- Platform code: one `platform` module per `cfg(target_os)` with the same functions (`file`, `render`, `load`, `unload`) instead of a trait with one implementation per build.
- **Not exercised on this machine:** installing a LaunchAgent is a persistent change to the owner's login, so the real install/uninstall test runs only with `STRATA_SYSTEM_TESTS=1` (CI). The plist passes `plutil -lint`; the systemd unit is checked by string tests only until it runs on Linux.
