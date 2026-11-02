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

## Phase 1 — task 1.7 crash recovery

- At startup, before serving, `ops::recover` settles what a crash interrupted:
  - `creating` → undone (worktree force-removed if git registered it, row deleted). The caller already got an error and never saw the path, so nothing of the user's is inside. A pre-existing branch is never deleted; a branch created by the interrupted `-b` stays (harmless; a retry reuses it).
  - `removing` → if git still has the worktree, it is re-locked and `ready` again (the caller saw a failure and can retry); otherwise the remove is finished.
  - `ready` whose worktree git no longer has → `orphaned`, unlocked and pruned.
  - Every action is logged and recorded as an `agent.recovered` event.
- Test hook (debug builds only): `STRATA_FAULT=<point>` stalls the daemon at `new:before_git`, `new:after_git`, `rm:before_git` or `rm:after_git`; the e2e test SIGKILLs it there, restarts and checks git and the store agree. Plus a worktree deleted while the daemon was down.
- Process re-adoption (pid and start time) belongs to Phase 3, when there are processes.
- Known gap: a graceful shutdown (SIGTERM, `daemon stop`) waits for in-flight requests; a request stuck forever would block it until the service manager's stop timeout sends SIGKILL, after which recovery runs. No such hang is known after the task 1.5 fix.

## Gate 1 result (2026-11-02): PASSED on macOS and Linux — stopped, waiting for the owner

Gate 1: "the git-level part of every Git row in the edge-case table passes end to end on macOS and Linux."

| Edge-case row (Git) | Test (`crates/strata/tests/`) | macOS | Linux |
| --- | --- | --- | --- |
| Branch already checked out in another worktree | `worktrees::refuses_a_branch_checked_out_in_another_worktree` | pass | pass |
| Worktree name or path already exists | `worktrees::refuses_an_existing_name_or_path` | pass | pass |
| Folder deleted by hand → orphaned, unlock, prune | `worktrees::a_hand_deleted_folder_is_orphaned_unlocked_and_pruned`, `recovery::worktree_deleted_while_the_daemon_was_down_is_orphaned` | pass | pass |
| `rm` with uncommitted changes refused unless `--force` | `worktrees::rm_refuses_uncommitted_changes_unless_forced` (+ unmerged branches kept) | pass | pass |
| Base branch moves → base worktree advances | `worktrees::base_worktree_follows_the_base_branch_not_the_main_checkout` | pass | pass |
| User switches branches in the main checkout → no effect | same test | pass | pass |
| Agent rebases or merges inside its worktree | `worktrees::rebase_merge_and_branch_switch_inside_a_worktree` | pass | pass |
| Several agents run git at once; locks survive a stray prune; unlock before own remove | `worktrees::concurrent_creates_serialize_and_locks_survive_a_stray_prune` | pass | pass |
| Worktrees nested inside the repo (`.claude/worktrees/`) | `worktrees::adopt_a_worktree_nested_inside_the_repo` | pass | pass |
| Repo uses submodules → warn | `worktrees::new_warns_about_uninitialized_submodules` | pass | pass |
| Worktree created by another tool → `adopt` | `worktrees::adopt_a_worktree_made_by_plain_git` | pass | pass |

Also passing: daemon rows (second daemon refused, socket 0600, socket path length), crash recovery at 4 kill points, config and detection. 55 tests, stable over repeated runs; `cargo fmt --check`, `clippy -D warnings` and `cargo deny check` clean.

