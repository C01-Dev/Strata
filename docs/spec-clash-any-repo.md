# Clash checks for any repo: the copy check

2026-11-09 · Phase 6 built on 2026-11-10 (NOTES, "Phase 6"); Phases 7 to 9 are a proposal · follows `PLAN.md` (Phases 0 to 5) and `NOTES.md`

## Summary and verdict

**Verdict: build it, in four phases, starting with the part that needs no checker at all.**

Today Strata finds clashes only in repos on TypeScript 7.1, and only between agents whose changes sit as layers on the shared checker. This spec adds a second way to check a pair that works in any git repo:

1. Take each agent's files as a git tree, without touching its worktree or the repo.
2. Merge two trees with `git merge-tree`. Files git cannot merge are reported at once.
3. Put the merged tree in one scratch worktree that has the repo's dependencies.
4. Run the repo's own check command there (`tsc`, `cargo check`, `pyright`, anything), and optionally its tests.
5. Report what fails only with both agents' changes.

Names used below: the **snapshot check** is today's (one shared `tsgo`, layers). The **copy check** is the new one.

A probe on the excalidraw fixture (section "Probe") ran this by hand on five real agent worktrees:

| Step | excalidraw | cal.com |
| --- | --- | --- |
| Agent's files as a tree, first time | 0.62 to 0.70 s | 2.13 s |
| Same, later rounds | 30 to 41 ms | 0.13 to 0.15 s |
| Merge two agents' trees | 9 to 34 ms | not measured |
| Switch the scratch copy to another state | 19 to 53 ms | not measured |
| Full check, TypeScript 5.9.3 | 9.6 to 10.8 s, 0.9 to 1.0 GB | not measured |
| Full check, TypeScript 7.1 | 1.9 to 2.5 s, 1.3 to 1.5 GB | 8.8 s, 3.0 GB (NOTES) |

- It found the same clash Strata's snapshot check found in run 2 (`actionBoundText.tsx:160:26` and `168:32`, TS2552, clear + rename), with TypeScript 5.9.3 and with 7.1, and nothing in the other 9 pairs.
- Nothing was written into the fixture repo.
- Everything except the check itself costs well under a second. The check is the cost, so the design is about running it as rarely as possible.

## What today's check cannot see

| Gap | Why | Where |
| --- | --- | --- |
| Any repo without TypeScript 7.1 gets no clash warning at all, not even for files git cannot merge | The whole clash path sits behind the checker group, which refuses when the checker is off. The daemon's loop drops that refusal | `broker.rs:243`, `ops.rs:1137` |
| A pair is skipped when either agent's dependencies differ, its diff is huge, or its TypeScript differs | Those agents are not layered on the base | `clash.rs:208` |
| An agent that edits any `package.json` falls into the row above | Every `package.json` is part of `dep_key` | `strata-overlay` `dep_key` |
| Two agents editing the same lines of a `.css`, `.prisma`, `.sql`, `.yml` or `.md` file are not reported | The overlay only walks `ts tsx mts cts js jsx mjs cjs json` | `strata-overlay` `EXTENSIONS` |
| An error that lands in a file neither agent changed | Only changed files are checked | `clash.rs`, header comment |
| A test that passes on each branch and fails once both merge | Types only | NOTES, "Limits" |
| A rename by one agent and an edit of the old path by the other is reported as unmergeable | Files are merged one by one with `git merge-file`, which has no rename detection | `clash.rs` `merge` |

The copy check closes all seven. The first, fourth and seventh need only steps 1 and 2, which is why Phase 6 ships them alone.

## How it works

### 1. Each agent's files as a git tree

```bash
export GIT_INDEX_FILE=$STRATA_HOME/clash/<repo key>/<agent>.index
export GIT_OBJECT_DIRECTORY=$STRATA_HOME/clash/<repo key>/objects
export GIT_ALTERNATE_OBJECT_DIRECTORIES=<repo>/.git/objects
git -C <worktree> read-tree HEAD                          # first time only
git -C <worktree> add -A
git -C <worktree> write-tree                              # the tree id
git -C <worktree> commit-tree <tree> -p HEAD -m strata    # fixed author and date: the same commit each time
```

`<repo key>` is a hash of the repo's path.

