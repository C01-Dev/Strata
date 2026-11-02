# Phase 0 report: one tsgo process vs five

2026-11-02 · Apple M5 Pro, 24 GB, macOS (Darwin 27.0.0), APFS · TypeScript `7.1.0-beta` · Go GC defaults on both sides

## Gate 0: not passed

S = 1 − shared ÷ baseline, at 5 worktrees, memory = `phys_footprint` summed over the whole process tree (Node client included on both sides).

| Fixture | Baseline peak | Shared peak | **S peak** | Baseline warm | Shared warm | **S warm** | Baseline idle 15 min | Shared idle 15 min | S idle 15 min |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| calcom (`apps/web`, 10,445 files) | 10.24 GiB | 7.62 GiB | **26%** | 10.24 GiB | 7.59 GiB | **26%** | 5.57 GiB | 2.03 GiB | 64% |
| excalidraw (1,519 files) | 3.61 GiB | 2.96 GiB | **18%** | 3.60 GiB | 2.94 GiB | **18%** | 1.61 GiB | 0.69 GiB | 57% |

| Gate 0 condition | Result |
| --- | --- |
| S ≥ 40% on peak and warm | **No.** calcom 26%/26% is in the 20–40% band (owner decides); excalidraw 18%/18% is under 20% (PLAN: stop and see Risks) |
| No diagnostic mismatch after path translation | **Yes.** 0 mismatches on both fixtures, all 5 agents, diagnostics and 20 hovers each; every agent's diagnostics differ from the others |
| Per-snapshot diagnostics available | **Yes** |

## Why the peak saving is small

- tsgo shares parsed and bound files across snapshots, as expected. It does not share checker state, and whole-project diagnostics build a full checker per snapshot.
- Marginal cost of one more agent, from the scaling runs (one process, every snapshot running whole-project diagnostics): excalidraw ~516 MB per snapshot vs ~738 MB per dedicated process (30% saved); calcom ~1.29 GB vs ~2.10 GB (38% saved). That is the ceiling on peak saving as N grows. On these fixtures, checker state is ~60–70% of a dedicated process's peak.
- The protocol is the worst case for sharing: all 5 agents run whole-project diagnostics at the same moment.
- Once tsgo's idle checker cleanup and Go's periodic GC and scavenger run (about 6 min in), memory drops to the shareable part: **57–64% saved at rest**. That reading is not part of Gate 0.

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

Scaling in one process: no errors up to 32 snapshots (excalidraw, 16.6 GB) and 16 snapshots (calcom, stopped at the 14 GB guard with 17.3 GB). Snapshot creation took about 35 ms (excalidraw) to 170–200 ms (calcom) per snapshot. Whole-project diagnostics time grows linearly with snapshot count (CPU-bound), then superlinearly once the machine is under memory pressure.

Timing at 5 agents: concurrent whole-project diagnostics took 2.2 s baseline vs 6.8 s shared (excalidraw) and 10.6 s vs 18.0 s (calcom).

## Caveats

- One run per mode on one machine. Go GC timing moves peaks by a few hundred MB between runs: the excalidraw shared peak was 2.75 GB in a smoke run and 3.03 GB in the protocol run.
- calcom has 730 type errors under tsgo 7.1 after minimal migration. tRPC's router type partly collapses, so calcom's checker work is somewhat understated. Parity is unaffected.
- The first calcom shared run carried 22 MB of junk per layer from a seeding bug (see NOTES.md). It was fixed and rerun, and the biased run is kept as `calcom-shared-5-junklayer*`. It measured S peak 24% and S warm 30%.
- The LSP comparison (5 × `tsc --lsp`, 10 open files each: 1.06 GiB and 5.64 GiB) checks only open files, so it is not like-for-like with whole-project diagnostics.

## Options for the owner

1. **Pivot** (PLAN Risks): a per-agent memory governor with budgets, eviction and process ownership, without the shared checker.
2. **Continue with a narrower claim:** "~60% less type-checker memory at rest, ~20–25% at peak", plus governor features that only a single process makes possible: serialize or limit concurrent whole-project checks across agents, and dispose idle snapshots' checkers.
3. **Measure once more before deciding:** a realistic agent workload (per-file diagnostics on edited files instead of whole-project checks for all 5 at once), at 1, 3, 5 and 8 agents. About 1 hour.

## Data

- `results/phase0.csv`: every run summary (MB = MiB).
- `results/raw/<fixture>-<mode>-5.json`: summary plus per-agent diagnostics and hovers. `*-mem.csv`: 250 ms memory samples. `*-parity.json`, `*-features.json`.
- Rerun: `bench/setup.sh && bench/phase0.sh && node bench/features.mjs <fixture> && node bench/report.mjs`.
