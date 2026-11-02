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

## Phase 2 — task 2.8 `strata bench`

- `strata bench <fixture> [--tsconfig] [--agents 5] [--workload whole|files] [--modes baseline,shared] [--idle 60,900] [--out csv]`: Phase 0's protocol on the broker code the daemon runs, in-process (no HTTP hop). baseline = one broker per worktree (own bridge and tsgo, a snapshot at its own paths, i.e. a separate tsgo per worktree); shared = one broker on the base with every worktree layered. Same steps as Phase 0: load (an outline of each agent's first edited file creates its snapshot), whole-project or per-edited-file diagnostics for all agents at once, then 20 hovers per agent (2 identifiers in each of 10 files no agent edited), warm reading 10 s later, optional idle readings. Memory = this process's tree footprint every 250 ms (`strata-proc`), this process counted on both sides as Phase 0 counted its Node client; the baseline leaves out its bridges' Node processes (a separate tsgo per agent has none), the shared side keeps its one bridge. CSV columns match `bench/results/phase0.csv`.
- Hovers in files outside the project (excalidraw's `dev-docs/`, 2 of the 20 targets) are skipped and counted, as Phase 0's harness caught them.
- `bench/strata-bench.sh` runs the Gate 2 set into `bench/results/strata.csv`.

## Gate 2 result (2026-11-02): PASSED — stopped, waiting for the owner

Gate 2: "parity passes on all three fixtures, and `strata bench` reproduces the Phase 0 saving within 10 percentage points."

- Parity (task 2.7): 0 mismatches on monorepo (111 answers), excalidraw (500) and cal.com (500).
- `strata bench` at 5 agents vs Phase 0 (S = 1 − shared ÷ baseline):

| Fixture | Workload | Phase 0 S peak / warm | `strata bench` S peak / warm | Baseline peak (Phase 0 → bench) | Shared peak (Phase 0 → bench) |
| --- | --- | --- | --- | --- | --- |
| excalidraw | whole | 21% / 22% | 20% / 21% | 3.61 → 3.64 GiB | 2.83 → 2.91 GiB |
| excalidraw | files | 65% / 65% | 60% / 61% | 1.41 → 1.41 GiB | 0.50 → 0.56 GiB |
| cal.com | whole | 30% / 30% | 29% / 29% | 10.24 → 10.19 GiB | 7.18 → 7.28 GiB |
| cal.com | files | 61% / 61% | 62% / 62% | 5.22 → 5.25 GiB | 2.05 → 2.01 GiB |

  Largest difference 5 points (excalidraw per-file), mostly the shared side's Node bridge (~50 MB on ~0.5 GiB). One run per configuration; Phase 0 saw run-to-run GC noise of a few hundred MB on whole-project peaks.
- Timing seen in passing (not a gate item, not investigated): cal.com whole-project diagnostics for 5 agents took 5.8 s shared vs 8.2 s baseline in `strata bench`, where Phase 0's Node harness measured 18 s vs 10.6 s.
- Not covered: Linux (Phase 2 code ran on macOS only; the `strata-proc` Linux path and the bridge on Linux have not run), CI (no remote), and the daemon's `.base` uses `node_modules` symlinks until Phase 4.

## Phase 3 — task 3.0 install tier

- `strata new` installs dependencies in the new worktree with the worktree's own package manager: `npm ci`, `pnpm install --frozen-lockfile`, `yarn install --immutable` (Berry) or `--frozen-lockfile` (Yarn 1), `bun install --frozen-lockfile`. Without a lockfile it tells the manager not to write one (`--no-package-lock`, `--lockfile=false`, `--no-save`): a new lockfile would dirty the worktree and change its `dep_key`. Lifecycle scripts run (postinstall steps such as `prisma generate` are usually needed for dev servers). Output goes to `$STRATA_HOME/logs/<agent>/install.log`.
- The daemon's git lock is released before installing, so other agents' git operations do not wait minutes. The agent stays `creating` meanwhile: `rm` refuses it, the maintenance pass skips it, and crash recovery undoes it.
- A failed install leaves a usable worktree: `seed_tier = install-failed` and a warning naming the log. `seed_tier` (`install`, `install-failed`, `none` for repos without `package.json`) is now in the agent record and status; adopted worktrees have none.
- Deviation, decided for the whole phase: process groups use `std`'s `process_group(0)` plus `libc` signals and descendant tracking instead of the `process-wrap` crate; v0.1 has no Windows job objects to justify it.

## Phase 3 — task 3.1 supervisor

- `strata-proc`: `spawn` runs `sh -c <cmd>` in the worktree as the leader of a new process group (`process_group(0)`), stdin closed, output appended to `$STRATA_HOME/logs/<agent>/<dev|run-<ms>>.log` (rotated to `.log.1` above 10 MB at start; `ponytail`: no rotation while running). `terminate` sends SIGTERM to the group and to every recorded process, waits up to 5 s, then SIGKILL. Recorded = (pid, start time), so a reused pid is never signalled. Start time: macOS `proc_pidinfo(PROC_PIDTBSDINFO)`, Linux `/proc/<pid>/stat` field 22. Also `cwd`, `descendants`, and `tcp_listeners` (the `listeners` crate, MIT).
- Linux cgroup v2 with delegation (`strata-proc::cgroup`): the daemon moves itself into a `daemon` leaf, enables `+memory` for children and gives each agent `agent-<name>`; a process joins in `pre_exec` with a single `write(2)` on a descriptor opened before fork (no allocation after fork); `cgroup.kill` clears an agent's leftovers. Without delegation (and on macOS) the recorded-pid walk is the only mechanism. **Not yet run on Linux.**
- Daemon: each process has a wait thread; on exit, whatever is left of its tree is terminated and the row becomes `exited` (or `stopped` when Strata stopped it). The maintenance tick records new descendants every 2 s, which is how a child that leaves the group (own session) is still found. API: `POST /v1/agents/{name}/procs {repo, kind: dev|run, cmd?, timeout_s?}` (dev returns at once, one per agent; run waits and returns exit code and the last 200 lines), `DELETE /v1/procs/{id} {agent?}`, `GET /v1/procs/{id}/logs?lines=&agent=` (an `agent` limits access to that agent's processes). `strata run <name> [--timeout] -- <cmd>` (exits with the command's code, 124 on timeout), `strata logs <name> [id] [--lines]`; status lists processes per agent.
- `strata rm`: validations first (`ops::removable`), then the agent's processes are stopped, then the worktree is removed. An orphaned agent's processes are stopped too.
- Daemon restart: a process is re-adopted only if pid and start time both match (then polled for exit); otherwise `dead`. With `dev.restart = true`, dead dev servers of ready agents are started again with the same command and port. A graceful `daemon stop` leaves processes running, so an upgrade does not take dev servers down.
- 6 e2e tests: run output, exit code, worktree cwd, `STRATA_AGENT`, timeout; a child escaped with `setsid` is gone after `rm`; one dev server per agent; another agent cannot stop it; re-adoption after SIGKILL of the daemon; `dead` plus `dev.restart`; orphan cleanup. No stray processes after the suite.

## Phase 3 — task 3.2 memory meter and budgets

- Every maintenance tick (2 s) measures each agent with running processes: its cgroup's `memory.current` where the daemon has delegation, else the sum of macOS `phys_footprint` / Linux PSS over every live process recorded in its trees (deduplicated). The shared checker is reported on its own line (`checker` in status, per group).
- Budget = `budget.per_agent_mb` (the `agents.budget_mb` column exists but no command sets it yet). Levels: ok, warning (≥ 90%), over (> 100%). Each change upward writes one event (`budget.warning`, `budget.exceeded`). With `budget.action = "stop"`, going over stops the agent's dev and run processes (`budget.stopped` event); the agent and its worktree stay.
- Warnings reach: `strata status` (`memory N MB (over budget)`), `/v1/status` (`memory_bytes`, `budget`, plus the last 20 `events`), and every MCP answer (a warning line appended while the agent is near or over).
- Found while doing this: two scripted edits in task 3.1 had silently not applied (the `strata status` process lines); scripted replacements now assert that they matched.
- Tests: a 60 MB process against a 10 MB budget is metered and reported over budget (warn keeps it running), and with `stop` its process is stopped while the agent stays ready.

## Phase 3 — task 3.3 port registry

- Each agent owns one port from `dev.port_range`, reserved by `strata new` (after the install) and `strata adopt`: the lowest port no agent holds that also binds on 127.0.0.1 and ::1 (a port an outside program uses is skipped; no IPv6 on the machine counts as free there). Assignment is serialized in the daemon and the `ports` primary key backs it; `rm` frees the port.
- `strata dev` / `kind: dev` sets `PORT`, `STRATA_AGENT` and `STRATA_URL` (`http://<agent>.<app>.localhost:<proxy_port>`) and answers with the port, the proxy URL and the direct `http://localhost:<port>` (PLAN: for OAuth callbacks that reject `.localhost`).
- Within 30 s the daemon finds which process of the tree listens (`listeners` crate, filtered to the recorded tree) and routes `<agent>.<app>` there; a wildcard listener is reached on loopback of its family. A server that ignored `PORT` is routed to where it really listens, its record updated, and a `route.updated` event written; nothing within 30 s writes `listener.missing`. Routes are in memory: a restarted daemon confirms running dev servers again. A route goes when its server ends.
- Status lists `unmanaged` listeners: sockets of processes whose working directory is inside a worktree but which Strata did not start, with a hint to use `strata_start_dev_server`.
- 4 e2e tests (each with its own port range, since test daemons run in parallel): distinct ports skipping an outsider's, reuse after `rm`; port, URL and confirmed route for a Node server; a server on another port gets the route and the event; an outside server in a worktree is listed.

## Phase 3 — task 3.4 proxy

- `strata-proxy` (hyper 1.12, HTTP/1.1): binds 127.0.0.1 and ::1 (no IPv6 is fine). Routes by Host header (`<agent>.<app>.localhost[:port]`, case-insensitive) to the agent's confirmed listener over a fresh connection per request; WebSocket upgrades are spliced with `hyper::upgrade` and `copy_bidirectional`, so hot reload works. The original Host is kept (plus `x-forwarded-host`/`-proto`), so the app sees the URL the browser used. Unknown host: 404 page listing live agents with links. A request that already carries `x-strata-proxy` (a route pointing back at the proxy): 508. Dead dev server: 502.
- A held port fails with the holder's name and pid (`listeners::get_process_by_port`) and a hint to change `dev.proxy_port`.
- Deviation: the proxy starts **lazily**, when the first dev server needs it (and again at daemon start if dev servers are running), not at daemon start. A daemon with no dev servers holds no port, and parallel test daemons do not fight over 7355. The held-port failure surfaces at `strata dev` / `strata_start_dev_server` instead of `daemon start`. `doctor` (Phase 5) will check it up front.
- Tests: 5 crate tests (Host routing with the Host kept, agent listing, 508 loop, 502 dead backend, WebSocket echo through the proxy, held port names its holder) and 2 e2e (two agents routed through the daemon's proxy; a held proxy port fails `dev` with the holder named and starts nothing).
- Since dev servers now start the proxy, every e2e test repo gets its own port range and proxy port (parallel test daemons would otherwise all want 7355). The first version of this commit missed that and had 5 failing tests; it was amended before anything was pushed.

## Phase 3 — task 3.5 CLI and MCP process tools

- CLI: `strata dev <name> [--cmd]` prints the proxy URL, the direct `http://localhost:<port>` URL and the process (or says it is already running); `strata run` and `strata logs` came with task 3.1.
- MCP (`strata-mcp`, now 10 tools): `strata_start_dev_server {command?}` returns the URL, the direct URL and the process id at once; `strata_run {command, timeout_s?}` returns the exit code (or the timeout) and the last 200 lines; `strata_stop {process_id}`; `strata_logs {process_id, lines?}`. Stop and logs pass the caller's agent name, and the daemon answers "no process" for another agent's process, so an agent can only act on its own. `strata_status` now shows the port, memory and budget level, and running processes. Every answer carries the budget warning when there is one.
- Tests: `strata dev` output and proxying, already-running; an MCP client starts a dev server reachable through the proxy, runs a command (exit code, output), hits a timeout, sees the process in status; another agent's MCP server is refused `strata_stop` and `strata_logs` on it; the owner stops it.

## Phase 3 — task 3.6 presets

- When the dev command is the project's own `dev` script (configured or detected) Strata adds the framework's port flags; an explicit `--cmd` / `command` runs as given with `PORT` set. Next.js reads `PORT` (no flag). Vite gets `--port N --strictPort` (so it fails instead of drifting to another port), Astro `--port N`; npm and bun need `--` before script arguments. Anything else is the generic preset: `PORT` only, and the listener check routes to wherever it really listens. Framework = `next` / `vite` / `astro` in the project's dependencies, in that order.
- Next.js cross-origin check (PLAN asked): Next 16.4 blocks cross-origin requests to `/_next/*` and the HMR socket unless the Origin matches `allowedDevOrigins`, but `**.localhost` and `localhost` are always allowed (`blockCrossSiteDEV` in `next/dist/server/lib/router-utils/block-cross-site-dev.js`). Proxy URLs are `*.localhost`, so nothing needs setting and the proxy does not rewrite Origin.
- Next 16 HMR is `/_next/hmr` (no longer `/_next/webpack-hmr`): JSON messages, `turbopack-connected` on connect, `serverComponentChanges` after a server component edit.
- Next 16 writes `AGENTS.md` into the project on `next dev` (`agentRules`, on by default). In an agent's worktree that is an untracked file, so `strata rm` would refuse without `--force`. The test fixture sets `agentRules: false`; for user repos this is left to them (Strata does not edit their config).
- Fixture `tests/fixtures/nextjs`: Next 16.4.4, React 19.3.1, app router, one page, lockfile committed, `node_modules` ignored.
- The detected-script check compares the command text: a `dev.command` that differs from the script (e.g. `next dev --turbo`) runs as configured, with `PORT` only.

## Gate 3 result (2026-11-02): PASSED — stopped, waiting for the owner

Gate 3: "5 Next.js dev servers run behind the proxy with working hot reload (checked by an end-to-end WebSocket test), and after `strata rm` no process or port is left behind."

Test: `crates/strata/tests/nextjs.rs` (`#[ignore]`d; run with `cargo test -p strata --test nextjs -- --ignored --nocapture`). A temp repo with the `tests/fixtures/nextjs` files; `strata init`; `strata new a1..a5` (each runs `npm ci`); `strata dev` on all five (Next preset, `PORT`). Per agent, through the proxy (`Host: aN.app.localhost:47150`): GET the page (version 1), open `/_next/hmr` with the page's Origin, wait for `turbopack-connected`, edit the worktree's `app/page.js`, wait for `serverComponentChanges`, GET again and see the edit. Then every agent still serves its own edit (routes do not cross). Before `rm` it records each server's process group plus whatever listens on its port; after `strata rm --force` (the edit makes the worktree dirty) each of those pids must be gone and the port must bind on 127.0.0.1 and ::1. Finally no routes and no agents.

Two runs on macOS (M-series, warm npm cache), both passed:

| Measure | Run 1 | Run 2 |
| --- | --- | --- |
| Whole test | 19.7 s | 18.6 s |
| `strata new` (incl. `npm ci` of Next 16.4.4) | 2.0–2.2 s each | similar (not printed) |
| Five `strata dev` → five routes confirmed | 1.1 s | — |
| First page through the proxy (includes first compile) | 0.52–1.03 s | 0.50–1.42 s |
| Edit → `serverComponentChanges` over the proxied socket | 0.04–0.06 s | 0.06 s |
| Memory per dev server (meter, sampled once) | 235–544 MB | 421–559 MB |
| Processes per server (group + listener) | 3 | 3 |
| `strata rm --force` per agent | 0.64–0.70 s | — |
| Processes or dev ports left after `rm` | 0 | 0 |

- No stray process after either run (`pgrep` for `next dev` / `next-server` / `strata daemon` showed only an unrelated Next 16.3.5 project of the owner's, outside any test dir).
- Not covered: Linux (cgroup path still unrun), a cold npm cache (install time is then network-bound), Vite and Astro end to end (presets are unit-tested only).
- The proxy port itself stays held by the daemon after the last agent goes (lazy start, no lazy stop); it is the daemon's port, not an agent's, and is freed by `daemon stop`.
- Five `npm ci` copies of Next are ~5 × 350 MB of disk in worktrees; Phase 4 seeding is what addresses that.

## Phase 4 — task 4.1 donors

- A donor is a Strata-owned checkout whose seed folders were installed (or cloned) for a known `dep_key`: one row per (repo, key) in `donors`, one key per checkout. `dep_key` is the checker's own (`strata_overlay::Tree::dep_key`), and the Node runtime string now comes from one function (`strata_core::detect::node_runtime`) for both, so the two can never disagree.
- The base worktree is the donor for its key. It gets its own dependencies at `strata init` (and at daemon start, and in the background whenever the base moves): cloned from a registered donor with its key, else from the main checkout when the main checkout has the same key and has seed folders, else installed (`$STRATA_HOME/logs/.base/install.log`). Phase 2's symlinks into the main checkout are removed and replaced. Deviation from PLAN's wording: the main checkout is used as a source for the base when its key matches. It is not a donor (Strata does not own it), and it is trusted on its key alone: a main checkout whose `node_modules` is out of date with its own lockfile seeds an out-of-date base.
- `strata new`: key of the new worktree; the donor for that key, if it still has it (re-scanned every time; a stale or deleted donor is dropped, since a stale donor measured slower than a fresh install), is cloned; otherwise install, and the new worktree becomes the donor for its key. Removing an agent removes its donor row. The tier is printed (`dependencies: clone from <donor>` on stderr; stdout stays the path), stored with the key on the agent, and shown in `strata status`.
- Seed folders: the donor's git-ignored folders (`git ls-files --others --ignored --directory`, 0.14 s on cal.com) matching `seed.paths`. `*` matches one folder, `**` any depth. Deviation: the default is `["**/node_modules"]`, not PLAN's three explicit patterns, because cal.com has 12 `node_modules` folders and some sit two levels down (`packages/platform/atoms`, `example-apps/credential-sync`, `packages/platform/examples/base`). A symlinked source folder is never cloned (it would share, not isolate; 4.6 reports it).
- Cloning (needed here so a donor means something; 4.2 adds Linux and the error handling): macOS `clonefile(2)` of each folder to `.strata-seed-<name>` next to it, then `rename` into place.
- Measured on cal.com `node_modules` (272,033 entries, 3.3 GB, APFS, M-series): `clonefile` of the folder 2.14 s; recursive `copyfile(COPYFILE_CLONE | COPYFILE_RECURSIVE)` 57.5 s (it clones file by file, like Phase 0's `cp -c -R`, which took 51 s for all ignored paths); free space unchanged after both. Deleting either clone took 15.4–15.5 s (`rm -rf`), which matters for `strata rm` later. `COPYFILE_CLONE_FORCE` rejects folders (EINVAL), as its man page says.
- Gate 3 test again with seeding: `strata new` 0.12–0.13 s per agent (was 2.0–2.2 s with `npm ci`), still passing.
- Tests (`tests/seed.rs`, replaces `install.rs`): base installs and agents clone from it; the main checkout seeds the base when its key matches; a new key installs and becomes the donor, a second agent clones from it, a donor whose `package.json` changed is never used, removing the donor removes it; a failed install still gives a usable worktree; no `package.json` means tier `none`. Clone assertions are macOS-only until 4.2.

## Phase 4 — task 4.2 clone engine

- macOS: one `clonefile(2)` per seed folder (4.1). Deviation: no recursive `copyfile` fallback. Measured on cal.com, it clones file by file (57.5 s against `clonefile`'s 2–8 s), and on a volume that cannot clone `COPYFILE_CLONE` silently degrades to a full copy, which is the opt-in `copy` tier's job, not the clone tier's. When `clonefile` fails, the configured fallback runs (4.3).
- Linux: `walk` recreates the tree in parallel (rayon 1.12.0, MIT/Apache-2.0): folders with their modes (set after their entries, so read-only folders work), symlinks as symlinks (never followed), files reflinked with `ioctl(FICLONE)` with mode and mtime kept. Deviation: `libc::FICLONE` instead of the `reflink-copy` crate, since it is one call and `libc` is already a dependency. On ext4 the first file fails (EOPNOTSUPP/EXDEV) and the fallback runs. The same walker does the `copy` and `hardlink` tiers on both platforms. **The Linux path compiles and passes clippy for `aarch64-unknown-linux-gnu` but has not run on Linux yet** (Gate 4 runs it on XFS).
- A full disk (`ENOSPC`) while cloning: the partial folder is removed, no install is attempted (it would fill the disk too), tier `clone-failed`, and the warning gives free space and the donor's apparent size.
- `examples/clone.rs` times one way on a tree. cal.com `node_modules` (272,033 entries, 3.3 GB) on APFS, M-series, release build:

| Way | Time |
| --- | --- |
| `clonefile` (one call) | 2.14, 4.34, 8.41, 7.11, 7.77 s (five runs, in that order) |
| walker, copy (parallel) | 20.06 s |
| walker, hardlink (parallel) | 55.58 s |
| recursive `copyfile` with `COPYFILE_CLONE` (C, 4.1) | 57.53 s |
| `rm -rf` of one clone | 13.6–15.5 s |

  `clonefile` times drift upward across back-to-back runs; Spotlight indexing the fresh trees is a suspect, not confirmed. APFS hardlinks are slower than a parallel copy here. Free space did not move for clones and dropped by about the tree's size for the copy.

## Phase 4 — task 4.3 fallback tiers

- When cloning from the donor fails, `seed.fallback` decides: `install` (default; the warning names the reason and suggests `seed.fallback = "copy"`), `copy` or `hardlink` (the walker from the same donor). `hardlink` always warns that the files are shared, so a write in one worktree changes the donor's and every other hardlinked worktree's. If the fallback fails too, it installs. Without a donor for the key there is nothing to copy, so it installs whatever the setting. The tier is printed by `strata new`, stored on the agent and shown in `strata status`: `clone`, `copy`, `hardlink`, `install`, `install-failed`, `clone-failed` (disk full) or `none`.
- `strata init` warns when `repo.worktree_dir` is on another volume than the repo (different `st_dev`): the base then cannot clone the main checkout and installs; agents still clone from the base, which shares their volume. (`doctor` is Phase 5.)
- Measured cost of the opt-ins on cal.com (APFS, 4.2): copy 20 s, hardlink 56 s. APFS hardlinks are slower than a parallel copy, so on macOS `hardlink` buys nothing; it is for filesystems where links are cheap and copies are not.
- Test hook: `STRATA_FAULT=no-clone` (debug builds only) makes every clone fail like ext4. Test: with it, `copy` gives the worktree its own files (different inode), `hardlink` the donor's (same inode) plus the warning, `install` installs with the warning. The fixture ships `fallback = "copy"` so its base has files to give on Linux too.

## Phase 4 — task 4.4 fix-ups

- After any seeding from a donor (clone, copy or hardlink, including main checkout → base), `strata_seed::fix_up` runs over each seed folder:
  - **Paths:** donor paths become the worktree's in pnpm's `.pnpm-workspace-state-v1.json` and `.modules.yaml`, and in every regular file of the folder's `.bin` (old pnpm shims carried absolute `NODE_PATH`s; symlinked bins are skipped).
  - **Timestamp:** `lastValidatedTimestamp` is set to now.
  - **Addons:** packages with a node-gyp `build/config.gypi` whose `build/` files (`config.gypi`, `Makefile`, `*.mk`) name the donor are rebuilt with the package manager's own command (`npm rebuild`, `pnpm rebuild`, Yarn Berry `yarn rebuild`; npm's for Yarn 1 and bun, which have none). The log goes to `logs/<agent>/rebuild.log` and the tier reads `clone from X, rebuilt a, b`.
  - **Hardlink tier:** nothing is rebuilt, with a warning, since a rebuild would write into the donor's files.
  - **No donor writes:** a rewritten file is a new file renamed over the old one, so a hardlinked donor is never changed.
- What reality showed:
  - **cal.com** (Yarn 4, `nodeLinker: node-modules`): no file under any of its `node_modules` contains the checkout's absolute path (`grep -rlF`, 8.7 s over 3.3 GB). Its 6 native addons (msgpackr-extract, node-cpu-profiler, sqlite3, @parcel/watcher, deasync, sharp) all ship prebuilt binaries, with no `build/config.gypi`. So cal.com needs no fix-ups and no rebuilds.
  - **pnpm 12.11.2:** only `.pnpm-workspace-state-v1.json` holds absolute paths (the `projects` keys); `.bin` shims now resolve their own location and `.modules.yaml` has none. A clone *without* the fix-up still ran `pnpm run` with `verify-deps-before-run=error` and `pnpm install --frozen-lockfile --offline` without reinstalling. The pnpm 10 behaviour PLAN cites (pnpm #14041) does not reproduce on 12, so the fix-up is kept because it is cheap and leaves the state true, not because 12 needs it.
- Not done: `.bin` folders nested inside pnpm's `.pnpm/*/node_modules/*/node_modules` are not scanned, which would mean walking the whole store; build caches inside `node_modules` (`.cache`, `.vite`) are cloned as they are, since most are content-keyed and deleting them costs time.
- Found on the way: a pnpm workspace with no lockfile and no `packageManager` field was detected as npm, which then fails on `workspace:` dependencies. `pnpm-workspace.yaml` now means pnpm.
- Tests: a unit test (state and shim rewritten, timestamp reset, hardlinked donor untouched, two stale addons found including one in `.pnpm`); e2e, a pnpm workspace whose clone's state file names the agent and whose `pnpm run` works (skipped without pnpm); a fake node-gyp addon built in the main checkout is rebuilt in the base and again in the agent, each time in its own folder.

## Phase 4 — task 4.5 warm pool

- **Entries:** `seed.warm_pool` worktrees under `<worktree_dir>/.pool/<millis>`, detached at the base's commit and seeded like an agent (clone from the donor, or the fallback). An entry never becomes a donor, since it moves. Its state is its git lock reason, not a table: `strata pool: seeding` while it is made, `strata pool: ready <dep_key> <tier> <donor>` after. At daemon start a `seeding` entry is a crash's leftover and is removed, and a record whose folder is gone is pruned.
- **Filling:** happens in the background after every `strata new` and at daemon start for repos that have agents. Deviation: it does not fill at `strata init`, so a repo nobody creates agents in costs nothing, and a test can turn the pool off right after `init` with nothing to race. Skipped for repos with a `.gitmodules` (PLAN) and for repos without a `package.json` (nothing to seed). `strata status` shows the count per repo (`warm_pool`).
- **`strata new`:** takes a ready entry only when `git diff --name-only <entry HEAD> <target>` touches no `dep_key` input (`strata_overlay::is_dep_input`, the same list the key is computed from), so its dependencies will match. It unlocks the entry, `git worktree move`s it into place, re-locks it, then `git switch -c <branch> <from>` (or `git switch <branch>` for an existing one). The fix-ups run again from the entry's old path, and the tier reads `clone from <donor> (warm pool)`. Deviation: PLAN says to re-seed when `dep_key` changed. Here an entry whose dependency files differ is not taken at all, because re-seeding means deleting the entry's `node_modules` first (13.6–15.5 s for cal.com) and would be slower than seeding from the donor. The key is still checked after the move, and a mismatch, which the diff check should make impossible, re-seeds.
- Found while testing it: the daemon-start loop `for repo in d.store().repos()…` held the store guard through its body, which took the store lock again, so the daemon deadlocked on start. This is the same trap as in task 1.x; bound first now.
- Tests (`tests/pool.rs`): no entry before a first `new`; after it one is seeded, and the next `new` takes it (moved, not copied; locked; on its branch; clean; dependencies there), the pool refills, and `rm` needs no `--force`; an entry is left alone for a `--from` whose `package.json` differs; a repo with `.gitmodules` gets no pool; a `seeding` leftover is removed at daemon start. Other tests turn the pool off after `init` (`common::no_pool`). Gate 3 with the pool on: `strata new` 0.06 s when it took an entry, 0.14–0.21 s when it cloned, and still passing.

## Phase 4 — task 4.6 shared-store detection

- `strata_seed::shared_store`: a checkout whose `node_modules/.modules.yaml` names a `virtualStoreDir` that resolves outside the checkout uses pnpm's global virtual store (`enableGlobalVirtualStore`). pnpm 12.11.2 writes the file as JSON (`"virtualStoreDir": "../../…/Library/pnpm/store/v11/links"`) and older versions as YAML; both forms are read.
- A shared-store donor is never cloned: the worktree installs, which for this layout only creates links, and the tier is `shared-store`, printed as **shared store, not isolated**. The checkout still donates its key, so the base is not reinstalled at every daemon start. The warm pool is off for such repos.
- What reality showed: pnpm 12's global-virtual-store links are *relative* (`is-number -> ../../../../../../../../Users/…/store/v11/links/@/is-number/7.0.0/<hash>/node_modules/is-number`). So beyond PLAN's "copies the links, not the files", a clone at another folder depth (main checkout → `.base`, or `.pool/<n>` → `<name>`) would break every link. One more reason never to clone it.
- Tests: unit (JSON global store → shared; YAML `.pnpm` → not; no file → not); e2e with pnpm, offline (`offline=true` in `.npmrc`, so it runs only where `is-number@7.0.0` is already in the pnpm store, and is skipped otherwise): base and agent are both `shared store, not isolated`, and the agent's package resolves.

## Phase 4 — task 4.7 page cache

- Answer on macOS (APFS): **cloned files do not share page cache.** Direct test with `mincore(2)`, which reports a mapped file's resident pages without reading it: write a 64 MiB file, `clonefile` it, read only the original. The original was 4096/4096 pages resident and the untouched clone 0/4096; reading the clone then brought in its own 4096. The unified buffer cache is per file (vnode), and a clone is another file that happens to share disk blocks. So the memory claim stays where PLAN put it: seeding saves disk and setup time, not RAM. Linux keeps its page cache per inode too, so reflinks are expected to behave the same; not measured there.
- PLAN's comparison (`tests/nextjs.rs`, `memory_of_five_servers_cloned_vs_installed`, ignored): five Next 16 dev servers, each having served its page, 15 s settle, two rounds alternating:

| Round | Seeding | Metered footprint, 5 servers | System page cache change |
| --- | --- | --- | --- |
| 1 | clone | 2102 MB | −1028 MB |
| 1 | own `npm ci` each | 2096 MB | −1436 MB |
| 2 | clone | 2078 MB | −385 MB |
| 2 | own `npm ci` each | 2094 MB | −1034 MB |

  Footprint (macOS `phys_footprint`, which excludes clean file pages) is the same within 1%, as expected: the servers' own memory does not depend on how their files got there. The page-cache column is not usable. The machine (24 GB) had about 130 MB free and the system evicted file cache while the servers allocated, so every delta is negative. The `mincore` test above is the evidence for the page-cache answer, not this column.
