# Phase 0 report: one tsgo process vs many

2026-11-02 · Apple M5 Pro, 24 GB, macOS (Darwin 27.0.0), APFS · TypeScript `7.1.0-beta` · Go GC defaults on both sides

## Summary

- **Gate 0 is not passed.** Under PLAN's protocol (all 5 agents run whole-project diagnostics at the same moment), one shared tsgo saves **30% (cal.com) and 21–22% (excalidraw)** at peak and warm, against a 40% target. Both fall in the 20–40% band, where the owner decides. These numbers are corrected for a harness bug found in the follow-up (it had shown 26% and 18%).
- **With a realistic agent workload, sharing saves 40–75%.** Each agent checks only the files it edited (the owner-requested follow-up). Sharing saves **42–49% at 3 agents, 61–65% at 5 and 74–75% at 8**, against separate API processes and against separate `tsc --lsp` servers alike. With 1 agent, sharing costs 16–28% more.
- **Correctness held:** 0 mismatches in every run.
- **The difference is checker state.** A whole-project check builds a full type checker for every snapshot, and tsgo does not share checkers. A per-file check builds a small one, so the shared parsed and bound files dominate.

## Gate 0: not passed (corrected)

S = 1 − shared ÷ baseline, at 5 worktrees. Memory is `phys_footprint` summed over the whole process tree, with the Node client counted on both sides.

| Fixture | Baseline peak | Shared peak | **S peak** | Baseline warm | Shared warm | **S warm** | Baseline idle 15 min | Shared idle 15 min | S idle 15 min |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| calcom (`apps/web`, 10,445 files) | 10.24 GiB | 7.18 GiB | **30%** | 10.24 GiB | 7.17 GiB | **30%** | 5.57 GiB | 2.08 GiB | 63% |
| excalidraw (1,519 files) | 3.61 GiB | 2.83 GiB | **21%** | 3.60 GiB | 2.82 GiB | **22%** | 1.61 GiB | 0.52 GiB | 68% |

| Gate 0 condition | Result |
| --- | --- |
| S ≥ 40% on peak and warm | **No.** Both fixtures fall in the 20–40% band, where the owner decides. Excalidraw's 21% is within run-to-run noise of the 20% line |
| No diagnostic mismatch after path translation | **Yes.** 0 mismatches on both fixtures, for all 5 agents (diagnostics plus 20 hovers each), and every agent's diagnostics differ from the others' |
| Per-snapshot diagnostics available | **Yes** |

**Correction.** The harness computed each agent's layer by reading every file in the repo from Node, and the footprint counted that memory against the shared side. On cal.com that was about 590 MiB: in one trace, memory went from 899 MiB after the base load to 1,492 MiB after the layers were computed, with tsgo idle. The harness now reads layers from `git diff` (changed files only), which is how the overlay tracker will work. The layers are the same (6 files plus 1 deletion per agent). Baseline runs compute no layers, so only the shared whole-project runs were repeated. The first runs are kept as `*-shared-5-walklayer*`; they measured 26% (cal.com) and 18% (excalidraw).

## Follow-up: per-file agent workload

This is the measurement the owner asked for after Gate 0.

**Protocol:**
- Same fixtures and harness. `AGENTS=8 bench/setup.sh` added worktrees 6–8, each with its own scripted diff.
- Each agent requests syntactic and semantic diagnostics for the 6 files its diff added or modified, one file at a time; agents run concurrently. Then the same 20 hovers as before.
- Readings: peak, and warm 10 s after the last query. No idle readings, since idle 60 s matched warm in every Phase 0 run.
- LSP mode: one `tsc --lsp --stdio` per worktree. It opens the edited files and calls `textDocument/diagnostic` on each, so this comparison is like-for-like.

