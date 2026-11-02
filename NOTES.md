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

| Fixture | S peak | S warm | S idle 15 min | Parity |
| --- | --- | --- | --- | --- |
| calcom | 26% | 26% | 64% | 0 mismatches |
| excalidraw | 18% | 18% | 57% | 0 mismatches |

calcom falls in the 20–40% band (owner decides); excalidraw is under 20%, which PLAN lists as a kill-or-rethink criterion. Correctness and per-snapshot diagnostics pass. Cause: checker state (~60–70% of a dedicated process's peak) is per snapshot; only parsed and bound files are shared. Full write-up and options in `bench/REPORT.md`. Phase 1 not started.