**Linux (owner OK'd Docker):** official `rust:1.99.1-bookworm` image (2.08 GB on disk), Debian bookworm, kernel 7.0.14-linuxkit, aarch64, git 2.39.5, source mounted read-only. `cargo fmt --check` ok, `cargo clippy --locked --all-targets -- -D warnings` clean (first Linux build of the systemd code path), `cargo test --locked`: all 55 pass, first try. The 27 end-to-end tests also pass as an unprivileged user (uid 1000), since the container otherwise runs them as root.

Not covered: Linux x86_64 (CI's `ubuntu-latest` is x86_64; it would need an emulated image here), the real service install on either OS (`STRATA_SYSTEM_TESTS=1`, CI only), and GitHub CI itself (no remote yet).

## Phase 2 — design decisions before task 2.1

- One Node bridge, and so one tsgo process, per checker group. The protocol's `group` field is kept, so one bridge could serve several groups later.
- Layer detection: a stat walk with a cached xxh3 per file, run before each query, instead of a `notify` watcher. It cannot miss events and needs no inotify watches; its cost is measured on cal.com in task 2.3. A watcher can replace it if the walk is too slow.
- A minimal process-tree memory meter (`phys_footprint` on macOS, PSS on Linux) is pulled forward from Phase 3, because the broker's `group_memory_mb` limit and `strata bench` both need it.

## Phase 2 — task 2.1 tsbridge

- `packages/tsbridge/bridge.mjs`, one file, no dependencies. `typescript/async`, `typescript/fs` and `typescript/ast` are loaded from the repo's own `node_modules` with `createRequire` (then `import()` of the resolved file, since they are ESM). `package.json` pins `typescript` 7.1.0-beta as a dev dependency, for tests only.
- Protocol as in PLAN, plus: `hello` returns the bridge pid; `openUnshared({agent, root, tsconfigs})` opens a snapshot at the worktree's own paths (dependencies differ, huge diff, parity reference); `diagnostics` takes an optional `limit` and always returns the full `total`.
- API facts verified on 7.1.0-beta: positions are UTF-16 code units (checked after an emoji); source files have `getLineAndCharacterOfPosition` / `getPositionOfLineAndCharacter`; resolved nodes have `getStart()` (skips trivia) and `getSourceFile().fileName` in real case (only `NodeHandle.path` is lowercased); `SymbolFlags` comes from `typescript/async`.
- Built from checker calls, since the 7.1 language service has no quick-info or go-to-definition: hover = keyword from symbol flags + name + `typeToString` (declared type for type aliases) + documentation; definition = aliased symbol's declarations, located at their name; references = `getReferencedSymbolsForNode` on the deepest real node; symbols = top-level declarations, with members of classes, interfaces, enums and namespaces.
- Base replacement (`openBase` on an existing group) keeps the old base alive, reference-counted, until every agent derived from it has moved to the new base; then it is disposed. That is PLAN's "re-create agent snapshots before disposing the old base".
- `node --test` suite (4 tests): layered snapshot vs an unshared snapshot of the same worktree agree on diagnostics, hover, definition, references and symbols after path translation; error codes; base retirement. CI runs it.

## Phase 2 — task 2.2 bridge client

- `strata-checker::bridge::Bridge`: spawns `node bridge.mjs <repo>` (`$STRATA_NODE` overrides `node` from PATH), `kill_on_drop`, checks `hello` (protocol 1, TypeScript version), matches replies by id from a reader task, applies per-call timeouts (a timed-out reply that arrives later is dropped, later calls are fine), and on EOF marks itself dead and fails every pending call with "checker restarting, retry in a few seconds". Restarting is the broker's job (task 2.5).
- Bridge error codes map to Strata errors: `not_in_project` → not found, `bad_request` → refused, `restarting` → refused (retry), `internal` → a new `Error::Checker`.
- The script is embedded with `include_str!` and written once to `$STRATA_HOME/tsbridge/<version>-<xxh3 of the script>/bridge.mjs` (atomic rename), so a changed script gets a new folder.
- Verified: SIGKILL of the Node bridge also ends tsgo (its stdio closes); no orphans after the tests.
- `xxhash-rust` is BSL-1.0 (Boost, permissive); added to the `cargo deny` allow list.

## Phase 2 — task 2.3 overlay tracker

- `strata-overlay::Tree`: walks a checkout for files the checker reads (`ts tsx mts cts js jsx mjs cjs json`). It skips `.git`, `node_modules`, `.next`, `.turbo`, `.cache`, `coverage`, `.yarn`, `.svelte-kit`, `.vercel` and `.parcel-cache`, plus any nested checkout (a folder with its own `.git`, e.g. `.claude/worktrees/*`). It does not follow directory symlinks. `checker.extra_paths` (literal relative paths) are walked even inside `node_modules`. Each file has an xxh3; a rescan rehashes only files whose size or mtime changed. Ignored and untracked files count (PLAN: content hash, not `git status`).
- Deviation (decided before 2.1): no `notify` watcher. The broker rescans before queries; measured below. `dist/` and `build/` are walked, because a workspace package's `types` can point there.
- `layer(base, wt)`: changed = differs or only in the worktree; removed = only in base; a hash identifies the exact layer so unchanged layers skip the snapshot update.
- `dep_key`: xxh3 over every `package.json` outside `node_modules`, root lockfiles (`package-lock.json`, `npm-shrinkwrap.json`, `pnpm-lock.yaml`, `yarn.lock`, `bun.lock(b)`), `pnpm-workspace.yaml`, `.npmrc`, `.yarnrc(.yml)`, everything under `patches/`, and a runtime string (Node ABI and platform). Files under `extra_paths` are excluded (they are per-worktree on purpose). Since every `package.json` counts, an agent that edits one gets an unshared snapshot; PLAN's table says such changes are "usually" shareable, but its own `dep_key` definition includes them, and that is the safer reading.
- Measured (`cargo run --release -p strata-overlay --example scan`, M5 Pro, APFS):

| Fixture | Files walked | Cold scan | Warm rescan | Layer diff | Layer |
| --- | --- | --- | --- | --- | --- |
| cal.com | 6,426 | 725 ms | 50–60 ms | 0.6 ms | 6 changed, 1 removed |
| excalidraw | 818 | 93 ms | 3.7 ms | 0.1 ms | 6 changed, 1 removed |

  The layers match Phase 0's. A warm rescan of cal.com costs ~55 ms, so the broker rescans an agent's worktree on every query but the base at most every 2 s (the maintenance tick's cadence).

## Phase 2 — task 2.4 path translator

- `strata-checker::translate::Translator { base, wt }`: `to_base` for request paths (absolute, or relative to the worktree), `to_wt` / `to_agent` for result paths (`to_agent` = relative when inside the worktree, absolute otherwise, MCP's convention), `text_to_wt` for message and hover text, and `response()` to rewrite a whole bridge result (`file` fields, `message` and `text` fields). `identity(root)` serves unshared snapshots.
- Each root is matched in its given and its real (symlink-resolved) spelling, longest first, and only as a whole root: preceded by start or a non-name character and followed by `/`, end, or a non-name character. So `/x/.base2/...`, `/y/x/.base/...` and `a/x/.base/...` are left alone.
- proptest (also run with 2,000 cases): round trips for random relative paths; outside and lookalike paths unchanged; every whole root replaced in text. It found nothing in the translator; the case it flagged was a test mistake (a root glued to a word is correctly left alone). Writing the fix exposed one real bug: when two matches touched, the preceding-character check ignored the text already emitted.
- Lowercased `PathKey`s never reach Rust: the bridge reports `fileName` from resolved nodes (task 2.1).

## Phase 2 — task 2.5 broker

- `strata-checker::broker::Broker`: one group per repo (one bridge, so one tsgo, plus the base snapshot) and one snapshot per agent. Before each query the agent's worktree is rescanned and its snapshot updated if the layer hash, the base version or the bridge changed. The base is rescanned at most every `base_rescan` (2 s): a content change re-opens the base (agents move lazily; the bridge keeps the old base until the last one moves), a `dep_key` change restarts the group.
- Sharing per agent, reported in every answer and in status:
  - shared: layered on base, at base paths (translator base → worktree);
  - unshared, same process: `dep_key` differs ("dependencies differ") or more than `huge_diff_ratio` of base files changed ("large diff (N of M files changed)"); a snapshot at the worktree's own paths;
  - unshared, own process: the worktree's `node_modules/typescript` reports a different version than the base's. A worktree without its own `node_modules` uses the base's TypeScript.
- Locks: agent, then group. The group lock covers bridge start/restart, base rescan, layer computation and the snapshot update (so a layer is never applied to a different base than it was computed against). Queries run under the agent's lock only, so agents query concurrently. The agent map stores each worktree next to its lock so lookups never wait on a running query.
- A dead bridge (crash, OOM, kill) is restarted by the next query; queries that were in flight get "checker restarting, retry in a few seconds". Status counts restarts.
- `maintain()` disposes snapshots idle longer than `idle_dispose_secs`, and while the group's process tree (new `strata-proc::tree_memory`: macOS `phys_footprint`, Linux PSS, summed over descendants) is above `group_memory_mb`, the least recently used one per call (`ponytail`: Go returns freed memory minutes later, so measuring right after an eviction would over-evict).
- Answers: 1-based `line`/`column`/`endLine`/`endColumn` (UTF-16), `file` relative to the worktree when inside it, absolute otherwise.
- Checker off (Yarn PnP, TypeScript missing or older than 7.1, checked on the base) is a clear refusal.
- Fixed on the way: tokio's `ChildStdin::shutdown()` does not close a child pipe (only dropping it does), so bridge shutdown waited 5 s and killed; the bridge also no longer waits on `api.close()` when stdin closes. A `limit: null` sliced diagnostics to nothing.
- Known Phase 2 stopgap for the daemon: the `.base` worktree has no `node_modules` until Phase 4 seeding (and agent worktrees none until Phase 3.0 installs). Wired up in task 2.6.
- 7 broker tests on real toy projects: edits, additions and deletions followed; base changes reach agents (the agent's own copy of a file wins); the three unshared modes; restart after SIGKILL; idle disposal; errors.

## Phase 2 — task 2.6 MCP and `strata check`

- Daemon: `POST /v1/agents/{name}/check {repo, op, file?, line?, column?, limit?}` (op = diagnostics, hover, definition, references, symbols) answers from the broker; the agent must be `ready`. `strata rm` and orphaning dispose the agent's snapshot; the maintenance loop runs the broker's idle/LRU pass every 30 s; `/v1/status` gains a `checker` section (per group: TypeScript, program files, restarts, memory; per agent: sharing, snapshot held, changed/removed files, idle time). The broker is shut down with the daemon.
- **Stopgap until Phase 4 seeding:** the base worktree gets symlinks to the main checkout's `seed.paths` folders (`node_modules`, `apps/*/node_modules`, ...) that exist, re-checked every tick so a later install is picked up. tsgo resolves through them to the main checkout's real paths, which the translator leaves absolute. Agents share the base's resolution, so an agent worktree needs no `node_modules` of its own to be checked; an unshared agent (dependencies differ) has none until Phase 3.0 installs it.
- `strata check <name> [file] [--json]`: `file:line:column category TScode: message` lines and a count; exits 1 when there are errors, like tsc.
- `strata mcp [--agent]` (crate `strata-mcp`, rmcp 3.8.0 over stdio): `strata_diagnostics` (whole project capped at 200 listed + total), `strata_hover`, `strata_definition`, `strata_references`, `strata_symbols`, `strata_status`. Answers are short text, 1-based, paths relative to the worktree; an unshared answer says why. The caller is the agent whose worktree contains an MCP root, else the working directory (deepest worktree wins, for nested ones), else `--agent` / `STRATA_AGENT`; with no match every tool returns an error and nothing is answered from base.
- **Reality vs PLAN:** MCP roots are deprecated by SEP-2577 in protocol 2026-07-28 (rmcp marks them deprecated). They still work with clients that support them, so they stay first, behind `#[allow(deprecated)]`; the working directory and `--agent` cover clients without them.
- `huge_diff_ratio` is measured against the program's file count from `openBase` (PLAN: "share of project files", libs and dependency types included), not the walked checkout; on a 4-file toy repo one edit was 25% of the checkout.
- The daemon's socket client moved to `strata-core::client` so the CLI and the MCP server share it.
- Tests: `strata check` output and exit codes, status, snapshot disposal on `rm`; an rmcp client spawning `strata mcp` lists the 6 tools and calls each; agent found from MCP roots (cwd outside), from `STRATA_AGENT`, and refused with no match. 86 tests in total.

## Phase 2 — task 2.7 parity suite

- `crates/strata-checker/tests/parity.rs`. Shared = one broker on the base, each worktree layered at base paths. Dedicated = per worktree its own broker (own bridge, own tsgo) whose base *is* the worktree, i.e. a snapshot at its own paths. Compared per worktree: whole-project diagnostics; for each sampled file (the agent's changed `.ts`/`.tsx` plus evenly spread untouched ones) its diagnostics and symbols; for 3 identifiers per file (after the first 30%) hover and definition, and references for the first. Results must be equal JSON (minus the `sharing` field); errors must be equal strings.
- New fixture `tests/fixtures/monorepo`: two packages and an app, three tsconfigs extending one base config, cross-package imports via `paths`; class, interface, enum and namespace for symbols. The test applies three diffs: a signature change breaking callers in other packages plus an added file; a deleted module; a tsconfig change (`noUnusedLocals`) plus an unused local (PLAN's "tsconfig changed in a worktree" row).
- The Phase 0 fixtures shipped TypeScript 5.9.3 in `node_modules` (Phase 0's harness used its own 7.1 copy). `bench/setup.sh` now links the pinned 7.1.0-beta into every checkout's `node_modules/typescript` (PLAN 0.1: "moves them to the TypeScript 7.1 Beta"); the repo's own copy stays at `node_modules/.typescript-repo`.
- The broker now translates paths in error messages too (a not-in-project error named the base path).
- Results (M5 Pro, release build):

| Fixture | Worktrees | Answers compared | Mismatches | Errors (same both sides) | Empty | Diagnostics | Shared / dedicated time |
| --- | --- | --- | --- | --- | --- | --- | --- |
| monorepo | 3 | 111 | **0** | 0 | 34 | 13 | 0.9 s / 1.2 s |
| excalidraw | 5 | 500 | **0** | 45 | 140 | 41 | 11.1 s / 11.9 s |
| cal.com | 5 | 500 | **0** | 0 | 153 | 3,680 | 71.5 s / 70.7 s |

  The 45 excalidraw errors are "not in any configured project" for sampled files under `dev-docs/` (outside the root tsconfig), identical on both sides with worktree paths. Empty = no symbol at a position (keywords, punctuation) or no declarations.
- `monorepo` runs in `cargo test` (and CI); `excalidraw` and `calcom` are `#[ignore]` (they need `bench/setup.sh` fixtures and ~3 min): `cargo test --release -p strata-checker --test parity -- --ignored --nocapture`.
