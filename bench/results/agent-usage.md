# Do agents use Strata on their own? (2026-11-09)

Raw logs stay local (`bench/results/agent-usage/`, ignored). Rerun: `bench/agent-usage.sh`. Tally: `node bench/agent-usage.mjs <logs>`.

## asis
```
## desc  (claude-opus-5-5, strata connected, 56 tool calls, 5 turns, 263s)
  Strata tools: strata_diagnostics x2
  Own type checks: 0
  Tests/lint/build: 10
    $ yarn vitest run packages/excalidraw/tests/data/restore.test.ts packages/element/tests/newElement.test.ts packages/element/tests/duplicate.te
    $ yarn vitest run packages/excalidraw/tests/data/restore.test.ts packages/element/tests/newElement.test.ts packages/element/tests/duplicate.te
    $ yarn vitest run packages/excalidraw/tests/data/restore.test.ts packages/element/tests/newElement.test.ts packages/element/tests/duplicate.te
    $ yarn vitest run 2>&1 | grep -E "FAIL|Test Files|Tests |Snapshots" | head -30
    $ yarn vitest run 2>&1 > /private/tmp/claude-501/-Users-seifelkady-Documents-Projects-Strata-bench-fixtures-excalidraw--strata-base-asis-desc/
    $ git checkout packages/excalidraw/tests/scene/__snapshots__/export.test.ts.snap && CI=1 yarn vitest run --maxWorkers=4 --minWorkers=1 2>&1 | 
    $ yarn vitest run --maxWorkers=4 --minWorkers=1 2>&1 | grep -E "^ FAIL|Test Files|Tests |Snapshots " | head -30
    $ yarn vitest run excalidraw-app/tests/MobileMenu.test.tsx packages/element/tests/resize.test.tsx packages/element/tests/zindex.test.tsx packa
    $ yarn vitest run packages/excalidraw/tests/renderOverrides.test.tsx packages/excalidraw/wysiwyg/textWysiwyg.test.tsx -t "complete snapshot|no
    $ yarn eslint packages/element/src/newElement.ts packages/element/src/types.ts packages/excalidraw/data/restore.ts packages/element/tests/dupl
## opts  (claude-opus-5-5, strata connected, 16 tool calls, 17 turns, 54s)
  Strata tools: strata_references x1, strata_diagnostics x2
  Own type checks: 0
  Tests/lint/build: 1
    $ npx prettier --write packages/element/src/mutateElement.ts packages/element/src/delta.ts packages/excalidraw/actions/actionProperties.tsx &&
## rename  (claude-opus-5-5, strata connected, 42 tool calls, 43 turns, 73s)
  Strata tools: strata_diagnostics x1
  Own type checks: 0
  Tests/lint/build: 0
Sessions: 3. Used a Strata tool: 3. Ran their own type check: 0.
```

## asis-sonnet
```
## desc  (claude-sonnet-5-5, strata connected, 32 tool calls, 33 turns, 481s)
  Strata tools: none
  Own type checks: 2
    $ yarn test:typecheck 2>&1 | tail -30
    $ yarn fix 2>&1 | tail -8; yarn test:typecheck 2>&1 | tail -8; yarn test:update --watch=false 2>&1 | tail -40
  Tests/lint/build: 5
    $ yarn test:update 2>&1 | tail -40; git status --short; npx prettier --check packages/element/src packages/excalidraw/data/restore.ts packages
    $ git diff packages/excalidraw/tests/__snapshots__/contextmenu.test.tsx.snap | head -30; git checkout packages/excalidraw/tests/__snapshots__/
    $ git checkout packages/excalidraw/tests/__snapshots__/contextmenu.test.tsx.snap packages/excalidraw/tests/scene/__snapshots__/export.test.ts.
    $ npx prettier --write packages/element/src/__tests__/description.test.ts packages/excalidraw/data/restore.ts packages/excalidraw/tests/data/r
    $ npx vitest run --watch=false --testTimeout=60000 packages/excalidraw/tests/data packages/element/src/__tests__ packages/excalidraw/tests/onD
## opts  (claude-sonnet-5-5, strata connected, 12 tool calls, 13 turns, 76s)
  Strata tools: strata_diagnostics x2
  Own type checks: 1
    $ yarn -s test:typecheck 2>&1 | tail -15
  Tests/lint/build: 0
## rename  (claude-sonnet-5-5, strata connected, 41 tool calls, 42 turns, 75s)
  Strata tools: strata_diagnostics x1
  Own type checks: 0
  Tests/lint/build: 0
Sessions: 3. Used a Strata tool: 2. Ran their own type check: 2.
```