| Fixture | Agents | Baseline peak | Shared peak | **S peak** | S warm | LSP peak | S peak vs LSP |
| --- | --- | --- | --- | --- | --- | --- | --- |
| calcom | 1 | 1.10 GiB | 1.40 GiB | −28% | −28% | 1.15 GiB | −21% |
| calcom | 3 | 3.24 GiB | 1.90 GiB | **42%** | 42% | 3.15 GiB | 40% |
| calcom | 5 | 5.22 GiB | 2.05 GiB | **61%** | 61% | 5.36 GiB | 62% |
| calcom | 8 | 8.38 GiB | 2.21 GiB | **74%** | 74% | 8.74 GiB | 75% |
| excalidraw | 1 | 0.31 GiB | 0.36 GiB | −16% | −16% | 0.36 GiB | 0% |
| excalidraw | 3 | 0.86 GiB | 0.43 GiB | **49%** | 49% | 0.87 GiB | 50% |
| excalidraw | 5 | 1.41 GiB | 0.50 GiB | **65%** | 65% | 1.43 GiB | 65% |
| excalidraw | 8 | 2.27 GiB | 0.57 GiB | **75%** | 75% | 2.29 GiB | 75% |

- **Cost of each extra agent (5 → 8):**
  - Shared: about 55 MiB on cal.com and 24 MiB on excalidraw.
  - Separate processes: about 1.05 GiB on cal.com and 290 MiB on excalidraw.
  - Separate processes cost about the same as one `tsc --lsp` server per agent, which is the setup agents use today.