- **The agent's own index is never touched**, so its `git status`, staging and commits are unaffected. `bench/agent-usage.sh` already records worktrees this way.
- **Nothing is written into the repo.** New objects go to Strata's own object folder, and the repo's objects are read through the alternates variable. Measured: the repo's object count was the same before and after. The folder must exist first: with a missing `GIT_OBJECT_DIRECTORY` git answers "not a git repository".
- **What is in the tree:** every tracked and untracked file that is not ignored, whether the agent committed it, staged it or left it unstaged. Seeded folders and the worktree note are ignored, so they stay out.
- **The tree id is the change key.** An agent whose tree equals the base commit's tree has no changes and is in no pair. This replaces the layer hash for the copy check.
- **The parent is the worktree's `HEAD`**, so git finds the real common ancestor of two agents, also when one branch started from an older commit or was rebased.

### 2. Merge two agents

```bash
git -C <repo> merge-tree --write-tree --name-only --no-messages --allow-unrelated-histories -z <commit a> <commit b>
```

- Exit 0: the first line is the merged tree's id.
- Exit 1: the lines after it are the files git cannot merge. The pair reports those and is not checked further, as today.
- It is a real three-way merge with rename detection, and it covers every tracked file type.
- Needs git 2.38 (Strata needs 2.36 today). Below that, `strata doctor` says so and the copy check is off.

### 3. One scratch worktree per repo

- `<worktree_dir>/.check`, made like a warm pool entry: `git worktree add --detach --lock --reason "strata check"`, then seeded by the existing `seed()` (clone from the donor). It is never a donor and never an agent.
- To show a state: `git -C .check read-tree -u --reset <tree>`, then `git clean -fdq` (untracked files only, so ignored folders stay). Both run with the object variables above.
- Only files that differ between two states are rewritten. The rest keep their modification times, so incremental tools (`cargo`, `tsc --incremental`, test caches) stay warm between checks.
- Its `HEAD` stays at the base commit, so inside it the merged changes look like uncommitted work. Test runners that pick tests from changed files can use that (Phase 8).

### 4. Run the command

- `sh -c "<command>"` in `.check`, through `strata_proc::spawn`: its own process group, output in `$STRATA_HOME/logs/.check/`.
- Environment: `CI=1`, `STRATA_CHECK=1`, `STRATA_CHANGED=<file listing the paths either agent changed, one per line>`. No `PORT`.
- The command comes from `strata.toml` in the **main checkout**, which is where the daemon already reads config. An agent editing `strata.toml` in its own worktree does not change what the daemon runs.
- A run that passes `clash.timeout_secs` is stopped with its whole process tree.

### 5. Verdict