## hint-sonnet
```
## desc  (claude-sonnet-5-5, strata connected, 22 tool calls, 23 turns, 163s)
  Strata tools: strata_diagnostics x1
  Own type checks: 0
  Tests/lint/build: 3
    $ yarn vitest run packages/excalidraw/tests/description.test.ts 2>&1 | tail -30
    $ yarn vitest run 2>&1 | tail -40; yarn prettier --check packages/excalidraw/tests/description.test.ts packages/excalidraw/data/restore.ts pac
    $ yarn prettier --write packages/excalidraw/tests/description.test.ts 2>&1 | tail -2; git status --short
## opts  (claude-sonnet-5-5, strata connected, 11 tool calls, 12 turns, 29s)
  Strata tools: strata_diagnostics x2
  Own type checks: 0
  Tests/lint/build: 0
## rename  (claude-sonnet-5-5, strata connected, 45 tool calls, 46 turns, 70s)
  Strata tools: strata_diagnostics x2
  Own type checks: 0
  Tests/lint/build: 0
Sessions: 3. Used a Strata tool: 3. Ran their own type check: 0.
```

## Own tsc memory (sampled every 2 s during asis and asis-sonnet; Opus ran none)
```
01:29:56 own_tsc=1 own_rss_mb=313 shared_mb=4
01:29:58 own_tsc=1 own_rss_mb=483 shared_mb=4
01:30:01 own_tsc=1 own_rss_mb=644 shared_mb=190
01:30:03 own_tsc=1 own_rss_mb=910 shared_mb=419
01:30:05 own_tsc=1 own_rss_mb=1180 shared_mb=500
01:30:08 own_tsc=1 own_rss_mb=1438 shared_mb=555
01:30:10 own_tsc=1 own_rss_mb=1441 shared_mb=551
01:31:03 own_tsc=1 own_rss_mb=19 shared_mb=757
01:31:05 own_tsc=1 own_rss_mb=646 shared_mb=449
01:31:08 own_tsc=1 own_rss_mb=685 shared_mb=15
01:31:11 own_tsc=1 own_rss_mb=930 shared_mb=14
01:31:14 own_tsc=1 own_rss_mb=710 shared_mb=14
01:31:17 own_tsc=1 own_rss_mb=1341 shared_mb=15
01:31:19 own_tsc=1 own_rss_mb=1137 shared_mb=15
01:37:56 own_tsc=1 own_rss_mb=450 shared_mb=267
01:37:58 own_tsc=1 own_rss_mb=1095 shared_mb=6
01:38:01 own_tsc=1 own_rss_mb=196 shared_mb=6
01:38:03 own_tsc=1 own_rss_mb=782 shared_mb=6
01:38:06 own_tsc=1 own_rss_mb=1345 shared_mb=6
01:38:08 own_tsc=1 own_rss_mb=531 shared_mb=6
01:38:11 own_tsc=1 own_rss_mb=1274 shared_mb=6
01:38:13 own_tsc=1 own_rss_mb=453 shared_mb=6
01:38:16 own_tsc=1 own_rss_mb=1038 shared_mb=6
01:38:21 own_tsc=1 own_rss_mb=593 shared_mb=6
01:38:23 own_tsc=1 own_rss_mb=1360 shared_mb=6
peak own tsc processes at once: 1, peak own tsc memory: 1441 MB, peak shared checker: 1470 MB
```