- **Fixed cost:** one shared process holds the base program plus each agent's program, so a single agent costs 0.31 GiB more on cal.com and 0.05 GiB more on excalidraw. Sharing should break even at about 2 agents; that point was not measured.
- **PLAN's success metric** (5 agents use at most 50% of 5 separate tsgo processes' memory): the per-file workload meets it (cal.com 39%, excalidraw 35%). The whole-project protocol does not (shared uses 70% of separate processes' memory on cal.com and 78% on excalidraw).
- **Correctness:** 0 mismatches at 1, 3, 5 and 8 agents on both fixtures (diagnostics on the edited files plus 20 hovers per agent).
- **Timing:**
  - Per-file diagnostics for one agent took 0.02–0.3 s with 1 agent.
  - At 8 agents on cal.com: 1.0 s shared vs 0.56 s for separate processes, because one process serves all 8.
  - Hovers took about 0.1 s throughout.

## Why the whole-project saving is small

- tsgo shares parsed and bound files across snapshots, as expected. It does not share checker state, and whole-project diagnostics build a full checker per snapshot.
- Marginal cost of one more agent, from the scaling runs (one process, every snapshot running whole-project diagnostics): excalidraw ~516 MB per snapshot vs ~738 MB per dedicated process (30% saved); cal.com ~1.29 GB vs ~2.10 GB (38% saved). That is the ceiling on peak saving as N grows. On these fixtures, checker state is ~60–70% of a dedicated process's peak.
- Gate 0's protocol is the worst case for sharing: all 5 agents run whole-project diagnostics at the same moment.
- At rest, after tsgo's idle checker cleanup and Go's periodic GC and scavenger (about 6 min in), memory drops to the shareable part: **63–68% saved**. That reading is not part of Gate 0.

## Correctness and features (tasks 0.5, 0.6)

Layered snapshot (base paths, translated back) vs a dedicated snapshot at the worktree's own paths, agent 1, 5 positions:

| Call | excalidraw | calcom | How |
| --- | --- | --- | --- |
| Diagnostics | 5/5 match | 5/5 match | `program.getSemanticDiagnostics(file)` |
| Hover | 5/5 match | 5/5 match | `checker.getSymbolAtPosition` → `typeToString(getTypeOfSymbol)` |
| Definition | 5/5 match | 5/5 match | `symbol.declarations` → `NodeHandle.resolve` |
| References | 5/5 match | 5/5 match | `languageService.getReferencedSymbolsForNode` (needs a real AST node) |
| Document symbols | 5/5 match | 5/5 match | walk the remote AST |
| Completions | 1/5 work | 5/5 work, 4/5 match | "completion list needs auto imports" on excalidraw even with `prepareAutoImports` |
| Rename | not exposed | not exposed | no API call in 7.1 |

Scaling in one process: no errors up to 32 snapshots (excalidraw, 16.6 GB) and 16 snapshots (cal.com, stopped at the 14 GB guard with 17.3 GB). Snapshot creation took about 35 ms (excalidraw) to 170–200 ms (cal.com) per snapshot. Whole-project diagnostics time grows linearly with snapshot count (CPU-bound), then superlinearly once the machine is under memory pressure. These runs used the old layer harness: its overhead is a constant offset in the Node process, so the per-snapshot costs above hold, but the absolute footprints include it.

Timing at 5 agents: concurrent whole-project diagnostics took 2.2 s with separate processes vs 6.3 s shared (excalidraw), and 10.6 s vs 17.9 s (cal.com).

## Caveats

- **One run per configuration.** Go GC timing moves peaks by a few hundred MB between runs: the excalidraw whole-project shared peak was 2.75 GB in a smoke run and 2.90–3.03 GB in protocol runs. Excalidraw's 21% is within that noise of the 20% line.
- **The per-file workload is one edit-and-check cycle with 6 files per agent.** Each edit creates a new snapshot, so the checker starts fresh, but a long run of checks without an edit grows a snapshot's checker toward the whole-project cost. An agent that runs a whole-project check pays about 0.5 GB (excalidraw) to 1.3 GB (cal.com) in the shared process too. Eight concurrent whole-project checks fall back to the Gate 0 picture.
- **cal.com has 730 type errors** under tsgo 7.1 after minimal migration. tRPC's router type partly collapses, so cal.com's checker work is somewhat understated. Parity is unaffected.
- **Layers come from git.** Ignored paths such as node_modules and generated clients are not in the layer, and the fixture diffs never change them. The overlay tracker needs PLAN's `extra_paths` for generated files that agents rebuild.
- **The first cal.com shared run carried 22 MB of junk per layer** from a seeding bug (see NOTES.md). It is kept as `calcom-shared-5-junklayer*`.
- **The whole-project LSP numbers are not like-for-like.** That comparison (5 × `tsc --lsp`, 10 open files each: 1.06 GiB and 5.64 GiB) checks only the open files. The per-file LSP comparison is like-for-like.

## Options for the owner

1. **Pivot** (PLAN Risks): build a per-agent memory governor with budgets, eviction and process ownership, without the shared checker.
2. **Go to Phase 1 with the claim the data supports:** "about 60% less type-checker memory at 5 agents when agents check the files they edit (75% at 8), about 20–30% when all of them run whole-project checks at once." Add governor features that only a single process makes possible: limit concurrent whole-project checks across agents, and dispose idle snapshots' checkers. This means accepting the per-file workload as the go criterion in place of Gate 0's whole-project protocol.
3. **Measure a mixed workload first** (about 1 hour): several edit-and-check cycles per agent, plus one agent at a time running a whole-project check, at 5 and 8 agents. This would show what the Option 2 governor buys before it is built.

## Data

- `results/phase0.csv`: every run summary in MiB, with a `workload` column (`whole` = Gate 0 protocol, `files` = per-file follow-up).
- `results/raw/<fixture>-<mode>-<agents>[-files].json`: summary plus per-agent diagnostics and hovers. `*-mem.csv`: 250 ms memory samples. `*-parity*.json`, `*-features.json`. `results/agents.log`: the follow-up run log.
- Rerun:
  - Fixtures: `AGENTS=8 bench/setup.sh`
  - Gate 0: `bench/phase0.sh`
  - Per-file follow-up: `bench/agents.sh`
  - Features and scaling: `node bench/features.mjs <fixture>`
  - Tables: `node bench/report.mjs`