Output lines are read with two patterns. A match that names a file of the tree is a diagnostic:
- `file(line,col): error TS1234: message` (the `tsc` CLI, as in `bench/clash-truth.mjs`);
- `file:line[:col]: message` or `file:line:col - message` (gcc, go, mypy, pyright, ruff, eslint's unix format, `cargo check --message-format short`).

| Merged run | The two agents alone | Verdict |
| --- | --- | --- |
| exits 0 | not run | clean |
| fails, with diagnostics | each run once, kept per tree id | diagnostics only the merge has are clashes (today's rule: matched by file, code and message, counting repeats); none means clean |
| fails, no diagnostics | both exit 0 | clash: the last 20 lines of the merged output |
| fails, no diagnostics | one fails | not checked: "a's own check fails" |
| timed out | | not checked: "timed out after 300 s" |

- **The alone runs are skipped whenever the merged run passes.** In the probe that was 9 of 10 pairs.
- Diagnostics become the existing `Clash` records and go through the existing `only_together` and `grouped`, so every place that shows clashes keeps working.
- The command is run once on the untouched base at first use. If it fails there with no diagnostics, the copy check stays off for that repo and `strata status` says "clash.command fails on the base itself". With diagnostics (cal.com's 730 errors) it stays on: they are in every run and cancel out.

## Which check answers which pair

| Pair | Files git cannot merge | Type or build errors | Tests |
| --- | --- | --- | --- |
| TypeScript 7.1, both agents layered on the base | tree merge | snapshot check, as today | copy check, if `clash.test` is set |
| TypeScript 7.1, pair skipped today (dependencies differ, huge diff, other TypeScript) | tree merge | copy check | copy check |
| TypeScript below 7.1, or another language | tree merge | copy check | copy check |

The snapshot check stays the fast lane: 0.05 to 0.7 s per pair against 2 to 10 s. The copy check is what makes the answer exist everywhere else.

## When it runs, and what it costs

- **Trees every round, with back-off.** Each 5 s round takes the tree of every agent that changed in the last round. An agent that did not change is looked at every sixth round (30 s) until it does. `strata_conflicts` looks at everyone at once.
  - Without back-off, 18 idle agents would cost about 7.5 s of CPU a minute on excalidraw (35 ms, 12 times a minute), against 1.94 s today. With it, about 1.3 s.
  - `ponytail:` polling with back-off; a file watcher replaces it if the CPU shows, the same upgrade path as the overlay's stat walk.
- **A pair is checked once both trees held still for a round**, and again only when a tree id, the base commit or the command changes. Same rule as today.
- **One command at a time for the whole daemon.** A `tsc` holds 0.9 to 3 GB, so two never run together. When a tree changes while its pair is running, the run is stopped.
- **Order:** the pairs of an agent that called `strata_conflicts`, then pairs never checked, then re-checks.
- **`strata_conflicts` waits at most 60 s.** After that it answers with what it has and names the rest: "Still checking against b. Call again in a minute." The existing "Not checked yet" line is the model.
- **Expected warning time** on excalidraw with TypeScript 5.9.3: one round to settle, then about 10 s per pair. Four agents (6 pairs), one clash: about 60 s for all pairs, about 15 s for the first.
- **Memory:** nothing is held between runs. The peak is one command.

## Config

```toml
[clash]
command = "auto"     # auto | off | a shell command run in the merged copy; exit 0 means clean
test = ""            # optional: a shell command run after `command` passes
timeout_secs = 300   # per run
```

- `auto`: with TypeScript in `node_modules`, `tsc -p <tsconfig> --noEmit --incremental false --pretty false` for each `checker.tsconfig`. Never incremental: NOTES found that a seeded `.tsbuildinfo` lets `tsc` answer from cache. Without TypeScript, `off`.
- `off`: files git cannot merge are still reported.
- `checker.conflicts = false` still turns every clash check off.
- Other languages set the command themselves, for example `cargo check --message-format short` or `pyright`. Presets are added when a user asks for one.

## What agents and people see

Diagnostics print exactly as today, with `TS` only when the tool gave a code. Two new lines:

```
Your changes and fix-login's fail the repo's check once combined (neither is merged yet):
  src/cart.rs:11:9: cannot find function `fetch_json` in this scope
Your changes and fix-login's fail these tests once combined; each passes them alone:
  FAIL src/cart.test.ts > total includes tax
```

- `status --json`: a pair gains `failed` (`[{ "command": "check" | "test", "output": [...] }]`) and `pending: true` while it waits or runs. Both are left out when empty, like `also`, so older clients read the same JSON.
- Repos with the checker off appear in the `checker` array with their `conflicts`, so `strata status`, the menu bar and the bench timeline need no new field to find them. The entry keeps every field the menu bar app decodes as required (`repo`, `typescript: ""`, `memory_bytes: 0`, `agents: []`), with `"pid": null`, so apps already installed still read the status.
- A diagnostic from a tool without error codes has `code: 0`. The three printers (MCP text, `strata status`, the menu bar) print `TS<code>` only when it is not 0; a menu bar app older than this change shows `TS0`.
- `strata status` gets one line while a command runs: `copy check: a + b, 12 s`.

## Edge cases

Each row needs a test. The rule from `PLAN.md` holds: never lose work silently, and say so when a pair is not checked.

| Case | Expected behavior |
| --- | --- |
| git older than 2.38 | Tree merges and the copy check are off; `doctor` and `status` say why |
| Agent commits, amends, rebases or switches branch | The tree is of its files, so the answer follows; the parent follows `HEAD` |
| Agent's branch started from an older commit than the base | Merged through the real common ancestor; no false "unmergeable" from the base's newer files |
| Both edit the same lines of a non-code file | Reported as unmergeable |
| One renames a file, the other edits it | Merged by git; no longer reported as unmergeable |
| Agent's own check fails mid-work, tool gives diagnostics | Its own errors cancel out; only new ones are clashes |
| Agent's own check fails, no diagnostics | Pair not checked, with the reason |
| The command fails on the untouched base | See "Verdict": off with a message, unless it gives diagnostics |
| The command hangs | Stopped after `timeout_secs` with its process tree; pair not checked |
| A tree changes during a run | Run stopped; the pair waits to settle again |
| A test fails at random | A failing test run is repeated once; reported only if it fails twice |
| One agent changed dependencies | Phase 9: the scratch copy is seeded from that agent's worktree for the pair. Until then: not checked, with the reason |
| Both agents changed dependencies | Not checked: "both changed dependencies" |
| Ignored generated files that differ per worktree (a Prisma client) | The scratch copy has the base's. Listed under limits; `checker.extra_paths` is the later fix |
| Tests that bind a port or write to a shared database | `clash.test` is off by default; `STRATA_CHECK=1` lets a test setup pick another target |
| Large untracked files in a worktree | They land in Strata's object folder. It is deleted when the repo has no agents, and `strata gc` empties it |
| `.check` deleted by hand, or left by a crash | Recreated on the next run; `gc` and recovery treat it like a pool entry |
| Disk full while seeding `.check` | As seeding does today: stop, report free space, pair not checked |
| Daemon restart | Indexes stay on disk; results are in memory and are checked again |
| `strata rm` | The agent's index is removed; a run that includes it is stopped |
| Submodules | Recorded as links, not checked out in `.check`; a command that needs them fails alone and merged alike, so the pair is not checked |

## Build plan

Same rules as `PLAN.md`: one task at a time, tests first, a gate with measured numbers at the end of each phase, deviations in `NOTES.md`. Sizes are rough.

### Phase 6: trees and merges, for every repo (built 2026-11-10)

1. **6.1 `strata-git`.** `snapshot` (private index and object folder), `commit_tree`, `merge_trees` returning the tree or the unmergeable files. Tests on temporary repos: an untouched worktree gives the base's tree; staged, unstaged, untracked, deleted and committed changes are all in the tree; ignored files are not; the repo's object count does not change.
2. **6.2 Rounds.** Tree ids with back-off. `unmerged` comes from the tree merge for every pair in every repo. The list of pairs moves from the checker group to the broker, per repo, so a repo with the checker off has one.
3. **6.3 `doctor` and `gc`.** The git 2.38 check; indexes of removed agents and the object folder.

**Gate 6.** On the excalidraw fixture with its 18 agents: the same 9 unmergeable pairs as the snapshot check reported, or each difference explained. A planted clash in a `.css` file is found. With TypeScript 5.9.3 in the repo (checker off) the list is the same. Idle daemon CPU stays under 4 s a minute. The repo's `.git` is unchanged.

**Result (2026-11-10): passed.** 26 agents, 231 pairs. The old and new builds name the same 30 unmergeable pairs and the same 20 type clashes. On TypeScript 5.9.3 the same 30 pairs are named, where there was no warning before. A planted `.scss` conflict showed after 6 s. The repo's `.git` did not change. The daemon's own idle CPU is 2.34 s a minute, as before; its git processes add 1.16 s a minute to the 4.14 s the old build already spent. Details and what was not covered: NOTES, "Gate 6".

### Phase 7: the copy check (about 2 days)

1. **7.1 Scratch worktree.** `.check`, on the warm pool's pattern.
2. **7.2 Runner.** One at a time, timeout, stop on change, logs.
3. **7.3 Verdict.** The two patterns, the table above, the base run.
4. **7.4 Scheduling.** Which pairs, in which order; `pending`; the 60 s wait in `strata_conflicts`.
5. **7.5 Text.** MCP answers, `strata status`, the menu bar, the worktree note and the skill (the note's `strata_conflicts` line now also appears when only the copy check is on).
6. **7.6 `auto`, the `strata init` template, README.**

**Gate 7.**
- excalidraw on TypeScript 5.9.3: all 8 planted clashes, no false one, and the same verdicts as `bench/clash-truth.mjs` on the kept product runs.
- Four agents, one clash: first warning within 30 s of the trees settling, all pairs within 90 s.
- A Rust repo (Strata itself, `cargo check --message-format short`): a planted rename against a new caller is found.
- On a TypeScript 7.1 repo, `strata_diagnostics` is no more than 10% slower while a command runs.

### Phase 8: tests (about 1 day)

1. **8.1 `clash.test`.** Runs after `clash.command` passes on the merge. Check that `vitest --changed` and `jest -o` pick the merged changes in `.check`; if not, the recipe uses `$STRATA_CHANGED`.
2. **8.2 Rule.** Passes for each agent alone and fails merged, twice.
3. **8.3 Text.**

**Gate 8.** A planted behaviour clash on excalidraw (types clean, one vitest test fails only merged) is found. A test that fails every second run is not reported. The `tests` column of `clash-truth.mjs` agrees on the product runs.

### Phase 9: cost and coverage (1 to 2 days, only what Gate 7's numbers call for)

1. **9.1 All together first.** When three or more settled agents merge cleanly, merge them all and run once. A pass marks every pair among them clean. In the probe, five agents took one 1.9 s run to learn that something fails, against ten runs pair by pair; with no clash that one run is the whole round.
   - `ponytail:` a pass with everyone can hide a pair that only fails without a third agent's change. Pair by pair stays the fallback, and is what runs after a failure.
2. **9.2 Dependencies.** One agent changed them: seed `.check` from that agent for its pairs, and back after.
3. **9.3 The snapshot check's leftovers.** Take merged file text from the merged tree instead of `git merge-file`, and drop the round's overlay scan once tree ids are the key.

**Gate 9.** Eight agents on TypeScript 5.9.3 with no clash: one run per settled round. An agent that added a dependency is checked against the others.

## Probe (2026-11-09)

Apple M5 Pro, macOS, APFS, git 2.52.0. Script kept outside the repo; it drives git by hand as in "How it works" and compares errors as `clash-truth.mjs` does. Worktrees: `note-related-2-clear`, `-rename`, `-upper`, `-opts` (the four real Opus agents of run 2) and `clash-oldname`.

| | TypeScript 5.9.3 | TypeScript 7.1.0-beta |
| --- | --- | --- |
| Each agent alone | 5 runs, all exit 0 | 5 runs, all exit 0 |
| Check time | 9.6 to 10.8 s | 1.9 to 2.5 s |
| Peak memory per run | 863 to 1,032 MB | 1,331 to 1,505 MB |
| clear + rename | exit 2: `actionBoundText.tsx:160:26` and `168:32`, TS2552 | the same |
| The other 9 pairs | exit 0 | exit 0 |
| All five merged at once | not run | 1.9 s, exit 2, the same 2 errors |

- Merges took 9 to 34 ms, switching the scratch copy 19 to 53 ms (0.8 s the first time after seeding).
- Trees: 0.62 to 0.70 s the first time and 30 to 41 ms after, on 1,328 tracked files. On cal.com's 7,711: 2.13 s, then 0.13 to 0.15 s, and an untouched worktree gave exactly the base's tree.
- Seeding the scratch copy took 10.7 s with `cp -c -R` on seven `node_modules` folders. Strata's own `clonefile` path was not used here.
- **Not measured:** cal.com checks on TypeScript below 7.1, Linux, any language but TypeScript, tests, and the daemon doing this in rounds.

## Not in this spec

- Pushing warnings to agents through hooks, a symbol index to warn before code breaks, landing branches in order, and a team server. Each is its own spec.
- A full-project copy check after the snapshot check says clean, to catch errors in files neither agent changed on TypeScript 7.1 repos. Cheap to add once Phase 7 exists; wait for a miss that needs it.
- Seeding for repos without a `package.json`. A persistent `.check` keeps `cargo`'s and similar caches warm without it.
- Windows.

## Decisions for the owner

1. **On by default?** `clash.command = "auto"` runs `tsc` in the background on repos below TypeScript 7.1: up to about 1 GB on excalidraw, more on larger repos, one at a time. Recommended: on, since today those repos get silence.
2. **git 2.38 as the minimum** for clash checks (2.36 stays enough for the rest).
3. **Tree merges in TypeScript 7.1 repos too** (Phase 6.2), which adds 30 to 150 ms per changed agent per round and buys non-code files and renames. Recommended: yes, one source for "git cannot merge".
4. **The table name.** `[clash]` next to `checker.conflicts` is two words for one thing. Either keep both, or move the switch to `clash.enabled` and keep reading the old key.
5. **All together first** (9.1): faster in the common case, with the stated blind spot.
